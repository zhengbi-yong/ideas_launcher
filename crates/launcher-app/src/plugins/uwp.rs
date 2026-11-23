//! The Microsoft Store (UWP) applications.
//!
//! Flow Launcher indexes the Store apps together with the Win32 programs, and
//! so does this launcher — but as a separate plugin, because a Store app has
//! no path on disk: it is started through its application user model id
//! instead.
//!
//! The index itself lives in `launcher_index::uwp`; this module turns it into
//! results and starts the applications.

use std::collections::HashMap;
use std::sync::Arc;

use launcher_core::matcher::StringMatcher;
use launcher_core::plugin::{PluginResult, QueryPlugin};
use launcher_core::pinyin::PinyinIndex;
use launcher_core::query::{GLOBAL_PLUGIN_WILDCARD, Query};
use launcher_core::result::{Result as LauncherResult, ResultIcon};
use launcher_index::uwp::UwpIndex;
use parking_lot::RwLock;

/// One application, prepared for searching.
#[derive(Debug, Clone)]
struct Entry {
    /// The localised display name.
    name: String,
    /// The description, when the shell exposes one.
    description: String,
    /// The application user model id.
    aumid: String,
    /// Lower-cased `name + description`.
    search_text: String,
    /// Which ASCII characters occur in `search_text`.
    text_mask: u128,
    /// Which ASCII characters occur in the display name.
    name_mask: u128,
    /// The pinyin rendering of the display name.
    pinyin: Option<PinyinIndex>,
}

/// The plugin that answers with the installed Store applications.
pub struct UwpPlugin {
    entries: Arc<RwLock<Vec<Entry>>>,
    /// The icon of every application, as PNG bytes.
    icons: Arc<RwLock<HashMap<String, Arc<Vec<u8>>>>>,
    matcher: StringMatcher,
}

impl UwpPlugin {
    /// Create the plugin.
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(Vec::new())),
            icons: Arc::new(RwLock::new(HashMap::new())),
            matcher: StringMatcher::new(),
        }
    }

    /// Build the index in the background.
    ///
    /// A cached index is used right away — reading it costs a couple of
    /// milliseconds, while walking the shell's `AppsFolder` takes far longer.
    /// The cache is refreshed in the background so that an app that was
    /// installed in the meantime shows up without a restart.
    pub fn start_indexing(&self) {
        let entries = self.entries.clone();
        let icons = self.icons.clone();
        let cache_path = crate::paths::cache_directory().join("uwp-index.json");

        std::thread::Builder::new()
            .name("uwp-index".into())
            .spawn(move || {
                if let Some(cached) = UwpIndex::load(&cache_path) {
                    log::info!("uwp index loaded from cache ({} apps)", cached.apps.len());
                    install(&entries, &icons, &cached);
                }

                // The shell is walked on every start, but off the hot path.
                let fresh = UwpIndex::enumerate();
                if fresh.apps.is_empty() {
                    log::info!("no store applications were found");
                    return;
                }
                log::info!("uwp index built ({} apps)", fresh.apps.len());
                if let Err(e) = fresh.save(&cache_path) {
                    log::warn!("could not write the uwp index cache: {e}");
                }
                install(&entries, &icons, &fresh);
            })
            .ok();
    }

    /// How many applications the index holds.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }
}

impl Default for UwpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

/// Turn an index into the entries the plugin searches.
fn install(
    entries: &Arc<RwLock<Vec<Entry>>>,
    icons: &Arc<RwLock<HashMap<String, Arc<Vec<u8>>>>>,
    index: &UwpIndex,
) {
    let mut prepared = Vec::with_capacity(index.apps.len());
    let mut encoded: HashMap<String, Arc<Vec<u8>>> = HashMap::new();

    for app in &index.apps {
        if let Some(icon) = &app.icon {
            if let Some(png) = encode_png(icon.width, icon.height, &icon.rgba) {
                encoded.insert(app.aumid.clone(), Arc::new(png));
            }
        }

        let search_text = format!("{} {}", app.display_name, app.description).to_lowercase();
        prepared.push(Entry {
            name: app.display_name.clone(),
            description: app.description.clone(),
            aumid: app.aumid.clone(),
            text_mask: ascii_mask(&search_text),
            name_mask: ascii_mask(&app.display_name.to_lowercase()),
            search_text,
            pinyin: PinyinIndex::build(&app.display_name),
        });
    }

    *icons.write() = encoded;
    *entries.write() = prepared;
}

/// Which ASCII characters occur in a string.
fn ascii_mask(text: &str) -> u128 {
    let mut mask = 0u128;
    for byte in text.bytes() {
        if byte.is_ascii() && byte != b' ' {
            mask |= 1u128 << (byte - b'a');
        }
    }
    mask
}

/// Encode raw RGBA pixels as a PNG.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }
    let buffer = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let mut out = std::io::Cursor::new(Vec::new());
    buffer
        .write_to(&mut out, image::ImageFormat::Png)
        .ok()?;
    Some(out.into_inner())
}

impl QueryPlugin for UwpPlugin {
    fn id(&self) -> &str {
        "uwp"
    }

    fn name(&self) -> &str {
        "Store Apps"
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

        let prepared = self.matcher.prepare(search);
        let icons = self.icons.read();
        let all = self.entries.read();
        let mut scored: Vec<(i32, &Entry, Vec<(usize, usize)>)> = Vec::new();

        for entry in all.iter() {
            let name_match = if prepared.mask_admits(entry.name_mask) {
                self.matcher.match_prepared_lower(&prepared, &entry.name.to_lowercase())
            } else {
                None
            };
            let text_match = if prepared.mask_admits(entry.text_mask) {
                self.matcher.match_prepared_lower(&prepared, &entry.search_text)
            } else {
                None
            };
            let pinyin_match = match (&entry.pinyin, prepared.has_cjk()) {
                (Some(pinyin), false) => {
                    self.matcher.match_prepared_lower(&prepared, pinyin.pinyin_lower())
                }
                _ => None,
            };

            let mut best_score = 0;
            let mut highlight: Vec<(usize, usize)> = Vec::new();
            let mut from_pinyin = false;

            if let Some(a) = name_match {
                best_score = a.score;
                highlight = a.matched_indices;
            }
            if let Some(b) = text_match {
                if b.score > best_score {
                    best_score = b.score;
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
                    highlight = entry
                        .pinyin
                        .as_ref()
                        .map(|p| p.map_ranges(&highlight))
                        .unwrap_or_default();
                }
                scored.push((best_score, entry, highlight));
            }
        }

        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));

        Ok(scored
            .into_iter()
            .take(30)
            .map(|(score, entry, highlight)| LauncherResult {
                title: entry.name.clone(),
                subtitle: if entry.description.is_empty() {
                    entry.aumid.clone()
                } else {
                    entry.description.clone()
                },
                score,
                plugin_id: "uwp".into(),
                context_data: Some(entry.aumid.clone()),
                title_highlight: highlight,
                icon: match icons.get(&entry.aumid) {
                    Some(png) => ResultIcon::Bytes(png.clone()),
                    None => ResultIcon::None,
                },
                ..Default::default()
            })
            .collect())
    }

    fn execute(&self, result: &LauncherResult) -> PluginResult<bool> {
        let Some(aumid) = &result.context_data else {
            return Ok(false);
        };
        if UwpIndex::activate(aumid) {
            Ok(true)
        } else {
            log::warn!("could not start the store application {aumid}");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ascii_mask_covers_the_letters() {
        let mask = ascii_mask("calculator");
        assert_ne!(mask & (1u128 << ('c' as u8 - b'a')), 0);
        assert_eq!(mask & (1u128 << ('z' as u8 - b'a')), 0);
    }

    #[test]
    fn an_empty_index_answers_nothing() {
        let plugin = UwpPlugin::new();
        assert!(plugin.query(&Query::plain("anything")).unwrap().is_empty());
        assert!(plugin.is_empty());
    }

    #[test]
    fn encodes_a_png() {
        let pixels = vec![255u8; 2 * 2 * 4];
        let png = encode_png(2, 2, &pixels).expect("png");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }
}
