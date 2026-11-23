//! The built-in plugins.
//!
//! Each one mirrors a plugin that ships with Flow Launcher: `Program`,
//! `Calculator`, `Web Searches`, `Shell` and `URL`, plus the ones added here
//! to cover the rest of that feature set — `Files`, `System Commands`,
//! `Browser Bookmarks` and `Processes`.

pub mod bookmarks;
pub mod files;
pub mod jsonrpc;
pub mod processes;
pub mod sys;
pub mod uwp;
pub mod windows_settings;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use launcher_core::plugin::{PluginContext, PluginResult, QueryPlugin};
use launcher_core::query::{GLOBAL_PLUGIN_WILDCARD, Query};
use launcher_core::result::{Result as LauncherResult, ResultIcon};
use launcher_index::{IndexOptions, Program, ProgramIndex};
use parking_lot::RwLock;

// ----------------------------------------------------------------- program

/// Searches the indexed Windows programs.
pub struct ProgramPlugin {
    index: Arc<RwLock<Option<Arc<ProgramIndex>>>>,
    matcher: launcher_core::matcher::StringMatcher,
    /// Set while a background index build is running.
    indexing: Arc<AtomicBool>,
}

impl ProgramPlugin {
    pub fn new() -> Self {
        Self {
            index: Arc::new(RwLock::new(None)),
            matcher: launcher_core::matcher::StringMatcher::new(),
            indexing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A plugin over an already built index.
    pub fn with_index(index: Arc<ProgramIndex>) -> Self {
        Self {
            index: Arc::new(RwLock::new(Some(index))),
            matcher: launcher_core::matcher::StringMatcher::new(),
            indexing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The current index, if one has been built.
    pub fn index(&self) -> Option<Arc<ProgramIndex>> {
        self.index.read().clone()
    }

    /// Start building the index on a background thread.
    ///
    /// A fresh cache is used right away — reading it costs a couple of
    /// milliseconds, while a full rebuild takes about 35. Only when there is
    /// no usable cache is the filesystem walked, and the result is written
    /// back for the next start.
    ///
    /// The launcher stays usable while this runs.
    pub fn start_indexing(&self) {
        if self.indexing.swap(true, Ordering::SeqCst) {
            return;
        }
        let index = self.index.clone();
        let indexing = self.indexing.clone();
        let cache_path = crate::paths::cache_directory().join("program-index.json");
        std::thread::Builder::new()
            .name("program-index".into())
            .spawn(move || {
                let cache = launcher_index::IndexCache::new(&cache_path);
                if let Some(cached) = {
                    let _timer = launcher_core::profile::phase("startup.programs");
                    cache.load()
                } {
                    log::info!("program index loaded from cache ({} programs)", cached.programs.len());
                    *index.write() = Some(Arc::new(cached));
                    indexing.store(false, Ordering::SeqCst);
                    return;
                }

                let built = {
                    let _timer = launcher_core::profile::phase("startup.programs");
                    ProgramIndex::build(&IndexOptions::default())
                };
                log::info!("program index built ({} programs)", built.programs.len());
                if let Err(e) = cache.save(&built) {
                    log::warn!("could not write the program index cache: {e}");
                }
                *index.write() = Some(Arc::new(built));
                indexing.store(false, Ordering::SeqCst);
            })
            .ok();
    }
}

impl Default for ProgramPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryPlugin for ProgramPlugin {
    fn id(&self) -> &str {
        "program"
    }

    fn name(&self) -> &str {
        "Programs"
    }

    fn action_keywords(&self) -> &[String] {
        static KEYWORDS: std::sync::LazyLock<Vec<String>> =
            std::sync::LazyLock::new(|| vec![GLOBAL_PLUGIN_WILDCARD.to_string()]);
        &KEYWORDS
    }

    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
        let search = query.search.trim();
        if search.is_empty() {
            return Ok(Vec::new());
        }
        let Some(index) = self.index() else {
            return Ok(Vec::new());
        };

        let mut scored: Vec<(i32, &Program, Vec<(usize, usize)>, bool)> = Vec::new();
        // The query is prepared once and reused for every program.
        let prepared = self.matcher.prepare(search);
        for program in &index.programs {
            // The haystack was lower-cased once when the index was built, so a
            // query never allocates per candidate.
            let title_match = if prepared.mask_admits(program.name_mask) {
                self.matcher.match_prepared_lower(&prepared, &program.name)
            } else {
                None
            };
            // The character mask rules most candidates out without touching
            // the (long) search text at all.
            let haystack_match = if prepared.mask_admits(program.text_mask) {
                self.matcher.match_prepared_lower(&prepared, &program.search_text)
            } else {
                None
            };

            // A CJK name can also be reached through its pinyin: `chongqi`
            // finds "重启". The pinyin form was built with the index, so this
            // costs one extra match and nothing else.
            let pinyin_match = match (&program.pinyin, prepared.has_cjk()) {
                (Some(pinyin), false) => {
                    self.matcher.match_prepared_lower(&prepared, &pinyin.pinyin_lower())
                }
                _ => None,
            };

            // The best of the three, with the highlight of the winner.
            let mut best_score = 0;
            let mut highlight: Vec<(usize, usize)> = Vec::new();
            let mut from_pinyin = false;
            let mut matched_title = false;

            if let Some(a) = title_match {
                best_score = a.score;
                highlight = a.matched_indices;
                matched_title = true;
            }
            if let Some(b) = haystack_match {
                if b.score > best_score {
                    best_score = b.score;
                    // The match is somewhere in the path, not in the title, so
                    // there is nothing to highlight.
                    highlight = Vec::new();
                }
            }
            if let Some(c) = pinyin_match {
                if c.score > best_score {
                    best_score = c.score;
                    highlight = c.matched_indices;
                    from_pinyin = true;
                }
            }

            if best_score > 0 {
                if from_pinyin {
                    // Map the pinyin hit back onto the original characters.
                    highlight = program
                        .pinyin
                        .as_ref()
                        .map(|p| p.map_ranges(&highlight))
                        .unwrap_or_default();
                }
                // Whether the match is on the **name** of the program (or its
                // pinyin), and not only on the letters scattered through its
                // path. A path-only match ranks with the files: `clash`
                // must not put *Hermes* — whose path merely contains the
                // letters — above every *Clash* on the machine.
                let path_only = !(matched_title || from_pinyin);
                scored.push((best_score, program, highlight, path_only));
            }
        }

        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));

        Ok(scored
            .into_iter()
            .take(1_000)
            .map(|(score, program, highlight, path_only)| {
                LauncherResult {
                    title: program.name.clone(),
                    subtitle: if program.description.is_empty() {
                        program.full_path.to_string_lossy().to_string()
                    } else {
                        program.description.clone()
                    },
                    icon: match &program.icon_path {
                        Some(p) => ResultIcon::Path(p.clone()),
                        None => ResultIcon::None,
                    },
                    score,
                    title_highlight: highlight,
                    plugin_id: "program".into(),
                    copy_text: program.full_path.to_string_lossy().to_string(),
                    path_only_match: path_only,
                    ..Default::default()
                }
            })
            .collect())
    }
}

// -------------------------------------------------------------- calculator

/// Evaluates simple arithmetic expressions.
pub struct CalculatorPlugin;

impl QueryPlugin for CalculatorPlugin {
    fn id(&self) -> &str {
        "calculator"
    }

    fn name(&self) -> &str {
        "Calculator"
    }

    fn action_keywords(&self) -> &[String] {
        static KEYWORDS: std::sync::LazyLock<Vec<String>> =
            std::sync::LazyLock::new(|| vec!["calc".to_string(), "=".to_string()]);
        &KEYWORDS
    }

    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
        let expression = query.search.trim();
        if expression.is_empty() {
            return Ok(Vec::new());
        }
        // Only treat it as arithmetic when it looks like arithmetic.
        if !expression
            .chars()
            .all(|c| c.is_ascii_digit() || "+-*/%(). ".contains(c))
        {
            return Ok(Vec::new());
        }
        if !expression.chars().any(|c| "+-*/%".contains(c)) {
            return Ok(Vec::new());
        }

        match evaluate(expression) {
            Some(value) => Ok(vec![LauncherResult {
                title: format!("{expression} = {value}"),
                subtitle: "Calculator".into(),
                score: 1000,
                plugin_id: "calculator".into(),
                copy_text: value.to_string(),
                ..Default::default()
            }]),
            None => Ok(Vec::new()),
        }
    }
}

/// Evaluate an arithmetic expression with a small recursive-descent parser.
pub fn evaluate(expression: &str) -> Option<f64> {
    let tokens: Vec<char> = expression.chars().filter(|c| !c.is_whitespace()).collect();
    let mut pos = 0usize;

    fn parse_expr(tokens: &[char], pos: &mut usize) -> Option<f64> {
        let mut left = parse_term(tokens, pos)?;
        while *pos < tokens.len() {
            match tokens[*pos] {
                '+' => {
                    *pos += 1;
                    left += parse_term(tokens, pos)?;
                }
                '-' => {
                    *pos += 1;
                    left -= parse_term(tokens, pos)?;
                }
                _ => break,
            }
        }
        Some(left)
    }

    fn parse_term(tokens: &[char], pos: &mut usize) -> Option<f64> {
        let mut left = parse_factor(tokens, pos)?;
        while *pos < tokens.len() {
            match tokens[*pos] {
                '*' => {
                    *pos += 1;
                    left *= parse_factor(tokens, pos)?;
                }
                '/' => {
                    *pos += 1;
                    let right = parse_factor(tokens, pos)?;
                    if right == 0.0 {
                        return None;
                    }
                    left /= right;
                }
                '%' => {
                    *pos += 1;
                    let right = parse_factor(tokens, pos)?;
                    if right == 0.0 {
                        return None;
                    }
                    left %= right;
                }
                _ => break,
            }
        }
        Some(left)
    }

    fn parse_factor(tokens: &[char], pos: &mut usize) -> Option<f64> {
        if *pos >= tokens.len() {
            return None;
        }
        match tokens[*pos] {
            '-' => {
                *pos += 1;
                Some(-parse_factor(tokens, pos)?)
            }
            '+' => {
                *pos += 1;
                parse_factor(tokens, pos)
            }
            '(' => {
                *pos += 1;
                let value = parse_expr(tokens, pos)?;
                if *pos >= tokens.len() || tokens[*pos] != ')' {
                    return None;
                }
                *pos += 1;
                Some(value)
            }
            _ => {
                let start = *pos;
                while *pos < tokens.len() && tokens[*pos].is_ascii_digit() {
                    *pos += 1;
                }
                if *pos < tokens.len() && tokens[*pos] == '.' {
                    *pos += 1;
                    while *pos < tokens.len() && tokens[*pos].is_ascii_digit() {
                        *pos += 1;
                    }
                }
                if start == *pos {
                    return None;
                }
                let text: String = tokens[start..*pos].iter().collect();
                text.parse::<f64>().ok()
            }
        }
    }

    let value = parse_expr(&tokens, &mut pos)?;
    if pos != tokens.len() {
        return None;
    }
    if value.is_finite() {
        Some(value)
    } else {
        None
    }
}

// --------------------------------------------------------------- web search

/// A single web-search provider.
#[derive(Clone)]
pub struct WebSearch {
    pub name: &'static str,
    pub keyword: &'static str,
    pub url_template: &'static str,
}

/// The built-in search providers, mirroring Flow Launcher's `Web Searches`
/// plugin (23 providers in the analysed install).
pub fn default_web_searches() -> Vec<WebSearch> {
    vec![
        WebSearch { name: "Google", keyword: "g", url_template: "https://www.google.com/search?q={query}" },
        WebSearch { name: "Bing", keyword: "b", url_template: "https://www.bing.com/search?q={query}" },
        WebSearch { name: "DuckDuckGo", keyword: "dd", url_template: "https://duckduckgo.com/?q={query}" },
        WebSearch { name: "Wikipedia", keyword: "wiki", url_template: "https://en.wikipedia.org/wiki/Special:Search?search={query}" },
        WebSearch { name: "GitHub", keyword: "gh", url_template: "https://github.com/search?q={query}" },
        WebSearch { name: "YouTube", keyword: "yt", url_template: "https://www.youtube.com/results?search_query={query}" },
        WebSearch { name: "Stack Overflow", keyword: "so", url_template: "https://stackoverflow.com/search?q={query}" },
        WebSearch { name: "Translate", keyword: "translate", url_template: "https://translate.google.com/?text={query}" },
        WebSearch { name: "Maps", keyword: "maps", url_template: "https://www.google.com/maps/search/{query}" },
        WebSearch { name: "Wolfram Alpha", keyword: "wa", url_template: "https://www.wolframalpha.com/input?i={query}" },
        WebSearch { name: "Amazon", keyword: "amz", url_template: "https://www.amazon.com/s?k={query}" },
        WebSearch { name: "Twitter", keyword: "tw", url_template: "https://twitter.com/search?q={query}" },
    ]
}

/// Offers the configured web searches for the current query.
pub struct WebSearchPlugin {
    searches: Vec<WebSearch>,
}

impl WebSearchPlugin {
    pub fn new(searches: Vec<WebSearch>) -> Self {
        Self { searches }
    }
}

impl QueryPlugin for WebSearchPlugin {
    fn id(&self) -> &str {
        "websearch"
    }

    fn name(&self) -> &str {
        "Web Searches"
    }

    fn action_keywords(&self) -> &[String] {
        static KEYWORDS: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(Vec::new);
        &KEYWORDS
    }

    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
        let term = query.search.trim();
        if term.is_empty() {
            return Ok(Vec::new());
        }
        // A keyword-scoped query only offers the matching provider.
        let scoped = query.action_keyword.as_deref().and_then(|kw| {
            self.searches
                .iter()
                .find(|s| s.keyword.eq_ignore_ascii_case(kw))
        });
        let providers: Vec<&WebSearch> = match scoped {
            Some(s) => vec![s],
            None => self.searches.iter().collect(),
        };
        Ok(providers
            .into_iter()
            .map(|s| {
                let url = s.url_template.replace("{query}", &url_encode(term));
                LauncherResult {
                    title: format!("Search {term} on {}", s.name),
                    subtitle: url.clone(),
                    score: 900,
                    plugin_id: "websearch".into(),
                    copy_text: url,
                    ..Default::default()
                }
            })
            .collect())
    }
}

/// Percent-encode a string for use in a URL query.
pub fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ------------------------------------------------------------------- shell

/// Runs a command in the user's shell.
pub struct ShellPlugin;

impl QueryPlugin for ShellPlugin {
    fn id(&self) -> &str {
        "shell"
    }

    fn name(&self) -> &str {
        "Shell"
    }

    fn action_keywords(&self) -> &[String] {
        static KEYWORDS: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| vec![">".to_string()]);
        &KEYWORDS
    }

    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
        let command = query.search.trim();
        if command.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![LauncherResult {
            title: command.to_string(),
            subtitle: "Run in shell".into(),
            score: 1000,
            plugin_id: "shell".into(),
            copy_text: command.to_string(),
            ..Default::default()
        }])
    }
}

// --------------------------------------------------------------------- url

/// Opens a URL.
pub struct UrlPlugin;

impl QueryPlugin for UrlPlugin {
    fn id(&self) -> &str {
        "url"
    }

    fn name(&self) -> &str {
        "URL"
    }

    fn action_keywords(&self) -> &[String] {
        static KEYWORDS: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| vec![GLOBAL_PLUGIN_WILDCARD.to_string()]);
        &KEYWORDS
    }

    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
        let term = query.search.trim();
        if term.is_empty() {
            return Ok(Vec::new());
        }
        let lowered = term.to_ascii_lowercase();
        let looks_like_url = lowered.starts_with("http://")
            || lowered.starts_with("https://")
            || (term.contains('.') && !term.contains(' '));
        if !looks_like_url {
            return Ok(Vec::new());
        }
        let url = if lowered.starts_with("http") {
            term.to_string()
        } else {
            format!("https://{term}")
        };
        Ok(vec![LauncherResult {
            title: term.to_string(),
            subtitle: url.clone(),
            score: 950,
            plugin_id: "url".into(),
            copy_text: url,
            ..Default::default()
        }])
    }
}

/// Build the default plugin set.
pub fn default_plugins() -> Vec<Arc<dyn QueryPlugin>> {
    vec![
        Arc::new(ProgramPlugin::new()),
        Arc::new(CalculatorPlugin),
        Arc::new(WebSearchPlugin::new(default_web_searches())),
        Arc::new(ShellPlugin),
        Arc::new(UrlPlugin),
    ]
}

/// The action keywords registered by the default plugin set.
pub fn default_action_keywords() -> Vec<String> {
    let mut keywords = Vec::new();
    for search in default_web_searches() {
        keywords.push(search.keyword.to_string());
    }
    keywords
}

/// The plugin context handed to every plugin.
pub fn plugin_context() -> PluginContext {
    let base = launcher_core::settings::Settings::default_directory();
    PluginContext::new(
        "ideaslauncher",
        base.join("Settings"),
        base.join("Cache"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_arithmetic() {
        assert_eq!(evaluate("1+2*3"), Some(7.0));
        assert_eq!(evaluate("(1+2)*3"), Some(9.0));
        assert_eq!(evaluate("10/4"), Some(2.5));
        assert_eq!(evaluate("-3+1"), Some(-2.0));
    }

    #[test]
    fn rejects_division_by_zero() {
        assert_eq!(evaluate("1/0"), None);
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(evaluate("abc"), None);
        assert_eq!(evaluate("1+"), None);
    }

    #[test]
    fn url_encodes_spaces() {
        assert_eq!(url_encode("a b"), "a+b");
        assert_eq!(url_encode("a&b"), "a%26b");
    }

    #[test]
    fn calculator_plugin_ignores_plain_words() {
        let plugin = CalculatorPlugin;
        let query = Query::plain("hello");
        assert!(plugin.query(&query).unwrap().is_empty());
    }

    #[test]
    fn calculator_plugin_evaluates() {
        let plugin = CalculatorPlugin;
        let query = Query::plain("2+2");
        let results = plugin.query(&query).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].title.contains('4'));
    }

    #[test]
    fn url_plugin_detects_urls() {
        let plugin = UrlPlugin;
        assert!(plugin.query(&Query::plain("example.com")).unwrap().len() == 1);
        assert!(plugin.query(&Query::plain("hello world")).unwrap().is_empty());
    }

    #[test]
    fn a_cjk_program_is_found_through_its_pinyin() {
        let program = Program::new(
            "重启".into(),
            std::path::PathBuf::from("C:/restart.exe"),
            None,
            String::new(),
            None,
            launcher_index::ProgramSourceKind::StartMenu,
            true,
        );
        let index = Arc::new(ProgramIndex {
            programs: vec![program],
            build_millis: 0,
        });
        let plugin = ProgramPlugin::with_index(index);

        // The Chinese name itself matches a Chinese query.
        let direct = plugin.query(&Query::plain("重启")).unwrap();
        assert_eq!(direct.len(), 1);

        // The pinyin of the name matches an ASCII query.
        //
        // "重启" is one of the polyphonic words the override table knows:
        // it is read "chong qi", not the "zhong qi" the single-character
        // dictionary would give. Flow Launcher resolves it the same way.
        let by_pinyin = plugin.query(&Query::plain("chongqi")).unwrap();
        assert_eq!(by_pinyin.len(), 1, "the pinyin of 重启 should match");

        // The reading the dictionary would have picked is no longer found.
        let wrong = plugin.query(&Query::plain("zhongqi")).unwrap();
        assert!(
            wrong.is_empty(),
            "重启 must not answer to the wrong reading zhongqi"
        );
    }

    #[test]
    fn an_ascii_program_is_not_matched_through_a_pinyin_query() {
        let program = Program::new(
            "Chrome".into(),
            std::path::PathBuf::from("C:/chrome.exe"),
            None,
            String::new(),
            None,
            launcher_index::ProgramSourceKind::StartMenu,
            true,
        );
        let index = Arc::new(ProgramIndex {
            programs: vec![program],
            build_millis: 0,
        });
        let plugin = ProgramPlugin::with_index(index);
        assert!(plugin.query(&Query::plain("zhongqi")).unwrap().is_empty());
    }
}
