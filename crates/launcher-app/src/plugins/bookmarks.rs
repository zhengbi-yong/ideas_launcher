//! Browser bookmarks.
//!
//! Mirrors Flow Launcher's `BrowserBookmark` plugin, which reads the bookmark
//! files of Chrome, Edge, Firefox and any custom Chromium-based browser.
//!
//! Chromium stores bookmarks as JSON; the structure is walked recursively so
//! nested folders are picked up. Firefox keeps them in `places.sqlite`, which
//! would pull in a SQLite dependency — it is therefore not read here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use launcher_core::plugin::{PluginResult, QueryPlugin};
use launcher_core::query::{GLOBAL_PLUGIN_WILDCARD, Query};
use launcher_core::result::Result as LauncherResult;

/// One bookmark.
#[derive(Debug, Clone)]
pub struct Bookmark {
    /// The bookmark title.
    pub title: String,
    /// The URL it points at.
    pub url: String,
    /// The browser it came from.
    pub browser: String,
}

/// A browser whose bookmarks can be read.
#[derive(Debug, Clone)]
pub struct Browser {
    /// Display name.
    pub name: String,
    /// The path of the bookmarks file.
    pub path: PathBuf,
}

/// The browsers whose bookmarks can be read.
pub fn known_browsers() -> Vec<Browser> {
    let mut out = Vec::new();

    let local = std::env::var("LOCALAPPDATA").ok().map(PathBuf::from);
    let roaming = std::env::var("APPDATA").ok().map(PathBuf::from);

    if let Some(local) = &local {
        out.push(Browser {
            name: "Chrome".into(),
            path: local.join("Google/Chrome/User Data/Default/Bookmarks"),
        });
        out.push(Browser {
            name: "Edge".into(),
            path: local.join("Microsoft/Edge/User Data/Default/Bookmarks"),
        });
        out.push(Browser {
            name: "Brave".into(),
            path: local.join("BraveSoftware/Brave-Browser/User Data/Default/Bookmarks"),
        });
        out.push(Browser {
            name: "Vivaldi".into(),
            path: local.join("Vivaldi/User Data/Default/Bookmarks"),
        });
    }

    if let Some(roaming) = &roaming {
        out.push(Browser {
            name: "Opera GX".into(),
            path: roaming.join("Opera Software/Opera GX Stable/Default/Bookmarks"),
        });
        out.push(Browser {
            name: "Opera".into(),
            path: roaming.join("Opera Software/Opera Stable/Default/Bookmarks"),
        });
    }

    out
}

/// Read every bookmark the known browsers have.
pub fn load_all() -> Vec<Bookmark> {
    let mut out = Vec::new();
    for browser in known_browsers() {
        if let Some(bookmarks) = load(&browser) {
            out.extend(bookmarks);
        }
    }
    out
}

/// Read the bookmarks of one browser.
pub fn load(browser: &Browser) -> Option<Vec<Bookmark>> {
    let text = std::fs::read_to_string(&browser.path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;

    let mut out = Vec::new();
    if let Some(roots) = json.get("roots") {
        for key in ["bookmark_bar", "other", "synced"] {
            if let Some(node) = roots.get(key) {
                walk(node, &browser.name, &mut out, 0);
            }
        }
    }
    Some(out)
}

/// Walk a bookmark node recursively.
fn walk(node: &serde_json::Value, browser: &str, out: &mut Vec<Bookmark>, depth: usize) {
    if depth > 8 || out.len() > 5000 {
        return;
    }
    let Some(children) = node.get("children").and_then(|c| c.as_array()) else {
        return;
    };
    for child in children {
        let title = child
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_string();
        if let Some(url) = child.get("url").and_then(|u| u.as_str()) {
            if !title.is_empty() {
                out.push(Bookmark {
                    title,
                    url: url.to_string(),
                    browser: browser.to_string(),
                });
            }
        }
        walk(child, browser, out, depth + 1);
    }
}

/// The bookmark plugin.
pub struct BookmarkPlugin {
    bookmarks: std::sync::Arc<parking_lot::RwLock<Vec<Bookmark>>>,
}

impl BookmarkPlugin {
    /// Create the plugin and load the bookmarks.
    pub fn new() -> Self {
        Self {
            bookmarks: std::sync::Arc::new(parking_lot::RwLock::new(load_all())),
        }
    }

    /// Reload the bookmarks from disk.
    pub fn reload(&self) {
        *self.bookmarks.write() = load_all();
    }

    /// How many bookmarks were found.
    pub fn len(&self) -> usize {
        self.bookmarks.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for BookmarkPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryPlugin for BookmarkPlugin {
    fn id(&self) -> &str {
        "bookmark"
    }

    fn name(&self) -> &str {
        "Browser Bookmarks"
    }

    fn action_keywords(&self) -> &[String] {
        static KEYWORDS: std::sync::LazyLock<Vec<String>> =
            std::sync::LazyLock::new(|| vec!["b".to_string(), GLOBAL_PLUGIN_WILDCARD.to_string()]);
        &KEYWORDS
    }

    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
        let term = query.search.trim().to_lowercase();
        if term.is_empty() {
            return Ok(Vec::new());
        }

        // The same bookmark is usually in more than one browser, because the
        // browsers sync. Showing it twice looks like a mistake, so only the
        // first copy is kept.
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();

        for bookmark in self.bookmarks.read().iter() {
            if !bookmark.title.to_lowercase().contains(&term)
                && !bookmark.url.to_lowercase().contains(&term)
            {
                continue;
            }
            if !seen.insert(bookmark.url.trim().to_lowercase()) {
                continue;
            }
            out.push(LauncherResult {
                title: bookmark.title.clone(),
                subtitle: bookmark.url.clone(),
                score: 750,
                plugin_id: "bookmark".into(),
                copy_text: bookmark.url.clone(),
                ..Default::default()
            });
            if out.len() >= 40 {
                break;
            }
        }

        Ok(out)
    }
}

/// Whether a path looks like a Chromium bookmarks file.
pub fn is_bookmark_file(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.eq_ignore_ascii_case("Bookmarks"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_browsers_have_distinct_paths() {
        let browsers = known_browsers();
        let mut paths: Vec<String> = browsers
            .iter()
            .map(|b| b.path.to_string_lossy().to_lowercase())
            .collect();
        let count = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), count);
    }

    #[test]
    fn parses_a_chromium_bookmark_file() {
        let json = r#"{
          "roots": {
            "bookmark_bar": {
              "children": [
                { "name": "Folder", "children": [
                    { "name": "Nested", "url": "https://example.com/nested" }
                ]},
                { "name": "Top", "url": "https://example.com/top" }
              ]
            }
          }
        }"#;
        let dir = std::env::temp_dir().join("ideas-launcher-bookmark-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("Bookmarks");
        std::fs::write(&path, json).unwrap();

        let browser = Browser {
            name: "Test".into(),
            path: path.clone(),
        };
        let bookmarks = load(&browser).expect("bookmarks should load");
        assert_eq!(bookmarks.len(), 2);
        assert!(bookmarks.iter().any(|b| b.title == "Nested"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_yields_nothing() {
        let browser = Browser {
            name: "Nope".into(),
            path: PathBuf::from(r"C:\definitely\not\here\Bookmarks"),
        };
        assert!(load(&browser).is_none());
    }

    #[test]
    fn plugin_matches_bookmark_titles() {
        let plugin = BookmarkPlugin {
            bookmarks: std::sync::Arc::new(parking_lot::RwLock::new(vec![Bookmark {
                title: "Rust Lang".into(),
                url: "https://rust-lang.org".into(),
                browser: "Test".into(),
            }])),
        };
        let results = plugin.query(&Query::plain("rust")).unwrap();
        assert_eq!(results.len(), 1);
        plugin.reload();
    }

    #[test]
    fn a_bookmark_that_is_in_two_browsers_is_only_listed_once() {
        // The browsers sync, so the same bookmark is in Chrome *and* Edge.
        let plugin = BookmarkPlugin {
            bookmarks: std::sync::Arc::new(parking_lot::RwLock::new(vec![
                Bookmark {
                    title: "Rust Lang".into(),
                    url: "https://rust-lang.org".into(),
                    browser: "Chrome".into(),
                },
                Bookmark {
                    title: "Rust Lang".into(),
                    url: "https://rust-lang.org".into(),
                    browser: "Edge".into(),
                },
                Bookmark {
                    title: "Rust Book".into(),
                    url: "https://doc.rust-lang.org/book".into(),
                    browser: "Chrome".into(),
                },
            ])),
        };
        let results = plugin.query(&Query::plain("rust")).unwrap();
        assert_eq!(
            results.len(),
            2,
            "the synced bookmark must not show up twice: {results:?}"
        );
    }
}
