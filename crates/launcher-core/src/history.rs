//! Query history and selection memory.
//!
//! Flow Launcher keeps two stores next to its settings:
//!
//! * `History.json` — a flat list of `{ Query, ExecutedDateTime }` for every
//!   executed query, so the user can walk back through what they typed.
//! * `UserSelectedRecord.json` — `recordsWithQuery`, a map from a hash of the
//!   query to the index of the row the user actually picked. The next time the
//!   same query comes in, that row is moved to the top.
//!
//! Both are implemented here with the same on-disk shape, so an existing
//! Flow Launcher history can be adopted as-is.
//!
//! The selection memory does more than that here: it counts *how often* each
//! row was picked, by the row's identity rather than by its position, and the
//! engine turns those counts into a score bonus. A row that the user keeps
//! choosing therefore climbs up the list on its own, while a row that was
//! picked once by accident stays where it is.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// One executed query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryItem {
    /// The text the user typed.
    pub query: String,
    /// When it was executed, as an ISO-8601 local timestamp.
    #[serde(rename = "ExecutedDateTime")]
    pub executed_at: String,
}

/// The persisted history document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HistoryFile {
    #[serde(rename = "Items", default)]
    items: Vec<HistoryItem>,
}

/// The persisted selection-memory document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SelectionFile {
    /// Flow Launcher's shape: a hash of the query → the index of the row the
    /// user picked.
    ///
    /// The original only remembers the *last* pick and moves that row to the
    /// top right away. That is too eager and too fragile — the index says
    /// nothing about *which* row it was. The field is kept so the original
    /// and an older build can still read the file, but the ranking is driven
    /// by [`SelectionFile::pick_counts`] now.
    #[serde(rename = "recordsWithQuery", default)]
    records_with_query: HashMap<u64, usize>,
    /// A hash of the query → a hash of the row → how often it was picked.
    ///
    /// This is what the smart ranking learns from.
    #[serde(rename = "pickCounts", default)]
    pick_counts: HashMap<u64, HashMap<u64, u32>>,
}

/// The query history.
#[derive(Debug)]
pub struct History {
    path: PathBuf,
    items: Vec<HistoryItem>,
    /// Maximum number of entries kept on disk.
    pub capacity: usize,
}

impl History {
    /// Open the history stored at `path`.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let items = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<HistoryFile>(&text).ok())
            .map(|f| f.items)
            .unwrap_or_default();
        Self {
            path,
            items,
            capacity: 1000,
        }
    }

    /// The recorded queries, oldest first.
    pub fn items(&self) -> &[HistoryItem] {
        &self.items
    }

    /// The most recent queries, newest first.
    pub fn recent(&self, limit: usize) -> Vec<&HistoryItem> {
        self.items.iter().rev().take(limit).collect()
    }

    /// Record an executed query.
    ///
    /// Consecutive duplicates are collapsed, matching the behaviour of a
    /// "type the same thing twice" flow.
    pub fn record(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        if self.items.last().map(|i| i.query.as_str()) == Some(query) {
            return;
        }
        self.items.push(HistoryItem {
            query: query.to_string(),
            executed_at: now_iso8601(),
        });
        if self.items.len() > self.capacity {
            let excess = self.items.len() - self.capacity;
            self.items.drain(0..excess);
        }
        self.save();
    }

    /// Remove all entries.
    pub fn clear(&mut self) {
        self.items.clear();
        self.save();
    }

    /// Write the history to disk.
    pub fn save(&self) {
        let file = HistoryFile {
            items: self.items.clone(),
        };
        write_json(&self.path, &file);
    }
}

/// The score bonus a row earns for being picked often.
///
/// Below the threshold nothing happens at all: a row the user picked once
/// stays where it is. Once the threshold is reached the row jumps over the
/// matches below it, and every further pick adds a little more, so a row the
/// user keeps choosing ends up at the top.
const BONUS_BASE: u32 = 250;
/// How much every pick beyond the threshold adds.
const BONUS_STEP: u32 = 80;
/// The largest bonus a row can earn.
const BONUS_MAX: u32 = 600;

/// The default number of picks before a row is promoted.
pub const DEFAULT_PICK_THRESHOLD: u32 = 3;

/// Remembers which rows the user picked for a given query, so that the rows
/// they keep choosing can climb up the list.
///
/// The state sits behind a lock, so the memory can be shared with the query
/// engine without a lock around it.
#[derive(Debug)]
pub struct SelectionMemory {
    path: PathBuf,
    state: RwLock<SelectionState>,
}

#[derive(Debug, Default)]
struct SelectionState {
    /// Query hash → the index of the row the user picked last.
    records: HashMap<u64, usize>,
    /// Query hash → row hash → how often it was picked.
    pick_counts: HashMap<u64, HashMap<u64, u32>>,
    /// How many picks a row needs before it is promoted.
    threshold: u32,
}

impl SelectionMemory {
    /// Open the memory stored at `path`.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<SelectionFile>(&text).ok())
            .unwrap_or_default();
        Self {
            path,
            state: RwLock::new(SelectionState {
                records: file.records_with_query,
                pick_counts: file.pick_counts,
                threshold: DEFAULT_PICK_THRESHOLD,
            }),
        }
    }

    /// How many picks a row needs before it is promoted.
    pub fn threshold(&self) -> u32 {
        self.state.read().threshold
    }

    /// Set how many picks a row needs before it is promoted.
    ///
    /// Zero turns the learning off.
    pub fn set_threshold(&self, threshold: u32) {
        self.state.write().threshold = threshold;
    }

    /// The remembered index for `query`, if any.
    pub fn get(&self, query: &str) -> Option<usize> {
        self.state.read().records.get(&query_key(query)).copied()
    }

    /// Remember that row `index` was picked for `query`.
    pub fn record(&self, query: &str, index: usize) {
        self.state.write().records.insert(query_key(query), index);
        self.save();
    }

    /// Remember that `item` was picked for `query`.
    ///
    /// This is what the ranking learns from: the row is counted by its
    /// identity, not by the position it happened to sit at.
    pub fn record_pick(&self, query: &str, item: u64) {
        {
            let mut state = self.state.write();
            let counts = state.pick_counts.entry(query_key(query)).or_default();
            let count = counts.entry(item).or_insert(0);
            *count = count.saturating_add(1);
        }
        self.save();
    }

    /// How often `item` was picked for `query`.
    pub fn pick_count(&self, query: &str, item: u64) -> u32 {
        self.pick_count_hashed(query_key(query), item)
    }

    /// How often `item` was picked for the query whose key is known.
    ///
    /// The key of the query is the same for every row of one query, so it is
    /// worked out once and handed over — otherwise every row of the list
    /// would make a string of it and hash it again.
    pub fn pick_count_hashed(&self, query_key: u64, item: u64) -> u32 {
        self.state
            .read()
            .pick_counts
            .get(&query_key)
            .and_then(|counts| counts.get(&item))
            .copied()
            .unwrap_or(0)
    }

    /// The score bonus `item` has earned for `query`.
    ///
    /// Returns zero while the row has not been picked often enough, so that a
    /// single pick never moves anything.
    pub fn bonus(&self, query: &str, item: u64) -> i32 {
        self.bonus_hashed(query_key(query), item)
    }

    /// The score bonus of a row, with the key of the query known.
    ///
    /// The key of the query is the same for every row of one query. It is
    /// worked out once for the whole query and handed over here, so that the
    /// list does not make a string of it for every row.
    pub fn bonus_hashed(&self, query_key: u64, item: u64) -> i32 {
        let threshold = self.threshold();
        if threshold == 0 {
            return 0;
        }
        let count = self.pick_count_hashed(query_key, item);
        if count < threshold {
            return 0;
        }
        let extra = count - threshold;
        BONUS_BASE
            .saturating_add(extra.saturating_mul(BONUS_STEP))
            .min(BONUS_MAX) as i32
    }

    /// Forget everything.
    pub fn clear(&self) {
        {
            let mut state = self.state.write();
            state.records.clear();
            state.pick_counts.clear();
        }
        self.save();
    }

    /// Write the memory to disk.
    pub fn save(&self) {
        let file = {
            let state = self.state.read();
            SelectionFile {
                records_with_query: state.records.clone(),
                pick_counts: state.pick_counts.clone(),
            }
        };
        write_json(&self.path, &file);
    }
}

/// The identity of a result row.
///
/// The plugin it came from, its title and its subtitle together say "which
/// row" — the position it happened to sit at says nothing, because the order
/// changes with every keystroke.
pub fn item_key(plugin_id: &str, title: &str, subtitle: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in plugin_id
        .as_bytes()
        .iter()
        .chain(title.as_bytes())
        .chain(subtitle.as_bytes())
    {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// The key a query is stored under.
///
/// Flow Launcher uses an unspecified 32-bit hash; a stable 64-bit FNV-1a is
/// used here so the file stays compatible in shape while avoiding collisions
/// on realistic query sets.
pub fn query_key(query: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in query.trim().to_lowercase().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// Write JSON to disk atomically, so a crash cannot corrupt the file.
fn write_json<T: Serialize>(path: &Path, value: &T) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(text) = serde_json::to_string_pretty(value) else {
        return;
    };
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// The current time as an ISO-8601 timestamp in UTC.
///
/// The on-disk shape matches Flow Launcher's `History.json`; only the offset
/// notation differs (`Z` instead of `+08:00`), which keeps the crate free of
/// platform-specific time-zone code.
pub fn now_iso8601() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let (y, mo, d, h, mi, s) = civil_from_unix(now);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.0000000Z")
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
#[cfg(test)]
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date from days since the Unix epoch.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Unix timestamp for a civil date and time.
#[cfg(test)]
fn civil_to_unix(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s
}

/// Split a Unix timestamp into its civil parts.
fn civil_from_unix(unix: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    (y, mo, d, secs / 3600, (secs % 3600) / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_records_and_dedups() {
        let dir = std::env::temp_dir().join("ideas-launcher-history-test");
        let path = dir.join("History.json");
        let mut history = History::load(&path);
        history.record("hello");
        history.record("hello");
        history.record("world");
        assert_eq!(history.items().len(), 2);

        let reloaded = History::load(&path);
        assert_eq!(reloaded.items().len(), 2);
        assert_eq!(reloaded.items()[0].query, "hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_ignores_blank_queries() {
        let dir = std::env::temp_dir().join("ideas-launcher-history-blank");
        let mut history = History::load(dir.join("History.json"));
        history.record("   ");
        assert!(history.items().is_empty());
    }

    #[test]
    fn selection_memory_round_trips() {
        let dir = std::env::temp_dir().join("ideas-launcher-selection-test");
        let path = dir.join("UserSelectedRecord.json");
        let memory = SelectionMemory::load(&path);
        memory.record("vscode", 3);
        assert_eq!(memory.get("vscode"), Some(3));
        // The lookup is case-insensitive.
        assert_eq!(memory.get("VSCode"), Some(3));

        let reloaded = SelectionMemory::load(&path);
        assert_eq!(reloaded.get("vscode"), Some(3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_single_pick_does_not_move_anything() {
        let dir = std::env::temp_dir().join("ideas-launcher-selection-threshold");
        let _ = std::fs::remove_dir_all(&dir);
        let memory = SelectionMemory::load(dir.join("UserSelectedRecord.json"));

        let item = item_key("program", "Chrome", "C:\\Chrome\\chrome.exe");
        assert_eq!(memory.bonus("chrome", item), 0);

        // Not even the second pick moves it.
        memory.record_pick("chrome", item);
        assert_eq!(memory.pick_count("chrome", item), 1);
        assert_eq!(memory.bonus("chrome", item), 0);

        memory.record_pick("chrome", item);
        assert_eq!(memory.pick_count("chrome", item), 2);
        assert_eq!(memory.bonus("chrome", item), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_row_that_is_picked_often_is_promoted() {
        let dir = std::env::temp_dir().join("ideas-launcher-selection-promote");
        let _ = std::fs::remove_dir_all(&dir);
        let memory = SelectionMemory::load(dir.join("UserSelectedRecord.json"));
        let item = item_key("program", "Chrome", "C:\\Chrome\\chrome.exe");

        for _ in 0..3 {
            memory.record_pick("chrome", item);
        }
        let first = memory.bonus("chrome", item);
        assert!(first > 0, "the row should be rewarded: {first}");

        // Every further pick adds a little more.
        memory.record_pick("chrome", item);
        let second = memory.bonus("chrome", item);
        assert!(second > first, "{second} should be more than {first}");

        // The bonus is capped.
        for _ in 0..50 {
            memory.record_pick("chrome", item);
        }
        assert!(memory.bonus("chrome", item) <= BONUS_MAX as i32);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_learning_can_be_switched_off() {
        let dir = std::env::temp_dir().join("ideas-launcher-selection-off");
        let _ = std::fs::remove_dir_all(&dir);
        let memory = SelectionMemory::load(dir.join("UserSelectedRecord.json"));
        let item = item_key("program", "Chrome", "");

        for _ in 0..10 {
            memory.record_pick("chrome", item);
        }
        assert!(memory.bonus("chrome", item) > 0);

        memory.set_threshold(0);
        assert_eq!(memory.bonus("chrome", item), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pick_counts_survive_a_restart() {
        let dir = std::env::temp_dir().join("ideas-launcher-selection-restart");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("UserSelectedRecord.json");
        let item = item_key("program", "Chrome", "");

        let memory = SelectionMemory::load(&path);
        for _ in 0..3 {
            memory.record_pick("chrome", item);
        }

        let reloaded = SelectionMemory::load(&path);
        assert_eq!(reloaded.pick_count("chrome", item), 3);
        assert!(reloaded.bonus("chrome", item) > 0);

        // The file still has the shape the original expects.
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("recordsWithQuery"), "{text}");
        assert!(text.contains("pickCounts"), "{text}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_rows_are_told_apart_by_their_identity() {
        // The same title from another plugin is another row.
        let chrome = item_key("program", "Chrome", "");
        let bookmark = item_key("bookmark", "Chrome", "");
        assert_ne!(chrome, bookmark);
        // The position does not matter.
        assert_eq!(item_key("program", "Chrome", "a"), item_key("program", "Chrome", "a"));
    }

    #[test]
    fn history_file_matches_flow_launcher_shape() {
        let file = HistoryFile {
            items: vec![HistoryItem {
                query: "设置".into(),
                executed_at: "2026-08-22T10:57:12.2860468+08:00".into(),
            }],
        };
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains("\"Items\""));
        assert!(json.contains("\"ExecutedDateTime\""));
    }

    #[test]
    fn civil_conversions_round_trip() {
        let (y, mo, d, h, mi, s) = (2026, 11, 2, 13, 45, 7);
        let unix = civil_to_unix(y, mo, d, h, mi, s);
        let back = civil_from_unix(unix);
        assert_eq!(back, (y, mo, d, h, mi, s));
    }

    #[test]
    fn timestamp_has_iso_shape() {
        let stamp = now_iso8601();
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(stamp.len(), 28, "{stamp}");
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[10..11], "T");
    }
}
