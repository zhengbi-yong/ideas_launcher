//! Query parsing and action-keyword routing.
//!
//! Mirrors the `Query` / `QueryBuilder` contract observed in Flow Launcher's
//! `Flow.Launcher.Plugin.dll` and `Flow.Launcher.Core.dll`:
//!
//! * `OriginalQuery` — exactly what the user typed.
//! * `RawQuery`      — original query with the action keyword removed.
//! * `TrimmedQuery`  — raw query trimmed of whitespace.
//! * `Search`        — the part of the query handed to plugins.
//! * `SearchTerms`   — `Search` split on the term separator.
//! * `ActionKeyword` — the leading keyword that routed the query to a plugin.

use std::collections::HashMap;

/// Wildcard that routes a query to every plugin that registered it.
pub const GLOBAL_PLUGIN_WILDCARD: &str = "*";

/// Default separator between search terms.
pub const TERM_SEPARATOR: char = ' ';

/// Separator between an action keyword and the rest of the query.
pub const ACTION_KEYWORD_SEPARATOR: char = ' ';

/// A parsed user query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// Exactly what the user typed, unmodified.
    pub original_query: String,
    /// The query with the leading action keyword removed.
    pub raw_query: String,
    /// `raw_query` with surrounding whitespace trimmed.
    pub trimmed_query: String,
    /// The portion of the query that plugins should match against.
    pub search: String,
    /// `search` split on [`TERM_SEPARATOR`], empty terms removed.
    pub search_terms: Vec<String>,
    /// The action keyword that routed this query, if any.
    pub action_keyword: Option<String>,
    /// True when the query re-runs the previous search (e.g. after a refresh).
    pub is_requery: bool,
    /// True when the query is the empty "home page" query.
    pub is_home_query: bool,
    /// The number of this query.
    ///
    /// A new query gets a new number, and a query that is still running can see
    /// that it has been overtaken — its answer is of no use to anybody then.
    pub generation: u64,
}

impl Query {
    /// Build a query from raw user input without action-keyword routing.
    pub fn plain(raw: impl Into<String>) -> Self {
        let original_query = raw.into();
        let trimmed_query = original_query.trim().to_string();
        let search_terms = split_terms(&trimmed_query);
        let is_home_query = trimmed_query.is_empty();
        Self {
            search: trimmed_query.clone(),
            raw_query: trimmed_query.clone(),
            original_query,
            trimmed_query,
            search_terms,
            action_keyword: None,
            is_requery: false,
            is_home_query,
            generation: 0,
        }
    }

    /// The full search string.
    pub fn first_search(&self) -> &str {
        &self.search
    }

    /// The search string with the final term removed.
    ///
    /// Used as a widening fallback when the full query yields too few results.
    pub fn second_search(&self) -> String {
        self.search_without_last(1)
    }

    /// The search string with the final two terms removed.
    pub fn third_search(&self) -> String {
        self.search_without_last(2)
    }

    fn search_without_last(&self, n: usize) -> String {
        if self.search_terms.len() <= n {
            return String::new();
        }
        self.search_terms[..self.search_terms.len() - n].join(" ")
    }

    /// The query split into individual terms.
    pub fn split_search(&self) -> &[String] {
        &self.search_terms
    }

    /// True when this query carries no searchable text.
    pub fn is_empty(&self) -> bool {
        self.trimmed_query.is_empty()
    }
}

fn split_terms(input: &str) -> Vec<String> {
    input
        .split(TERM_SEPARATOR)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Maps action keywords to the plugins that registered them.
///
/// Keywords are matched case-insensitively. The wildcard `*` means "run for
/// every plugin that has no more specific keyword match".
#[derive(Debug, Default, Clone)]
pub struct ActionKeywordIndex {
    /// keyword -> plugin ids
    by_keyword: HashMap<String, Vec<String>>,
    /// plugins that registered the wildcard
    wildcard: Vec<String>,
}

impl ActionKeywordIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `plugin_id` for `keyword`.
    pub fn register(&mut self, plugin_id: impl Into<String>, keyword: &str) {
        let plugin_id = plugin_id.into();
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return;
        }
        if keyword == GLOBAL_PLUGIN_WILDCARD {
            if !self.wildcard.contains(&plugin_id) {
                self.wildcard.push(plugin_id);
            }
            return;
        }
        let entry = self.by_keyword.entry(keyword.to_ascii_lowercase()).or_default();
        if !entry.contains(&plugin_id) {
            entry.push(plugin_id);
        }
    }

    /// Remove a plugin from every keyword it registered.
    pub fn unregister(&mut self, plugin_id: &str) {
        self.wildcard.retain(|id| id != plugin_id);
        for ids in self.by_keyword.values_mut() {
            ids.retain(|id| id != plugin_id);
        }
        self.by_keyword.retain(|_, ids| !ids.is_empty());
    }

    /// Plugins registered under `keyword` (exact, case-insensitive).
    pub fn plugins_for(&self, keyword: &str) -> &[String] {
        self.by_keyword
            .get(&keyword.to_ascii_lowercase())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Plugins registered under the wildcard.
    pub fn wildcard_plugins(&self) -> &[String] {
        &self.wildcard
    }

    /// All registered keywords, sorted.
    pub fn keywords(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.by_keyword.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    /// True when no keyword has been registered.
    pub fn is_empty(&self) -> bool {
        self.by_keyword.is_empty() && self.wildcard.is_empty()
    }
}

/// Turns raw user input into a routed [`Query`].
#[derive(Debug, Default, Clone)]
pub struct QueryBuilder {
    index: ActionKeywordIndex,
}

impl QueryBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_index(index: ActionKeywordIndex) -> Self {
        Self { index }
    }

    pub fn index(&self) -> &ActionKeywordIndex {
        &self.index
    }

    pub fn index_mut(&mut self) -> &mut ActionKeywordIndex {
        &mut self.index
    }

    /// Parse `raw` into a query, extracting a leading action keyword when one
    /// matches a registered plugin.
    ///
    /// The keyword is the first whitespace-delimited token. A keyword that is
    /// not registered stays in the search text, so plugins still see it.
    pub fn build(&self, raw: &str) -> Query {
        let original_query = raw.to_string();
        let trimmed = raw.trim();

        if trimmed.is_empty() {
            let mut q = Query::plain(original_query);
            q.is_home_query = true;
            return q;
        }

        // Split off the first token only if it is a registered keyword.
        let (first_token, rest) = match trimmed.split_once(ACTION_KEYWORD_SEPARATOR) {
            Some((head, tail)) => (head, tail.trim_start()),
            None => (trimmed, ""),
        };

        // The wildcard is a routing marker, never a literal keyword.
        let keyword = if self.index.plugins_for(first_token).is_empty() {
            None
        } else {
            Some(first_token.to_ascii_lowercase())
        };

        match keyword {
            Some(kw) => {
                let search = rest.to_string();
                let trimmed_query = search.trim().to_string();
                let search_terms = split_terms(&trimmed_query);
                Query {
                    original_query,
                    raw_query: trimmed_query.clone(),
                    trimmed_query,
                    search,
                    search_terms,
                    action_keyword: Some(kw),
                    is_requery: false,
                    is_home_query: false,
                    generation: 0,
                }
            }
            None => {
                let mut q = Query::plain(original_query);
                q.is_home_query = false;
                q
            }
        }
    }

    /// Parse `raw` and report which plugins should handle it.
    ///
    /// Returns the routed query plus the plugin ids that should be queried:
    /// the keyword-specific plugins first, then the wildcard plugins.
    pub fn build_routed(&self, raw: &str) -> (Query, Vec<String>) {
        let query = self.build(raw);
        let mut targets = Vec::new();

        if let Some(kw) = &query.action_keyword {
            for id in self.index.plugins_for(kw) {
                if !targets.contains(id) {
                    targets.push(id.clone());
                }
            }
        }
        for id in self.index.wildcard_plugins() {
            if !targets.contains(id) {
                targets.push(id.clone());
            }
        }
        (query, targets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> QueryBuilder {
        let mut b = QueryBuilder::new();
        b.index_mut().register("calc", "*");
        b.index_mut().register("ws", "sc");
        b.index_mut().register("term", "term");
        b
    }

    #[test]
    fn empty_query_is_home_query() {
        let q = builder().build("   ");
        assert!(q.is_home_query);
        assert!(q.is_empty());
        assert_eq!(q.search_terms.len(), 0);
    }

    #[test]
    fn known_keyword_is_extracted() {
        let (q, targets) = builder().build_routed("sc hello world");
        assert_eq!(q.action_keyword.as_deref(), Some("sc"));
        assert_eq!(q.search, "hello world");
        assert_eq!(q.search_terms, vec!["hello", "world"]);
        assert_eq!(q.trimmed_query, "hello world");
        assert!(targets.contains(&"ws".to_string()));
        assert!(targets.contains(&"calc".to_string()));
    }

    #[test]
    fn unknown_keyword_stays_in_search() {
        let q = builder().build("unknown thing");
        assert_eq!(q.action_keyword, None);
        assert_eq!(q.search, "unknown thing");
    }

    #[test]
    fn keyword_with_no_payload() {
        let (q, _) = builder().build_routed("sc");
        assert_eq!(q.action_keyword.as_deref(), Some("sc"));
        assert_eq!(q.search, "");
    }

    #[test]
    fn fallback_searches_drop_tail_terms() {
        let q = builder().build("term alpha beta gamma");
        assert_eq!(q.first_search(), "alpha beta gamma");
        assert_eq!(q.second_search(), "alpha beta");
        assert_eq!(q.third_search(), "alpha");
    }

    #[test]
    fn keyword_index_case_insensitive() {
        let mut idx = ActionKeywordIndex::new();
        idx.register("p", "Go");
        assert_eq!(idx.plugins_for("go").len(), 1);
        assert_eq!(idx.plugins_for("GO").len(), 1);
    }

    #[test]
    fn wildcard_is_separate_from_named_keywords() {
        let mut idx = ActionKeywordIndex::new();
        idx.register("p", "*");
        assert_eq!(idx.wildcard_plugins().len(), 1);
        assert!(idx.plugins_for("*").is_empty());
    }
}
