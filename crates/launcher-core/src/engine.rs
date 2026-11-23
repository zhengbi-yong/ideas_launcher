//! The query engine.
//!
//! Reproduces the dispatch pipeline observed in Flow Launcher's
//! `MainViewModel`:
//!
//! ```text
//! ChangeQueryTextAsync
//!   └─ QueryAsync
//!        └─ QueryResultsAsync
//!             ├─ ConstructQueryAsync   → build the Query object
//!             ├─ BuildQueryAsync       → route to plugins by action keyword
//!             └─ UpdateResultView      → merge, sort, truncate
//! ```
//!
//! The important difference is that plugins run in parallel on a thread pool
//! and every result carries a score, so the merged list can be ordered
//! globally instead of per plugin.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock, RwLockReadGuard};

use crate::matcher::{SearchPrecision, StringMatcher};
use crate::history::{item_key, SelectionMemory};
use crate::plugin::{PluginContext, PluginError, QueryPlugin};
use crate::query::{ActionKeywordIndex, Query, QueryBuilder};
use crate::result::Result as LauncherResult;
use crate::result::{result_group, ResultOrder};

/// A registered plugin together with its routing information.
#[derive(Clone)]
struct Registered {
    plugin: Arc<dyn QueryPlugin>,
    keywords: Vec<String>,
}

/// The outcome of running a query.
#[derive(Debug, Clone, Default)]
pub struct QueryOutcome {
    /// The parsed query.
    pub query: Query,
    /// The merged, sorted and truncated results.
    pub results: Vec<LauncherResult>,
    /// How long the whole dispatch took.
    pub elapsed: Duration,
    /// Per-plugin result counts, for diagnostics.
    pub per_plugin: Vec<(String, usize)>,
}

/// The launcher engine: holds the plugins and runs queries against them.
///
/// The engine is cheap to clone and safe to share across threads. That is what
/// lets the UI hand a copy to a worker thread and answer queries off the main
/// thread — the same split Flow Launcher makes between `ChangeQueryTextAsync`
/// and `QueryAsync`. Running the query on the UI thread would freeze the
/// window for as long as the slowest plugin takes.
#[derive(Clone)]
pub struct Engine {
    /// The registered plugins.
    plugins: Arc<RwLock<Vec<Registered>>>,
    /// The action-keyword routing table.
    builder: Arc<RwLock<QueryBuilder>>,
    /// The fuzzy matcher.
    matcher: Arc<RwLock<StringMatcher>>,
    /// How many results are kept.
    max_results: Arc<AtomicUsize>,
    context: PluginContext,
    /// Set when a plugin asked for its data to be reloaded.
    reload_requested: Arc<Mutex<bool>>,
    /// What the user picked before, used to rank the results.
    picks: Arc<RwLock<Option<Arc<SelectionMemory>>>>,
    /// How the results of the different plugins are put in order.
    result_order: Arc<RwLock<ResultOrder>>,
}

impl Engine {
    /// Create an engine with the given plugin context.
    pub fn new(context: PluginContext) -> Self {
        Self {
            plugins: Arc::new(RwLock::new(Vec::new())),
            builder: Arc::new(RwLock::new(QueryBuilder::new())),
            matcher: Arc::new(RwLock::new(StringMatcher::new())),
            max_results: Arc::new(AtomicUsize::new(10)),
            context,
            reload_requested: Arc::new(Mutex::new(false)),
            picks: Arc::new(RwLock::new(None)),
            result_order: Arc::new(RwLock::new(ResultOrder::default())),
        }
    }

    /// Let the engine learn from what the user picks.
    ///
    /// A row the user picks often is moved up the list, so the thing they
    /// always want does not have to be picked from the third place every
    /// time.
    pub fn set_pick_memory(&self, memory: Arc<SelectionMemory>) {
        *self.picks.write() = Some(memory);
    }

    /// Register a plugin and wire up its action keywords.
    pub fn register(&self, plugin: Arc<dyn QueryPlugin>) {
        let keywords = plugin.action_keywords().to_vec();
        for keyword in &keywords {
            self.builder.write().index_mut().register(plugin.id(), keyword);
        }
        let _ = plugin.init(&self.context);
        self.plugins.write().push(Registered { plugin, keywords });
    }

    /// Number of registered plugins.
    pub fn plugin_count(&self) -> usize {
        self.plugins.read().len()
    }

    /// Set how many results are kept.
    pub fn set_max_results(&self, max_results: usize) {
        self.max_results.store(max_results.max(1), Ordering::SeqCst);
    }

    /// Set the search precision tier.
    pub fn set_precision(&self, precision: SearchPrecision) {
        self.matcher.write().set_precision(precision);
    }

    /// Set how the results of the different plugins are put in order.
    ///
    /// With [`ResultOrder::ProgramsFirst`] the programs the user can start
    /// are above the files; with [`ResultOrder::Mixed`] everything is in one
    /// list, ordered by the score alone.
    pub fn set_result_order(&self, order: ResultOrder) {
        *self.result_order.write() = order;
    }

    /// The order the results are currently put in.
    pub fn result_order(&self) -> ResultOrder {
        *self.result_order.read()
    }

    /// The action keyword index.
    pub fn action_keywords(&self) -> ActionKeywordIndex {
        self.builder.read().index().clone()
    }

    /// Register an action keyword for a plugin.
    ///
    /// The routing table lives behind a lock, so a plugin may be wired up
    /// after the engine has been shared with a worker thread.
    pub fn register_keyword(&self, plugin_id: &str, keyword: &str) {
        self.builder.write().index_mut().register(plugin_id, keyword);
    }

    /// Run a query.
    ///
    /// Every plugin that is routed to by the query's action keyword runs in
    /// parallel; results are merged, sorted by score and truncated.
    pub fn query(&self, raw: &str) -> QueryOutcome {
        self.query_with_sink(raw, Arc::new(|_| {}))
    }

    /// Run a query and report the results as the plugins finish.
    ///
    /// The original pushes every plugin's results into the list only once all
    /// of them are done, because it collects them in a `Task.WhenAll`. That
    /// makes a slow plugin hold back the fast ones. Here each plugin hands its
    /// results over as soon as it is done, and the sink is called with the
    /// merged list every time — so the window can show a partial list while a
    /// slow plugin (an out-of-process one, typically) is still working.
    ///
    /// The sink runs on a helper thread, which is why it is shared behind an
    /// `Arc`; it must not block for long.
    pub fn query_with_sink(
        &self,
        raw: &str,
        sink: Arc<dyn Fn(Vec<LauncherResult>) + Send + Sync>,
    ) -> QueryOutcome {
        self.query_with_sink_at(raw, sink, 0)
    }

    /// Run a query and report the results as the plugins finish.
    ///
    /// The number tells the query which one it is. A query that is overtaken
    /// by a newer one can see that and stop, instead of finishing work that
    /// nobody wants any more.
    pub fn query_with_sink_at(
        &self,
        raw: &str,
        sink: Arc<dyn Fn(Vec<LauncherResult>) + Send + Sync>,
        generation: u64,
    ) -> QueryOutcome {
        let started = Instant::now();
        let (mut query, targets) = {
            let _timer = crate::profile::phase("query.parse");
            self.builder.read().build_routed(raw)
        };
        query.generation = generation;

        // Plugins that always participate, plus the keyword-specific ones.
        let selected: Vec<Registered> = self
            .plugins
            .read()
            .iter()
            .filter(|p| {
                targets.iter().any(|t| t == p.plugin.id())
                    || p.keywords.iter().any(|k| k == crate::query::GLOBAL_PLUGIN_WILDCARD)
            })
            .cloned()
            .collect();

        if selected.is_empty() {
            return QueryOutcome {
                query,
                results: Vec::new(),
                elapsed: started.elapsed(),
                per_plugin: Vec::new(),
            };
        }

        // Each plugin sends its results here as soon as it is done.
        let (tx, rx) = std::sync::mpsc::channel::<(String, Vec<LauncherResult>)>();

        // The collector runs next to the plugins so the sink is called while
        // the slower ones are still working.
        let max_results = self.max_results.clone();
        let picks = self.picks.read().clone();
        // How the results of the different plugins are put in order.
        let order = *self.result_order.read();
        // The ranking learns from the text the user typed, not from the part
        // of it that was handed to the plugins.
        let query_text = query.original_query.clone();
        let collector = std::thread::Builder::new()
            .name("query-collector".into())
            .spawn(move || {
                // The results are sorted by their *effective* score — the
                // match score plus what the user learned — while the score
                // that is shown stays the plain match score.
                //
                // The groups come first: the programs the user can start are
                // above the files, because a file name matches a lot better
                // than a program name and the files would bury the programs.
                //
                // The group is worked out once per row, not in the comparison
                // — the sort calls the comparison O(n log n) times.
                let mut merged: Vec<(u8, i32, LauncherResult)> = Vec::new();
                let mut kept: Vec<&(u8, i32, LauncherResult)> = Vec::new();
                let mut per_plugin: Vec<(String, usize)> = Vec::new();
                // The key of the query is the same for every row of this query,
                // so it is worked out **once** here and not once per row.
                let query_key = crate::history::query_key(&query_text);
                for (id, results) in rx {
                    per_plugin.push((id, results.len()));
                    let _timer = crate::profile::phase("query.merge");
                    for result in results {
                        let effective = match &picks {
                            Some(memory) => {
                                let item = item_key(
                                    &result.plugin_id,
                                    &result.title,
                                    &result.subtitle,
                                );
                                result.score + memory.bonus_hashed(query_key, item)
                            }
                            None => result.score,
                        };
                        let group = result_group(&result.plugin_id, order, result.path_only_match);
                        merged.push((group, effective, result));
                    }
                    merged.sort_by(|a, b| {
                        a.0.cmp(&b.0)
                            .then_with(|| b.1.cmp(&a.1))
                            .then_with(|| a.2.title.cmp(&b.2.title))
                    });
                    kept = keep_by_group(&merged, max_results.load(Ordering::SeqCst), order);
                    sink(kept.iter().map(|(_, _, result)| result.clone()).collect());
                }
                (
                    kept.into_iter()
                        .map(|(_, _, result)| result.clone())
                        .collect(),
                    per_plugin,
                )
            })
            .ok();

        // The plugins themselves run on the rayon pool, so a keystroke does
        // not pay for a thread per plugin.
        rayon::scope(|scope| {
            for r in &selected {
                let tx = tx.clone();
                let query = query.clone();
                scope.spawn(move |_| {
                    let started = Instant::now();
                    // The name of the phase carries the plugin, so the report
                    // says which plugin the time went into.
                    let phase_name = format!("query.plugin.{}", r.plugin.id());
                    let result = {
                        let _timer = crate::profile::phase(&phase_name);
                        match r.plugin.query(&query) {
                            Ok(results) => results,
                            Err(PluginError::Query(msg)) => {
                                log::warn!("plugin {} failed: {msg}", r.plugin.id());
                                Vec::new()
                            }
                            Err(e) => {
                                log::warn!("plugin {} failed: {e}", r.plugin.id());
                                Vec::new()
                            }
                        }
                    };
                    log::debug!(
                        "plugin {:<12} {:>6} results in {:?}",
                        r.plugin.id(),
                        result.len(),
                        started.elapsed()
                    );
                    let _ = tx.send((r.plugin.id().to_string(), result));
                });
            }
        });
        drop(tx);

        let (results, per_plugin) = collector
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();

        QueryOutcome {
            query,
            results,
            elapsed: started.elapsed(),
            per_plugin,
        }
    }

    /// Run the action of a result the user selected.
    ///
    /// The plugin that produced the result gets to act on it. Returns whether
    /// the window should close afterwards; `false` keeps it open.
    pub fn execute(&self, result: &LauncherResult) -> bool {
        let plugin = self
            .plugins
            .read()
            .iter()
            .find(|r| r.plugin.id() == result.plugin_id)
            .map(|r| r.plugin.clone());
        let Some(plugin) = plugin else {
            return false;
        };
        match plugin.execute(result) {
            Ok(hide) => hide,
            Err(e) => {
                log::warn!("plugin {} failed to run its action: {e}", plugin.id());
                false
            }
        }
    }

    /// Score a set of candidates against a query using the engine's matcher.
    ///
    /// Used by plugins that hold their own local index.
    pub fn score_candidates(&self, query: &str, candidates: &[(String, String)]) -> Vec<(usize, i32)> {
        let matcher = self.matcher.read();
        let mut scored: Vec<(usize, i32)> = candidates
            .iter()
            .enumerate()
            .filter_map(|(i, (title, subtitle))| {
                let r = matcher.match_result(query, title).or_else(|| {
                    matcher.match_result(query, subtitle)
                })?;
                Some((i, r.score))
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored
    }

    /// The matcher used for local candidate scoring.
    pub fn matcher(&self) -> RwLockReadGuard<'_, StringMatcher> {
        self.matcher.read()
    }

    /// Ask every plugin to reload its data.
    pub fn reload_all(&self) {
        let plugins: Vec<Arc<dyn QueryPlugin>> = self
            .plugins
            .read()
            .iter()
            .map(|r| r.plugin.clone())
            .collect();
        for plugin in plugins {
            if let Err(e) = plugin.reload() {
                log::warn!("plugin {} reload failed: {e}", plugin.id());
            }
        }
    }

    /// Whether a reload was requested by a plugin.
    pub fn take_reload_request(&self) -> bool {
        let mut flag = self.reload_requested.lock();
        std::mem::replace(&mut *flag, false)
    }

    /// Request a reload of all plugin data.
    pub fn request_reload(&self) {
        *self.reload_requested.lock() = true;
    }
}

/// How much of the list the programs may take.
///
/// The programs come first, but they may not take the whole list: the files
/// have to be visible too, or the order would be of no use to anybody who
/// is looking for a file.
const PROGRAM_SHARE: usize = 2;
/// Keep the rows that fit in the list.
///
/// The rows are already sorted — the groups first, the score inside a group.
/// The programs may take at most two thirds of the list; the rest belongs to
/// the files and to everything else. A program that does not fill its share
/// gives the room back.
/// Keep the rows that fit in the list.
///
/// The rows are already sorted — the groups first, the score inside a group.
/// The programs may take at most two thirds of the list; the rest belongs to
/// the files and to everything else. A program that does not fill its share
/// gives the room back.
///
/// The rows are **borrowed**, not copied: a row is a handful of strings and
/// two paths, and copying ten thousand of them for every plugin that answers
/// is the most expensive thing the merge does.
fn keep_by_group<'a>(
    merged: &'a [(u8, i32, LauncherResult)],
    max: usize,
    order: ResultOrder,
) -> Vec<&'a (u8, i32, LauncherResult)> {
    if order == ResultOrder::Mixed {
        return merged.iter().take(max).collect();
    }
    // The share of the list that is not for the programs.
    let program_limit = max * PROGRAM_SHARE / 3;
    let mut kept: Vec<&(u8, i32, LauncherResult)> = Vec::with_capacity(max);
    let mut programs = 0usize;
    for item in merged {
        if kept.len() >= max {
            break;
        }
        if item.0 == 0 {
            if programs >= program_limit {
                // Too many programs: this one is left out, and the room goes
                // to the files below.
                continue;
            }
            programs += 1;
        }
        kept.push(item);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::GLOBAL_PLUGIN_WILDCARD;

    struct Static {
        id: &'static str,
        keywords: Vec<String>,
        results: Vec<LauncherResult>,
    }

    impl QueryPlugin for Static {
        fn id(&self) -> &str {
            self.id
        }
        fn name(&self) -> &str {
            self.id
        }
        fn action_keywords(&self) -> &[String] {
            &self.keywords
        }
        fn query(&self, _query: &Query) -> Result<Vec<LauncherResult>, PluginError> {
            Ok(self.results.clone())
        }
    }

    fn ctx() -> PluginContext {
        PluginContext::new("test", std::env::temp_dir(), std::env::temp_dir())
    }

    #[test]
    fn wildcard_plugin_always_runs() {
        let engine = Engine::new(ctx());
        engine.register(Arc::new(Static {
            id: "always",
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
            results: vec![LauncherResult::new("hit")],
        }));
        let outcome = engine.query("anything");
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.results[0].title, "hit");
    }

    #[test]
    fn keyword_plugin_only_runs_when_routed() {
        let engine = Engine::new(ctx());
        engine.register(Arc::new(Static {
            id: "sc",
            keywords: vec!["sc".to_string()],
            results: vec![LauncherResult::new("scoped")],
        }));
        assert!(engine.query("plain").results.is_empty());
        assert_eq!(engine.query("sc thing").results.len(), 1);
    }

    /// A plugin that returns one row of a given kind.
    fn one(id: &'static str, title: &str, score: i32) -> Static {
        Static {
            id,
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
            results: vec![LauncherResult {
                title: title.into(),
                plugin_id: id.into(),
                score,
                ..Default::default()
            }],
        }
    }

    #[test]
    fn the_programs_are_above_the_files() {
        let engine = Engine::new(ctx());
        // The file matches better than the program — that is the whole
        // reason the two kinds are put in separate groups.
        engine.register(Arc::new(one("file", "a.txt", 900)));
        engine.register(Arc::new(one("program", "a.exe", 100)));
        let outcome = engine.query("a");
        let titles: Vec<&str> = outcome.results.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["a.exe", "a.txt"],
            "the program has to be above the file"
        );
    }

    #[test]
    fn a_path_only_program_ranks_below_a_named_file() {
        // The case of `clash`: *Hermes* is a program whose path contains the
        // letters c-l-a-s-h scattered through it, while a *Clash* folder is
        // a file whose name matches outright. The program may not outrank
        // the file on the strength of its path alone.
        let engine = Engine::new(ctx());
        let hermes = Static {
            id: "program",
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
            results: vec![LauncherResult {
                title: "hermes".into(),
                plugin_id: "program".into(),
                score: 100,
                path_only_match: true,
                ..Default::default()
            }],
        };
        engine.register(Arc::new(hermes));
        engine.register(Arc::new(one("file", "Clash Verge", 900)));
        let outcome = engine.query("clash");
        let titles: Vec<&str> = outcome.results.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Clash Verge", "hermes"],
            "the file matched by its name outranks the program matched by its path"
        );
    }

    #[test]
    fn the_files_come_last() {
        let engine = Engine::new(ctx());
        engine.register(Arc::new(one("file", "readme.md", 500)));
        engine.register(Arc::new(one("bookmark", "readme", 100)));
        let outcome = engine.query("readme");
        let titles: Vec<&str> = outcome.results.iter().map(|r| r.title.as_str()).collect();
        // There is no program, so the bookmark is at the top — but the file
        // is still the last one.
        assert_eq!(titles, vec!["readme", "readme.md"]);
    }

    #[test]
    fn the_programs_do_not_take_the_whole_list() {
        let engine = Engine::new(ctx());
        engine.set_max_results(10);
        // Ten programs and ten files: the files have to stay visible.
        engine.register(Arc::new(Static {
            id: "program",
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
            results: (0..10)
                .map(|i| LauncherResult {
                    title: format!("p{i}"),
                    plugin_id: "program".into(),
                    score: 900 - i,
                    ..Default::default()
                })
                .collect(),
        }));
        engine.register(Arc::new(Static {
            id: "file",
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
            results: (0..10)
                .map(|i| LauncherResult {
                    title: format!("f{i}"),
                    plugin_id: "file".into(),
                    score: 800 - i,
                    ..Default::default()
                })
                .collect(),
        }));
        let outcome = engine.query("x");
        let programs = outcome
            .results
            .iter()
            .filter(|r| r.plugin_id == "program")
            .count();
        let files = outcome
            .results
            .iter()
            .filter(|r| r.plugin_id == "file")
            .count();
        assert_eq!(outcome.results.len(), 10);
        assert_eq!(programs, 6, "the programs take at most two thirds");
        assert_eq!(files, 4, "the files have to stay visible");
    }

    #[test]
    fn the_room_of_the_programs_goes_to_the_files() {
        let engine = Engine::new(ctx());
        engine.set_max_results(10);
        engine.register(Arc::new(Static {
            id: "program",
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
            results: vec![LauncherResult {
                title: "only-program".into(),
                plugin_id: "program".into(),
                score: 100,
                ..Default::default()
            }],
        }));
        engine.register(Arc::new(Static {
            id: "file",
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
            results: (0..10)
                .map(|i| LauncherResult {
                    title: format!("f{i}"),
                    plugin_id: "file".into(),
                    score: 900 - i,
                    ..Default::default()
                })
                .collect(),
        }));
        let outcome = engine.query("x");
        assert_eq!(outcome.results.len(), 10);
        assert_eq!(
            outcome.results[0].title, "only-program",
            "the one program is at the top"
        );
        assert_eq!(
            outcome.results[1].title, "f0",
            "the room the program did not use goes to the files"
        );
    }

    #[test]
    fn the_mixed_order_puts_the_score_first() {
        let engine = Engine::new(ctx());
        engine.set_result_order(ResultOrder::Mixed);
        engine.register(Arc::new(one("file", "a.txt", 900)));
        engine.register(Arc::new(one("program", "a.exe", 100)));
        let outcome = engine.query("a");
        let titles: Vec<&str> = outcome.results.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["a.txt", "a.exe"]);
    }

    #[test]
    fn the_store_applications_count_as_programs() {
        let engine = Engine::new(ctx());
        engine.register(Arc::new(one("uwp", "store.app", 100)));
        engine.register(Arc::new(one("file", "store.txt", 900)));
        let outcome = engine.query("store");
        let titles: Vec<&str> = outcome.results.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["store.app", "store.txt"]);
    }

    #[test]
    fn results_are_sorted_by_score() {
        let engine = Engine::new(ctx());
        engine.register(Arc::new(Static {
            id: "a",
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
            results: vec![
                LauncherResult::new("low").with_score(10),
                LauncherResult::new("high").with_score(900),
                LauncherResult::new("mid").with_score(100),
            ],
        }));
        let outcome = engine.query("x");
        let titles: Vec<&str> = outcome.results.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["high", "mid", "low"]);
    }

    #[test]
    fn results_are_truncated_to_max() {
        let engine = Engine::new(ctx());
        engine.set_max_results(2);
        engine.register(Arc::new(Static {
            id: "a",
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
            results: (0..10)
                .map(|i| LauncherResult::new(format!("r{i}")).with_score(1000 - i))
                .collect(),
        }));
        assert_eq!(engine.query("x").results.len(), 2);
    }

    #[test]
    fn a_row_that_is_picked_often_moves_up() {
        let dir = std::env::temp_dir().join("ideas-launcher-engine-picks");
        let _ = std::fs::remove_dir_all(&dir);
        let memory = Arc::new(crate::history::SelectionMemory::load(
            dir.join("UserSelectedRecord.json"),
        ));

        let engine = Engine::new(ctx());
        engine.register(Arc::new(Static {
            id: "program",
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
            results: vec![
                LauncherResult {
                    title: "first".into(),
                    plugin_id: "program".into(),
                    score: 900,
                    ..Default::default()
                },
                LauncherResult {
                    title: "second".into(),
                    plugin_id: "program".into(),
                    score: 800,
                    ..Default::default()
                },
                LauncherResult {
                    title: "third".into(),
                    plugin_id: "program".into(),
                    score: 700,
                    ..Default::default()
                },
            ],
        }));
        engine.set_pick_memory(memory.clone());

        // Without any history the order is the one the plugin gave.
        let outcome = engine.query("x");
        assert_eq!(outcome.results[0].title, "first");

        // The user keeps picking the third one.
        let item = crate::history::item_key("program", "third", "");
        for _ in 0..3 {
            memory.record_pick("x", item);
        }

        let outcome = engine.query("x");
        assert_eq!(
            outcome.results[0].title, "third",
            "the row that is picked often should come first: {:?}",
            outcome
                .results
                .iter()
                .map(|r| &r.title)
                .collect::<Vec<_>>()
        );

        // The score that is shown is still the plain match score.
        let third = outcome
            .results
            .iter()
            .find(|r| r.title == "third")
            .expect("the row is still there");
        assert_eq!(
            third.score, 700,
            "the bonus must not change the score that is shown"
        );

        // Another query is not affected.
        let outcome = engine.query("something else");
        assert_eq!(outcome.results[0].title, "first");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_failure_does_not_break_the_query() {        struct Failing {
            keywords: Vec<String>,
        }
        impl QueryPlugin for Failing {
            fn id(&self) -> &str {
                "failing"
            }
            fn name(&self) -> &str {
                "failing"
            }
            fn action_keywords(&self) -> &[String] {
                &self.keywords
            }
            fn query(&self, _q: &Query) -> Result<Vec<LauncherResult>, PluginError> {
                Err(PluginError::Query("boom".into()))
            }
        }

        let engine = Engine::new(ctx());
        engine.register(Arc::new(Failing {
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
        }));
        engine.register(Arc::new(Static {
            id: "ok",
            keywords: vec![GLOBAL_PLUGIN_WILDCARD.to_string()],
            results: vec![LauncherResult::new("fine")],
        }));
        let outcome = engine.query("x");
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.results[0].title, "fine");
    }
}
