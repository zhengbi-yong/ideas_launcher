//! The full-disk file index.
//!
//! The index holds every file and folder of every NTFS volume in a compact,
//! contiguous form: the names live in one string arena and the tree is stored
//! as parent pointers. That is the same shape Everything builds in its
//! `convert parent frn to parent pointers` step, and it is what makes a
//! search over a million entries a few milliseconds of work.
//!
//! # How a query stays fast
//!
//! A query has to look at every entry, so the per-entry cost decides how
//! fast the launcher feels. Three things keep it down:
//!
//! 1. **A character filter.** Every entry carries a 64 bit mask of the
//!    characters that occur in its name, in its pinyin rendering and in the
//!    name of its folder. A query can only match when all of its characters
//!    are present, so most entries are ruled out by two machine words
//!    without touching the name at all.
//! 2. **No allocation on the hot path.** The query is compiled once; the
//!    names are matched in place and never copied.
//! 3. **Bounded selection.** The scan produces a score per candidate, but
//!    only the best few are ever turned into results, so the memory does not
//!    grow with the size of the index.
//!
//! The filter is applied in parallel over a separate array of masks, which
//! streams nicely and leaves the entry array untouched for the candidates
//! that survive.

use std::collections::HashMap;

use launcher_core::cancel::Cancel;
use launcher_core::search_syntax::{self, Node, ParsedSearch, SearchTarget, Term};
use launcher_core::settings::FileSort;
use rayon::prelude::*;

use crate::usn::{self, FileMeta};

/// Marks an entry without a parent.
pub const NO_PARENT: u32 = u32::MAX;

/// The first four bytes of an index file.
const CACHE_MAGIC: &[u8; 4] = b"FSIX";

/// The first four bytes of a file with the changes since the last full table.
const DELTA_MAGIC: &[u8; 4] = b"FSDL";

/// The version of the file with the changes.
const DELTA_VERSION: u32 = 1;

/// One change of the index that is written between two full tables.
///
/// It is the same idea as a change of the change journal, but with the
/// details that the journal does not carry: the size and the times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexChange {
    /// The file reference number of the file.
    pub frn: u64,
    /// The file reference number of the folder that holds it.
    pub parent_frn: u64,
    /// The name of the file.
    pub name: String,
    /// True when the file is gone.
    pub deleted: bool,
    /// The size of the content.
    pub size: u64,
    /// When the file was created, in seconds since the Unix epoch.
    pub created: u32,
    /// When the content changed the last time.
    pub modified: u32,
    /// When the file was read the last time.
    pub accessed: u32,
    /// The Win32 file attributes.
    pub attributes: u32,
    /// Whether the size and the times are known.
    ///
    /// The change journal does not carry them, so a change that comes from
    /// there leaves the old values alone.
    pub meta_known: bool,
}

/// Bumped whenever the layout of the index file changes.
const CACHE_VERSION: u32 = 8;

/// The low bits of a file reference number that hold the number of the record.
///
/// The high 16 bits are the sequence of the record, and it grows every time
/// the record is used again — so two files can have the same record number
/// and different numbers.
const RECORD_NUMBER_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// One indexed file or folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsEntry {
    /// Offset of the name inside the arena.
    name_start: u32,
    /// Index of the containing folder, or [`NO_PARENT`] for a volume root.
    parent: u32,
    /// The size of the content, when it is known.
    size: u64,
    /// Length of the name in bytes.
    name_len: u16,
    /// How many folders lie between the entry and its volume root.
    depth: u16,
    /// When the file was created, in seconds since the Unix epoch.
    created: u32,
    /// When the content changed the last time.
    modified: u32,
    /// When the file was read the last time.
    accessed: u32,
    /// The Win32 file attributes.
    attributes: u32,
}

impl FsEntry {
    /// True for folders.
    pub fn is_dir(&self) -> bool {
        self.attributes & usn::FILE_ATTRIBUTE_DIRECTORY != 0
    }

    /// The metadata of the entry, when it is known.
    pub fn meta(&self) -> Option<FileMeta> {
        Some(FileMeta {
            size: self.size,
            created: self.created as u64,
            modified: self.modified as u64,
            accessed: self.accessed as u64,
            attributes: self.attributes,
        })
    }
}

/// A volume that the index covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedVolume {
    /// The volume root, for example `C:\`.
    pub root: String,
    /// The device path, for example `\\.\C:`.
    pub device: String,
    /// The volume label.
    pub label: String,
}

/// The range of entries that belong to one volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VolumeRange {
    /// The first entry of the range.
    start: u32,
    /// One past the last entry of the range.
    end: u32,
    /// The index into [`FsIndex::volumes`].
    volume: u32,
}

/// A search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsHit {
    /// The file or folder name.
    pub name: String,
    /// The full path.
    pub path: String,
    /// True for folders.
    pub is_dir: bool,
    /// Size in bytes, when it is known.
    pub size: Option<u64>,
    /// Modification time, when it is known.
    pub modified: Option<u64>,
    /// How well the result matches the query.
    pub score: i32,
}

/// What the index leaves out.
#[derive(Debug, Clone, Default)]
pub struct Exclusions {
    /// Folders that are never indexed, in lower case.
    ///
    /// A folder matches when its path starts with one of these.
    pub paths: Vec<String>,
    /// File extensions that are never indexed, in lower case, without the dot.
    pub extensions: Vec<String>,
}

impl Exclusions {
    /// Nothing is left out.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.extensions.is_empty()
    }

    /// Whether a folder is left out, together with everything below it.
    pub fn excludes_folder(&self, path: &str) -> bool {
        if self.paths.is_empty() {
            return false;
        }
        // The list is in lower case already, so the comparison is done on
        // the bytes: putting the whole path in lower case would allocate a
        // string for every entry of the index.
        let bytes = path.as_bytes();
        self.paths.iter().any(|excluded| {
            let excluded = excluded.as_bytes();
            bytes.len() >= excluded.len() && bytes[..excluded.len()].eq_ignore_ascii_case(excluded)
        })
    }

    /// Whether a file is left out because of its extension.
    pub fn excludes_extension(&self, name: &str) -> bool {
        if self.extensions.is_empty() {
            return false;
        }
        let Some((_, extension)) = name.rsplit_once('.') else {
            return false;
        };
        // The list is in lower case already, so the extension only has to be
        // put in lower case when it has letters in it that are not ASCII.
        let lower = if extension.is_ascii() {
            extension
        } else {
            &extension.to_lowercase()
        };
        self.extensions.iter().any(|e| {
            e.len() == lower.len() && e.as_bytes().eq_ignore_ascii_case(lower.as_bytes())
        })
    }
}

/// The full-disk index.
#[derive(Debug, Default)]
pub struct FsIndex {
    /// The names of every entry, one after the other.
    names: String,
    /// The tree.
    entries: Vec<FsEntry>,
    /// The volumes that are covered.
    volumes: Vec<IndexedVolume>,
    /// Which entries belong to which volume.
    ///
    /// The entries are appended volume by volume, so a handful of ranges
    /// replaces a field in every entry.
    volume_ranges: Vec<VolumeRange>,
    /// The character filter of every entry.
    ///
    /// Kept apart from the entries so that the scan streams over sixteen
    /// times fewer bytes than it would over the whole tree.
    masks: Vec<u64>,
    /// The character filter for queries that match the whole path.
    ///
    /// A path query can match any folder on the way down, so the filter has
    /// to hold the characters of all of them. Without this, a path query
    /// would have to build a million paths.
    path_masks: Vec<u64>,
    /// Metadata that was fetched for individual entries.
    meta: launcher_core::hash::FastMap<u32, FileMeta>,
    /// The pinyin rendering of the names that contain Chinese characters.
    ///
    /// Only the names that actually need it are converted, so the cost of the
    /// index stays proportional to the number of Chinese files rather than to
    /// the number of files.
    ///
    /// The table is looked up for every candidate whose name is not plain
    /// ASCII, and a broad query has millions of candidates — so the keys are
    /// hashed with the quick hash of the core crate and not with the one of
    /// the standard library.
    pinyin: launcher_core::hash::FastMap<u32, String>,
    /// When the index was built, in seconds since the Unix epoch.
    built_at: u64,
    /// When the index was last brought up to date, in seconds since the Unix
    /// epoch.
    ///
    /// The table that was saved is built once, and the changes that came
    /// after it are read back and applied to it. The two moments are usually
    /// not the same, and the age of the index is the age of the **changes**.
    fresh_at: u64,
    /// The fingerprint of the contents of the index, as it was saved.
    ///
    /// It answers one question: *is this the same table as the one that was
    /// written to the disk?* When the answer is yes, the table does not have to
    /// be written again — and that write is the biggest one the program ever
    /// does: three quarters of a gigabyte for a big volume.
    saved_fingerprint: u64,
    /// The entries that are left out, one byte each.
    excluded: Vec<bool>,
    /// Which entries have a known size and time, one byte each.
    meta_known: Vec<bool>,
    /// The file reference number of every entry.
    ///
    /// It is what the change journal talks about, so the index can be brought
    /// up to date while the program runs.
    frns: Vec<u64>,
    /// Where the change journal of every volume stood when the table was read.
    ///
    /// The journal is a ring buffer in the file system: it says what changed
    /// since a given point, and the point is what makes the difference
    /// between reading the whole disk again and reading a few hundred
    /// kilobytes.
    ///
    /// The table carries the point with it, so that the table that is read
    /// back from the disk can be brought up to date without reading the disk
    /// again. Zero means that the point is not known — then the table has to
    /// be read again.
    journal_points: Vec<u64>,
    /// The map from the file reference number to the entry.
    ///
    /// It holds **only** the entries that the change journal brought in. The
    /// entries of the master file table are found by a binary search over the
    /// file reference numbers, which are in order — a map of nine million
    /// entries would cost two hundred megabytes of memory, and the memory of
    /// the launcher is worth more than the speed of this.
    frn_index: launcher_core::hash::FastMap<u64, u32>,
    /// Whether the file reference numbers of the master file table are in
    /// order, so that a file can be found by a binary search.
    ///
    /// The table is read in the order of the numbers, so they are. A table
    /// that was put together in another way falls back to the map.
    frns_in_order: bool,
    /// How many entries share a name.
    ///
    /// It is only built when a query asks for the duplicates.
    name_counts: std::sync::OnceLock<HashMap<String, u32>>,
    /// The children of every folder.
    ///
    /// It is only built when a query asks for the children.
    children: std::sync::OnceLock<HashMap<u32, Vec<u32>>>,
}

/// Turn the changes of the change journal into the changes that are written
/// to the disk.
///
/// The journal does not carry the size and the times, so those are left
/// alone: the values that are in the table stay.
pub fn journal_changes(changes: &[usn::JournalChange]) -> Vec<IndexChange> {
    changes.iter().map(journal_change).collect()
}

/// Turn one change of the change journal into the change that is written to
/// the disk.
fn journal_change(change: &usn::JournalChange) -> IndexChange {
    IndexChange {
        frn: change.frn,
        parent_frn: change.parent_frn,
        name: change.name.clone(),
        deleted: change.reason & usn::reason::FILE_DELETE != 0,
        size: 0,
        created: 0,
        modified: 0,
        accessed: 0,
        attributes: change.attributes,
        meta_known: false,
    }
}

impl FsIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many entries the index holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the index holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The volumes that are covered.
    pub fn volumes(&self) -> &[IndexedVolume] {
        &self.volumes
    }

    /// How many bytes the names of the entries take.
    ///
    /// All the names live in one string, so this is the whole of it.
    pub fn name_bytes(&self) -> usize {
        self.names.len()
    }

    /// Give the memory that the growth of the tables left over back.
    ///
    /// A table that grows by doubling holds twice the memory it needs when
    /// it is half full, and an index of a million entries holds a few dozen
    /// megabytes more than it needs. The walk is over at this point, so the
    /// tables are squeezed once and then left alone.
    pub fn shrink(&mut self) {
        self.names.shrink_to_fit();
        self.entries.shrink_to_fit();
        self.masks.shrink_to_fit();
        self.path_masks.shrink_to_fit();
        self.excluded.shrink_to_fit();
        self.meta_known.shrink_to_fit();
        self.frns.shrink_to_fit();
    }

    /// How many bytes the entries take, apart from their names.
    pub fn entry_bytes(&self) -> usize {
        self.entries.len() * std::mem::size_of::<FsEntry>()
            + self.masks.len() * 8
            + self.path_masks.len() * 8
            + self.excluded.len()
            + self.meta_known.len()
            + self.frns.len() * 8
    }

    /// The name of one entry.
    pub fn name(&self, index: u32) -> &str {
        let entry = &self.entries[index as usize];
        &self.names[entry.name_start as usize..(entry.name_start + entry.name_len as u32) as usize]
    }

    /// Build the index from the entries of the given volumes.
    ///
    /// The input is the flat list the MFT enumeration produces: every entry
    /// knows its own name and the file reference number of its folder. The
    /// names live in one arena that the index takes over.
    pub fn build(
        volumes: &[usn::Volume],
        entries: &[(u32, Vec<usn::RawEntry>)],
        names: String,
        exclusions: &Exclusions,
    ) -> Self {
        let mut sink = EntrySink::new(volumes);
        sink.index.names = names;
        let total: usize = entries.iter().map(|(_, list)| list.len()).sum();
        sink.reserve(total);
        for (volume_id, list) in entries {
            sink.begin_volume(*volume_id, false);
            for entry in list {
                sink.add_raw(entry);
            }
            sink.end_volume();
        }
        sink.finish(exclusions)
    }

    /// Build the index while the entries are still being read.
    ///
    /// The entries are handed over one at a time, so the flat list of them
    /// never exists on its own. That is what makes the difference between a
    /// peak of twice the size of the index and a peak of the size of the
    /// index, on a volume with millions of files.
    pub fn build_from(
        volumes: &[usn::Volume],
        exclusions: &Exclusions,
        feed: impl FnOnce(&mut EntrySink),
    ) -> Self {
        let mut sink = EntrySink::new(volumes);
        feed(&mut sink);
        sink.finish(exclusions)
    }

    /// Mark the entries that the user does not want to see.
    ///
    /// The check is done once, while the index is built, so that the query
    /// does not have to look at paths it is going to throw away anyway.
    fn apply_exclusions(&mut self, exclusions: &Exclusions) {
        let count = self.entries.len();
        self.excluded = vec![false; count];
        if exclusions.is_empty() {
            return;
        }
        // The first pass marks the entries that are left out on their own.
        let mut own: Vec<bool> = vec![false; count];
        for index in 0..count {
            let name = self.name(index as u32);
            let by_extension =
                !self.entries[index].is_dir() && exclusions.excludes_extension(name);
            // The path is only needed when a folder is excluded, and building
            // it means walking up the tree and joining the parts.
            let by_folder = !exclusions.paths.is_empty()
                && exclusions.excludes_folder(&self.path(index as u32));
            own[index] = by_folder || by_extension;
        }
        // The second pass hands the mark down to the children. The entries are
        // walked from the top, so a folder is always handled before what is
        // inside it.
        //
        // The depth is a small number, so the entries are put in buckets
        // instead of being sorted: sorting millions of entries means
        // millions of random accesses, and the buckets are a single pass.
        let deepest = self
            .entries
            .iter()
            .map(|entry| entry.depth as usize)
            .max()
            .unwrap_or(0);
        let mut places = vec![0usize; deepest + 2];
        for entry in &self.entries {
            places[entry.depth as usize + 1] += 1;
        }
        for depth in 1..places.len() {
            places[depth] += places[depth - 1];
        }
        let mut order: Vec<u32> = vec![0; count];
        for (index, entry) in self.entries.iter().enumerate() {
            let depth = entry.depth as usize;
            order[places[depth]] = index as u32;
            places[depth] += 1;
        }
        for index in order {
            let parent = self.entries[index as usize].parent;
            if parent != NO_PARENT && self.excluded[parent as usize] {
                self.excluded[index as usize] = true;
            } else {
                self.excluded[index as usize] = own[index as usize];
            }
        }
    }

    /// Append one entry to the arena and the tree.
    fn push_entry(&mut self, _volume: u32, record: &usn::RawEntry, parent: u32) -> u32 {
        let name_start = record.name_start;
        let name_len = record.name_len;
        self.entries.push(FsEntry {
            name_start,
            name_len: name_len.min(u16::MAX as u32) as u16,
            parent,
            attributes: record.attributes,
            depth: 0,
            size: record.size,
            created: record.created,
            modified: record.modified,
            accessed: record.accessed,
        });
        self.meta_known.push(record.meta_known);
        self.frns.push(record.frn);
        self.entries.len() as u32 - 1
    }

    /// The volume an entry belongs to.
    fn volume_of(&self, index: u32) -> usize {
        self.volume_of_depth(index, 0)
    }

    /// The volume an entry belongs to.
    ///
    /// The entries that the change journal brought in are **outside** the
    /// ranges of the volumes — they were added at the end — so their volume
    /// is the one of the folder they are in.
    fn volume_of_depth(&self, index: u32, depth: u32) -> usize {
        for range in &self.volume_ranges {
            if index >= range.start && index < range.end {
                return range.volume as usize;
            }
        }
        if depth > 8 {
            return 0;
        }
        let parent = self
            .entries
            .get(index as usize)
            .map(|entry| entry.parent)
            .unwrap_or(NO_PARENT);
        if parent == NO_PARENT || parent == index {
            return 0;
        }
        self.volume_of_depth(parent, depth + 1)
    }

    /// Work out how deep every entry sits.
    ///
    /// The depth is needed by the `depth:` filter and by the ranking, and
    /// walking the parent chain for a million entries on every keystroke
    /// would cost tens of milliseconds. It is computed once, here.
    fn build_depths(&mut self) {
        // A depth of `u16::MAX` means "not worked out yet".
        for entry in &mut self.entries {
            entry.depth = u16::MAX;
        }
        // The chain is kept between the entries, so that walking over
        // millions of them does not allocate millions of small lists.
        let mut chain: Vec<u32> = Vec::new();
        for start in 0..self.entries.len() {
            if self.entries[start].depth != u16::MAX {
                continue;
            }
            // Collect the chain up to the first known depth.
            chain.clear();
            let mut current = start as u32;
            let mut guard = 0usize;
            let base = loop {
                if current == NO_PARENT {
                    break 0u16;
                }
                let entry = self.entries[current as usize];
                if entry.depth != u16::MAX {
                    break entry.depth;
                }
                chain.push(current);
                current = entry.parent;
                guard += 1;
                if guard > 256 {
                    break 0u16;
                }
            };
            // Write the depths back, deepest first.
            for (step, &index) in chain.iter().enumerate().rev() {
                let depth = base.saturating_add(step as u16 + 1);
                self.entries[index as usize].depth = depth;
            }
        }
    }

    /// Build the character filter of every entry.
    ///
    /// The mask holds the characters of the entry's own name, of its pinyin
    /// rendering and of the name of its folder, because a query can match
    /// any of the three.
    fn build_masks(&mut self) {
        let count = self.entries.len();
        let mut own: Vec<u64> = Vec::with_capacity(count);
        for index in 0..count {
            own.push(self.own_mask_of(index as u32));
        }

        // The filter for a name query: the characters of the entry and of the
        // folder that holds it, because either can be matched.
        self.masks.reserve(count);
        for index in 0..count {
            let parent = self.entries[index].parent;
            let mut mask = own[index];
            if parent != NO_PARENT {
                mask |= own[parent as usize];
            }
            self.masks.push(mask);
        }

        // The filter for a path query: the characters of the entry and of
        // every folder above it, because a path query can match any of them.
        let mut path = vec![0u64; count];
        let mut known = vec![false; count];
        // The chain is kept between the entries, so that walking over
        // millions of them does not allocate millions of small lists.
        let mut chain: Vec<u32> = Vec::new();
        for start in 0..count {
            if known[start] {
                continue;
            }
            chain.clear();
            let mut current = start as u32;
            while current != NO_PARENT && !known[current as usize] && chain.len() < 256 {
                chain.push(current);
                current = self.entries[current as usize].parent;
            }
            let mut value = if current != NO_PARENT && known[current as usize] {
                path[current as usize]
            } else {
                0
            };
            for &index in chain.iter().rev() {
                value |= own[index as usize];
                path[index as usize] = value;
                known[index as usize] = true;
            }
        }
        self.path_masks = path;
    }

    /// Convert the names that contain Chinese characters.
    ///
    /// The pinyin is computed once, while the index is built, so that a query
    /// never has to translate anything.
    fn build_pinyin(&mut self) {
        for index in 0..self.entries.len() {
            let name = self.name(index as u32);
            // A plain ASCII name cannot hold a Chinese character, and the
            // check for that is a fast byte scan.
            if name.is_ascii() || !name.chars().any(is_cjk) {
                continue;
            }
            if let Some(pinyin) = launcher_core::pinyin::PinyinIndex::build(name) {
                let rendered = pinyin.pinyin_lower().to_string();
                if !rendered.is_empty() {
                    self.pinyin.insert(index as u32, rendered);
                }
            }
        }
    }

    /// The full path of one entry.
    ///
    /// The tree is walked upwards and the parts are put back together, the
    /// same way Everything rebuilds a path from its folder pointers.
    pub fn path(&self, index: u32) -> String {
        let mut parts: Vec<&str> = Vec::new();
        let mut current = index;
        let mut guard = 0usize;
        while current != NO_PARENT && guard < 256 {
            let name = self.name(current);
            // The root of a volume is called "." in the master file table,
            // and it is already the beginning of the path. Without this the
            // path would read `C:\.\Users\...`.
            if name != "." {
                parts.push(name);
            }
            current = self.entries[current as usize].parent;
            guard += 1;
        }
        let volume = self.volume_of(index);
        let root = self
            .volumes
            .get(volume)
            .map(|v| v.root.as_str())
            .unwrap_or("C:\\");
        let mut path = String::from(root);
        for part in parts.into_iter().rev() {
            if !path.ends_with('\\') {
                path.push('\\');
            }
            path.push_str(part);
        }
        path
    }

    /// The name of the folder that holds an entry.
    pub fn parent_name(&self, index: u32) -> &str {
        let parent = self.entries[index as usize].parent;
        if parent == NO_PARENT {
            ""
        } else {
            let name = self.name(parent);
            // The root of a volume is called "." — as a parent it is nothing.
            if name == "." {
                ""
            } else {
                name
            }
        }
    }

    /// The number of folders between the entry and its volume root.
    pub fn depth(&self, index: u32) -> u32 {
        self.entries[index as usize].depth as u32
    }

    /// Store the metadata of an entry.
    pub fn set_meta(&mut self, index: u32, meta: FileMeta) {
        self.meta.insert(index, meta);
    }

    /// The metadata of an entry, when it is known.
    pub fn meta(&self, index: u32) -> Option<&FileMeta> {
        self.meta.get(&index)
    }

    /// The moment the index is written, in seconds since the Unix epoch.
    ///
    /// The age of the index is counted from here, and the same number is
    /// written next to the index as the point in the change journal that the
    /// index was taken at — so that the two can be paired up again when the
    /// index is read back.
    pub fn stamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// The fingerprint of the contents of the index.
    ///
    /// Two indexes that hold the same files in the same order have the same
    /// fingerprint. It is the way to find out whether the table that was just
    /// read from the disk is the same as the one that was saved — and if it
    /// is, the saved one does not have to be written again. That write is the
    /// biggest one the program ever does: three quarters of a gigabyte for a
    /// big volume, and it is the one that wears the disk.
    pub fn fingerprint(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        {
            // The bytes are mixed in eight at a time: going over them one by
            // one would cost a second on a big volume, and the fingerprint is
            // only worth having if it is cheap.
            let mut mix = |bytes: &[u8]| {
                let mut words = bytes.chunks_exact(8);
                for word in &mut words {
                    hash ^= u64::from_le_bytes(word.try_into().unwrap());
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
                let rest = words.remainder();
                if !rest.is_empty() {
                    let mut tail = [0u8; 8];
                    tail[..rest.len()].copy_from_slice(rest);
                    hash ^= u64::from_le_bytes(tail);
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
            };
            mix(&(self.entries.len() as u64).to_le_bytes());
            for (index, entry) in self.entries.iter().enumerate() {
                if let Some(frn) = self.frns.get(index) {
                    mix(&frn.to_le_bytes());
                }
                mix(&entry.parent.to_le_bytes());
                mix(&entry.size.to_le_bytes());
                mix(&entry.created.to_le_bytes());
                mix(&entry.modified.to_le_bytes());
                mix(&entry.attributes.to_le_bytes());
            }
            // The names come last: they are the biggest part, and the entries
            // above say which name belongs to which entry.
            mix(self.names.as_bytes());
        }
        hash
    }

    /// The fingerprint of the table that was read back from the file.
    ///
    /// Zero means that there was no table — a table that was built in memory
    /// has no fingerprint yet.
    pub fn saved_fingerprint(&self) -> u64 {
        self.saved_fingerprint
    }

    /// Write the index to a file.
    ///
    /// The whole table of a big volume is tens of megabytes, so it is worth
    /// keeping: Everything does the same with its database, and without it
    /// every start would have to read the master file table again.
    ///
    /// The file is written to a temporary name first and then renamed, so an
    /// interrupted write cannot leave a half file behind.
    ///
    /// Returns the moment the index was written, so that the caller can
    /// write the point in the change journal with the very same number.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<u64> {
        let stamp = self.stamp();
        let fingerprint = self.fingerprint();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        {
            // The file is written in pieces, so the whole table never has to
            // be in memory a second time. On a big volume the file is over
            // half a gigabyte.
            let file = std::fs::File::create(&tmp)?;
            io_priority::make_background(&file);
            let mut out = ChunkWriter::new(file);

            out.put(CACHE_MAGIC)?;
            out.put(&CACHE_VERSION.to_le_bytes())?;
            out.put(&stamp.to_le_bytes())?;
            out.put(&fingerprint.to_le_bytes())?;
            out.put(&(self.volumes.len() as u32).to_le_bytes())?;
            for volume in &self.volumes {
                for text in [&volume.root, &volume.device, &volume.label] {
                    let bytes = text.as_bytes();
                    out.put(&(bytes.len() as u16).to_le_bytes())?;
                    out.put(bytes)?;
                }
            }
            // Where the change journal of every volume stood when the table
            // was read. The table that is read back can be brought up to date
            // from there, without reading the disk again.
            out.put(&(self.journal_points.len() as u32).to_le_bytes())?;
            for point in &self.journal_points {
                out.put(&point.to_le_bytes())?;
            }
            out.put(&(self.volume_ranges.len() as u32).to_le_bytes())?;
            for range in &self.volume_ranges {
                out.put(&range.start.to_le_bytes())?;
                out.put(&range.end.to_le_bytes())?;
                out.put(&range.volume.to_le_bytes())?;
            }
            out.put(&(self.entries.len() as u64).to_le_bytes())?;
            // The bytes of one entry, put together once.
            let mut bytes = [0u8; 36];
            for entry in &self.entries {
                bytes[0..4].copy_from_slice(&entry.name_start.to_le_bytes());
                bytes[4..8].copy_from_slice(&entry.parent.to_le_bytes());
                bytes[8..16].copy_from_slice(&entry.size.to_le_bytes());
                bytes[16..18].copy_from_slice(&entry.name_len.to_le_bytes());
                bytes[18..20].copy_from_slice(&entry.depth.to_le_bytes());
                bytes[20..24].copy_from_slice(&entry.created.to_le_bytes());
                bytes[24..28].copy_from_slice(&entry.modified.to_le_bytes());
                bytes[28..32].copy_from_slice(&entry.accessed.to_le_bytes());
                bytes[32..36].copy_from_slice(&entry.attributes.to_le_bytes());
                out.put(&bytes)?;
            }
            out.put(&(self.names.len() as u64).to_le_bytes())?;
            out.put(self.names.as_bytes())?;
            let mut bytes = [0u8; 8];
            for mask in &self.masks {
                bytes.copy_from_slice(&mask.to_le_bytes());
                out.put(&bytes)?;
            }
            for mask in &self.path_masks {
                bytes.copy_from_slice(&mask.to_le_bytes());
                out.put(&bytes)?;
            }
            out.put(&(self.pinyin.len() as u32).to_le_bytes())?;
            let mut pinyin: Vec<(&u32, &String)> = self.pinyin.iter().collect();
            pinyin.sort_unstable();
            for (index, text) in pinyin {
                out.put(&index.to_le_bytes())?;
                out.put(&(text.len() as u16).to_le_bytes())?;
                out.put(text.as_bytes())?;
            }
            // The entries that are left out.
            out.put(&(self.excluded.len() as u64).to_le_bytes())?;
            for flag in &self.excluded {
                out.put(&[u8::from(*flag)])?;
            }
            // Which entries have a known size and time.
            out.put(&(self.meta_known.len() as u64).to_le_bytes())?;
            for flag in &self.meta_known {
                out.put(&[u8::from(*flag)])?;
            }
            // The file reference number of every entry.
            //
            // The change journal talks in these numbers, so without them the
            // index that was read back from the file could not be kept up to
            // date.
            out.put(&(self.frns.len() as u64).to_le_bytes())?;
            for frn in &self.frns {
                out.put(&frn.to_le_bytes())?;
            }
            out.finish()?.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(stamp)
    }

    /// Read an index back from a file.
    ///
    /// Returns `None` when the file is missing, of another version, or
    /// damaged. A damaged file is not an error: the index is rebuilt.
    pub fn load(path: &std::path::Path) -> Option<FsIndex> {
        let file = std::fs::File::open(path).ok()?;
        let mut reader = FileReader::new(file);
        let mut magic = [0u8; 4];
        reader.read_into(&mut magic)?;
        if magic != *CACHE_MAGIC {
            return None;
        }
        let version = reader.number(4)? as u32;
        if version != CACHE_VERSION {
            return None;
        }
        let built_at = reader.number(8)?;
        let saved_fingerprint = reader.number(8)?;

        let volume_count = reader.number(4)? as usize;
        let mut volumes = Vec::with_capacity(volume_count);
        for _ in 0..volume_count {
            volumes.push([
                reader.text()?,
                reader.text()?,
                reader.text()?,
            ]);
        }

        // Where the change journal of every volume stood when the table was
        // read. Without it the table would have to be read again.
        let point_count = reader.number(4)? as usize;
        let mut journal_points = Vec::with_capacity(point_count);
        for _ in 0..point_count {
            journal_points.push(reader.number(8)?);
        }

        let range_count = reader.number(4)? as usize;
        let mut volume_ranges = Vec::with_capacity(range_count);
        for _ in 0..range_count {
            volume_ranges.push(VolumeRange {
                start: reader.number(4)? as u32,
                end: reader.number(4)? as u32,
                volume: reader.number(4)? as u32,
            });
        }

        let entry_count = reader.number(8)? as usize;
        // The entries are read in pieces, so the file never has to be in
        // memory as a whole.
        let mut entries = Vec::with_capacity(entry_count);
        let mut bytes = [0u8; 36];
        while entries.len() < entry_count {
            let take = (entry_count - entries.len()).min(4096);
            for _ in 0..take {
                reader.read_into(&mut bytes)?;
                entries.push(FsEntry {
                    name_start: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
                    parent: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                    size: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
                    name_len: u16::from_le_bytes(bytes[16..18].try_into().unwrap()),
                    depth: u16::from_le_bytes(bytes[18..20].try_into().unwrap()),
                    created: u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
                    modified: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
                    accessed: u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
                    attributes: u32::from_le_bytes(bytes[32..36].try_into().unwrap()),
                });
            }
        }

        let names_len = reader.number(8)? as usize;
        // The names are read straight into the arena of the index, so they are
        // not in memory twice.
        let mut names = String::with_capacity(names_len);
        let read = unsafe {
            let buffer = names.as_mut_vec();
            buffer.resize(names_len, 0);
            reader.read_into(buffer).is_some()
        };
        if !read {
            return None;
        }
        // The names come from the file system, so they are text. A damaged
        // file is not an error: the index is rebuilt.
        if std::str::from_utf8(names.as_bytes()).is_err() {
            let bytes = names.as_bytes().to_vec();
            names = String::from_utf8_lossy(&bytes).to_string();
        }

        let mut masks = Vec::with_capacity(entry_count);
        let mut bytes = [0u8; 8];
        while masks.len() < entry_count {
            let take = (entry_count - masks.len()).min(4096);
            for _ in 0..take {
                reader.read_into(&mut bytes)?;
                masks.push(u64::from_le_bytes(bytes));
            }
        }
        let mut path_masks = Vec::with_capacity(entry_count);
        while path_masks.len() < entry_count {
            let take = (entry_count - path_masks.len()).min(4096);
            for _ in 0..take {
                reader.read_into(&mut bytes)?;
                path_masks.push(u64::from_le_bytes(bytes));
            }
        }
        let pinyin_count = reader.number(4)? as usize;
        let mut pinyin = launcher_core::hash::fast_map_with_capacity(pinyin_count);
        for _ in 0..pinyin_count {
            let index = reader.number(4)? as u32;
            let text = reader.text()?;
            pinyin.insert(index, text);
        }
        let excluded_len = reader.number(8)? as usize;
        let mut excluded = Vec::with_capacity(excluded_len);
        while excluded.len() < excluded_len {
            let take = (excluded_len - excluded.len()).min(4096);
            let mut chunk = [0u8; 4096];
            reader.read_into(&mut chunk[..take])?;
            for flag in &chunk[..take] {
                excluded.push(*flag != 0);
            }
        }
        let known_len = reader.number(8)? as usize;
        let mut meta_known = Vec::with_capacity(known_len);
        while meta_known.len() < known_len {
            let take = (known_len - meta_known.len()).min(4096);
            let mut chunk = [0u8; 4096];
            reader.read_into(&mut chunk[..take])?;
            for flag in &chunk[..take] {
                meta_known.push(*flag != 0);
            }
        }
        // The file reference number of every entry: the change journal talks
        // in these numbers, so the index that was read back can be kept up to
        // date.
        let frn_len = reader.number(8)? as usize;
        let mut frns = Vec::with_capacity(frn_len);
        while frns.len() < frn_len {
            let take = (frn_len - frns.len()).min(4096);
            let mut chunk = [0u8; 8 * 4096];
            reader.read_into(&mut chunk[..take * 8])?;
            for number in chunk[..take * 8].chunks_exact(8) {
                frns.push(u64::from_le_bytes(number.try_into().unwrap()));
            }
        }

        let mut index = FsIndex {
            names,
            entries,
            volumes: volumes
                .into_iter()
                .map(|parts| IndexedVolume {
                    root: parts[0].clone(),
                    device: parts[1].clone(),
                    label: parts[2].clone(),
                })
                .collect(),
            volume_ranges,
            masks,
            path_masks,
            meta: launcher_core::hash::fast_map(),
            pinyin,
            built_at,
            fresh_at: built_at,
            saved_fingerprint,
            excluded,
            meta_known,
            frns,
            journal_points,
            frn_index: launcher_core::hash::fast_map(),
            frns_in_order: true,
            name_counts: std::sync::OnceLock::new(),
            children: std::sync::OnceLock::new(),
        };
        // The table was read in the order of the numbers, so they are in
        // order and a file can be found by a binary search. The check is one
        // pass, and it saves the map of nine million entries.
        index.frns_in_order = index.frns_are_in_order();
        Some(index)
    }

    /// When the index was built, in seconds since the Unix epoch.
    pub fn built_at(&self) -> u64 {
        self.built_at
    }

    /// Where the change journal of every volume stood when the table was read.
    ///
    /// The numbers belong to the volumes of the table, in the same order. A
    /// number of zero means that the point is not known for that volume.
    pub fn journal_points(&self) -> &[u64] {
        &self.journal_points
    }

    /// Say where the change journal of every volume stands now.
    pub fn set_journal_points(&mut self, points: Vec<u64>) {
        self.journal_points = points;
    }

    /// Say when the index was last brought up to date.
    ///
    /// The table that was saved is built once; the changes that came after it
    /// are applied to it when it is read back. The age of the index is the
    /// age of those changes.
    pub fn set_fresh_at(&mut self, stamp: u64) {
        self.fresh_at = stamp;
    }

    /// How old the index is.
    ///
    /// The age is the age of the **changes** that were applied to the saved
    /// table, not the age of the table itself: the table may have been built
    /// weeks ago and brought up to date every few hours since.
    pub fn age(&self) -> std::time::Duration {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let stamp = if self.fresh_at != 0 {
            self.fresh_at
        } else {
            self.built_at
        };
        std::time::Duration::from_secs(now.saturating_sub(stamp))
    }

    /// The names of the volumes the index covers.
    pub fn volume_signature(&self) -> Vec<String> {
        self.volumes
            .iter()
            .map(|v| format!("{}|{}", v.device, v.root))
            .collect()
    }

    /// How many entries share a name.
    ///
    /// It is built the first time a query asks for the duplicates.
    fn name_counts(&self) -> &HashMap<String, u32> {
        self.name_counts.get_or_init(|| {
            let mut counts: HashMap<String, u32> = HashMap::with_capacity(self.entries.len());
            for index in 0..self.entries.len() as u32 {
                *counts
                    .entry(self.name(index).to_lowercase())
                    .or_insert(0) += 1;
            }
            counts
        })
    }

    /// The children of every folder.
    ///
    /// It is built the first time a query asks for the children.
    fn child_map(&self) -> &HashMap<u32, Vec<u32>> {
        self.children.get_or_init(|| {
            let mut map: HashMap<u32, Vec<u32>> = HashMap::new();
            for (index, entry) in self.entries.iter().enumerate() {
                map.entry(entry.parent).or_default().push(index as u32);
            }
            map
        })
    }

    /// Bring the index up to date with the changes of the change journal.
    ///
    /// This is what keeps the index alive while the program runs
    /// (`_db_monitor_ntfs_process_usn_records` in Everything):
    ///
    /// * a file that was created is added,
    /// * a file that was deleted is taken out,
    /// * a file that was renamed gets its new name.
    ///
    /// Returns how many entries changed.
    pub fn apply_changes(&mut self, changes: &[usn::JournalChange]) -> usize {
        self.apply_changes_collecting(changes, &mut Vec::new())
    }

    /// Bring the index up to date with the changes of the change journal,
    /// and say **which of the changes actually changed the table**.
    ///
    /// Only those are worth writing down. A program that rewrites the same
    /// file a thousand times leaves a thousand records in the journal, and
    /// **none of them changes the table** — the file is there, with the same
    /// name, in the same folder, and the journal does not even carry its
    /// size and its time.
    ///
    /// Writing them down anyway would make the changes file a hundred times
    /// bigger than it has to be — and it is written again from the beginning
    /// every time, so the cost is paid over and over.
    pub fn apply_changes_collecting(
        &mut self,
        changes: &[usn::JournalChange],
        important: &mut Vec<IndexChange>,
    ) -> usize {
        if changes.is_empty() {
            return 0;
        }
        if !self.frns_in_order {
            self.ensure_frn_index();
        }
        let mut touched = 0usize;
        for change in changes {
            let existing = self.entry_of_frn(change.frn);
            match (change.reason & usn::reason::FILE_CREATE != 0, existing) {
                (true, None) => {
                    self.add_entry(change);
                    important.push(journal_change(change));
                    touched += 1;
                }
                (false, Some(index)) => {
                    if change.reason & usn::reason::FILE_DELETE != 0 {
                        self.remove_entry(index);
                        important.push(journal_change(change));
                        touched += 1;
                    } else if change.reason & usn::reason::RENAME_NEW_NAME != 0 {
                        self.rename_entry(index, &change.name);
                        important.push(journal_change(change));
                        touched += 1;
                    }
                }
                (true, Some(_)) => {}
                (false, None) => {}
            }
        }
        touched
    }

    /// The changes that turn `older` into this index.
    ///
    /// Both indexes hold the entries of the master file table in the order of
    /// their record numbers, so the two are walked side by side: the walk
    /// costs one pass and no map of nine million entries.
    ///
    /// The result is what is written between two full tables: the whole table
    /// is three quarters of a gigabyte, and the changes between two of them
    /// are a few megabytes at most.
    ///
    /// The walk stops after `limit` changes. A table that was read back
    /// wrongly (a different volume, a different exclusion) would otherwise
    /// say that **every** entry changed — and nine million changes are nine
    /// hundred megabytes of memory. When the limit is reached, the caller
    /// writes the whole table again, which is the right answer for a table
    /// that really did change completely.
    pub fn changes_since(&self, older: &FsIndex) -> Vec<IndexChange> {
        self.changes_since_limited(older, 200_000).0
    }

    /// The changes that turn `older` into this index, up to a limit.
    ///
    /// Returns the changes and whether the limit was reached.
    pub fn changes_since_limited(
        &self,
        older: &FsIndex,
        limit: usize,
    ) -> (Vec<IndexChange>, bool) {
        let mut changes = Vec::new();
        // The two tables are compared by the **number of the record**, never
        // by the place: a table that was read back can have its entries in
        // another order than a table that was just read (the files that the
        // change journal brought in are put at the end), and comparing them
        // by their places would say that *everything* changed — and then the
        // whole table would be written again for nothing.
        let old_order = older.sorted_by_number();
        let new_order = self.sorted_by_number();
        // The files that are new, and the files that changed.
        for index in new_order.iter().copied() {
            let number = self.frns[index as usize] & RECORD_NUMBER_MASK;
            let old = older.index_of_number(&old_order, number);
            let changed = match old {
                Some(old) => self.entry_differs_from(older, old as usize, index as usize),
                None => true,
            };
            if changed {
                changes.push(self.change_of(index as usize, false));
                if changes.len() > limit {
                    return (changes, true);
                }
            }
        }
        // The files that are gone.
        for index in old_order.iter().copied() {
            if older.is_gone(index as usize) {
                continue;
            }
            let number = older.frns[index as usize] & RECORD_NUMBER_MASK;
            if self.index_of_number(&new_order, number).is_none() {
                changes.push(older.change_of(index as usize, true));
                if changes.len() > limit {
                    return (changes, true);
                }
            }
        }
        // The files that the change journal brought in are at the end of a
        // table and are not in the order of the record numbers, so they are
        // looked at one by one.
        for index in self.mft_end()..self.entries.len() {
            let number = self.frns[index] & RECORD_NUMBER_MASK;
            let changed = match older.entry_of_frn(number) {
                Some(old) => self.entry_differs_from(older, old as usize, index),
                None => true,
            };
            if changed {
                changes.push(self.change_of(index, false));
                if changes.len() > limit {
                    return (changes, true);
                }
            }
        }
        for index in older.mft_end()..older.entries.len() {
            if older.is_gone(index) {
                continue;
            }
            let number = older.frns[index] & RECORD_NUMBER_MASK;
            // The file is still there — the change was already reported by the
            // loop above, which looks every file of the new table up in the old
            // one. Only the files that are really gone are reported here.
            if self.entry_of_frn(number).is_none() {
                changes.push(older.change_of(index, true));
                if changes.len() > limit {
                    return (changes, true);
                }
            }
        }
        (changes, false)
    }

    /// The entries of the master file table, in the order of their record
    /// numbers.
    ///
    /// The table is read in that order, so the list is usually already in it
    /// and nothing has to be done.
    fn sorted_by_number(&self) -> Vec<u32> {
        let mut order: Vec<u32> = (0..self.mft_end() as u32).collect();
        let mut sorted = true;
        for pair in order.windows(2) {
            let left = self.frns[pair[0] as usize] & RECORD_NUMBER_MASK;
            let right = self.frns[pair[1] as usize] & RECORD_NUMBER_MASK;
            if left >= right {
                sorted = false;
                break;
            }
        }
        if !sorted {
            order.sort_unstable_by_key(|index| self.frns[*index as usize] & RECORD_NUMBER_MASK);
        }
        order
    }

    /// The entry with the given record number, in a list that is sorted by it.
    fn index_of_number(&self, order: &[u32], number: u64) -> Option<u32> {
        order
            .binary_search_by(|index| {
                (self.frns[*index as usize] & RECORD_NUMBER_MASK).cmp(&number)
            })
            .ok()
            .map(|at| order[at])
    }

    /// Whether the entry is one that is not there any more.
    ///
    /// A file that was deleted is left in the table with a mark on it, so that
    /// the other entries keep their places. Such a file is not reported as a
    /// change again — it was reported when it was deleted.
    fn is_gone(&self, index: usize) -> bool {
        self.excluded.get(index).copied().unwrap_or(false)
    }

    /// How many entries of the index came from the master file table.
    ///
    /// They are the first ones, and they are in the order of the record
    /// numbers. The files that the change journal brought in follow them.
    fn mft_end(&self) -> usize {
        self.volume_ranges
            .iter()
            .map(|range| range.end as usize)
            .max()
            .unwrap_or(0)
            .min(self.frns.len())
    }

    /// The change that turns the entry `index` into something else.
    ///
    /// With `deleted` the file is gone, without it the file has these values.
    fn change_of(&self, index: usize, deleted: bool) -> IndexChange {
        let entry = &self.entries[index];
        IndexChange {
            frn: self.frns[index],
            parent_frn: self.parent_frn_of(index),
            name: self.name(index as u32).to_string(),
            deleted,
            size: entry.size,
            created: entry.created,
            modified: entry.modified,
            accessed: entry.accessed,
            attributes: entry.attributes,
            meta_known: true,
        }
    }

    /// The file reference number of the folder that holds an entry.
    fn parent_frn_of(&self, index: usize) -> u64 {
        let parent = self.entries[index].parent;
        if parent == NO_PARENT {
            return 0;
        }
        self.frns
            .get(parent as usize)
            .copied()
            .unwrap_or(0)
    }

    /// Whether the entry `older` is another file than the entry `new`.
    fn entry_differs_from(&self, older: &FsIndex, older_index: usize, new_index: usize) -> bool {
        let old_frn = older.frns[older_index];
        let new_frn = self.frns[new_index];
        // A different sequence means that the record was used again: it is a
        // different file now.
        if (old_frn >> 48) != (new_frn >> 48) {
            return true;
        }
        let old_entry = &older.entries[older_index];
        let new_entry = &self.entries[new_index];
        if older.name(older_index as u32) != self.name(new_index as u32) {
            return true;
        }
        // A file that was moved to another folder keeps its name and its
        // size, so the folder it is in has to be looked at as well.
        if older.parent_frn_of(older_index) != self.parent_frn_of(new_index) {
            return true;
        }
        old_entry.size != new_entry.size
            || old_entry.created != new_entry.created
            || old_entry.modified != new_entry.modified
            || old_entry.attributes != new_entry.attributes
    }

    /// Bring the index up to date with the changes that were written down.
    ///
    /// This is the counterpart of [`FsIndex::changes_since`]: the changes
    /// that are read back are applied to the table that was saved, and the
    /// result is the table as it was when the changes were written.
    pub fn apply_index_changes(&mut self, changes: &[IndexChange]) -> usize {
        if changes.is_empty() {
            return 0;
        }
        if !self.frns_in_order {
            self.ensure_frn_index();
        }
        let mut touched = 0usize;
        for change in changes {
            let existing = self.entry_of_frn(change.frn);
            match (change.deleted, existing) {
                (true, Some(index)) => {
                    self.remove_entry(index);
                    touched += 1;
                }
                (true, None) => {}
                (false, Some(index)) => {
                    if self.name(index) != change.name {
                        self.rename_entry(index, &change.name);
                    }
                    // The record may have been used again since the table was
                    // written: the number of the record is the same, but the
                    // sequence is a new one — and the sequence is what tells
                    // the two files apart.
                    self.frns[index as usize] = change.frn;
                    self.frn_index.insert(change.frn, index);
                    // The file is there again: it was deleted before and the
                    // record was used again. The mark of the deletion has to
                    // go, or the file would stay invisible for ever.
                    self.excluded[index as usize] = false;
                    if change.meta_known {
                        let entry = &mut self.entries[index as usize];
                        entry.size = change.size;
                        entry.created = change.created;
                        entry.modified = change.modified;
                        entry.accessed = change.accessed;
                        entry.attributes = change.attributes;
                    }
                    self.refresh_entry(index);
                    touched += 1;
                }
                (false, None) => {
                    self.add_index_entry(change);
                    touched += 1;
                }
            }
        }
        touched
    }

    /// Add a file that is not in the index yet.
    fn add_index_entry(&mut self, change: &IndexChange) {
        // The folder has to be there; when it is not, the entry is dropped,
        // because its place in the tree would be unknown.
        let parent = self
            .entry_of_frn(change.parent_frn)
            .unwrap_or(NO_PARENT);
        let name_start = self.names.len() as u32;
        self.names.push_str(&change.name);
        let index = self.entries.len() as u32;
        self.entries.push(FsEntry {
            name_start,
            name_len: change.name.len().min(u16::MAX as usize) as u16,
            parent,
            attributes: change.attributes,
            depth: 0,
            size: change.size,
            created: change.created,
            modified: change.modified,
            accessed: change.accessed,
        });
        self.excluded.push(false);
        self.meta_known.push(false);
        self.frns.push(change.frn);
        self.frn_index.insert(change.frn, index);
        self.masks.push(0);
        self.path_masks.push(0);
        self.refresh_entry(index);
    }

    /// Write the changes since the saved table to a file.
    ///
    /// The number of the saved table comes with them, so that changes that
    /// belong to another table are not used.
    pub fn save_changes(
        &self,
        path: &std::path::Path,
        base_stamp: u64,
        changes: &[IndexChange],
    ) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let stamp = self.stamp();
        let tmp = path.with_extension("tmp");
        {
            let file = std::fs::File::create(&tmp)?;
            io_priority::make_background(&file);
            let mut out = ChunkWriter::new(file);
            out.put(DELTA_MAGIC)?;
            out.put(&DELTA_VERSION.to_le_bytes())?;
            out.put(&base_stamp.to_le_bytes())?;
            out.put(&stamp.to_le_bytes())?;
            out.put(&(changes.len() as u64).to_le_bytes())?;
            for change in changes {
                out.put(&change.frn.to_le_bytes())?;
                out.put(&change.parent_frn.to_le_bytes())?;
                out.put(&change.size.to_le_bytes())?;
                out.put(&change.created.to_le_bytes())?;
                out.put(&change.modified.to_le_bytes())?;
                out.put(&change.accessed.to_le_bytes())?;
                out.put(&change.attributes.to_le_bytes())?;
                out.put(&[u8::from(change.deleted)])?;
                out.put(&[u8::from(change.meta_known)])?;
                let name = change.name.as_bytes();
                out.put(&(name.len() as u16).to_le_bytes())?;
                out.put(name)?;
            }
            out.finish()?.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Read the changes since the saved table back.
    ///
    /// Returns `None` when the file is missing, of another version, or does
    /// not belong to the table that was read. The number that comes with the
    /// changes is the moment they were written.
    pub fn load_changes(path: &std::path::Path, base_stamp: u64) -> Option<(u64, Vec<IndexChange>)> {
        let file = std::fs::File::open(path).ok()?;
        let mut reader = FileReader::new(file);
        let mut magic = [0u8; 4];
        reader.read_into(&mut magic)?;
        if magic != *DELTA_MAGIC {
            return None;
        }
        let version = reader.number(4)? as u32;
        if version != DELTA_VERSION {
            return None;
        }
        let stamp = reader.number(8)?;
        if stamp != base_stamp {
            // The changes belong to another table, so they are of no use.
            return None;
        }
        let written_at = reader.number(8)?;
        let count = reader.number(8)? as usize;
        let mut changes = Vec::with_capacity(count.min(1 << 20));
        for _ in 0..count {
            let frn = reader.number(8)?;
            let parent_frn = reader.number(8)?;
            let size = reader.number(8)?;
            let created = reader.number(4)? as u32;
            let modified = reader.number(4)? as u32;
            let accessed = reader.number(4)? as u32;
            let attributes = reader.number(4)? as u32;
            let deleted = reader.number(1)? != 0;
            let meta_known = reader.number(1)? != 0;
            let name = reader.text()?;
            changes.push(IndexChange {
                frn,
                parent_frn,
                name,
                deleted,
                size,
                created,
                modified,
                accessed,
                attributes,
                meta_known,
            });
        }
        Some((written_at, changes))
    }

    /// The entry of a file reference number.
    ///
    /// The number of a record is 64 bits, but the high 16 of them are the
    /// **sequence** of the record — it grows every time the record is used
    /// again. The low 48 bits are the number of the record itself.
    ///
    /// **The whole 64 bits are the name of the file.** A file that is deleted
    /// and created again keeps its record number and gets a new sequence —
    /// and then it is a different file. Looking only at the low half would
    /// find the old file and take the new one for it, and the new file would
    /// never show up.
    fn entry_of_frn(&self, frn: u64) -> Option<u32> {
        let number = frn & RECORD_NUMBER_MASK;
        if !self.frns_in_order {
            return self.frn_index.get(&frn).copied();
        }
        // The files that the change journal brought in are few, and they are
        // in the map.
        if let Some(index) = self.frn_index.get(&frn) {
            return Some(*index);
        }
        // The files of the master file table are found by a binary search:
        // their numbers are in order, one volume after the other.
        for range in &self.volume_ranges {
            let start = range.start as usize;
            let end = (range.end as usize).min(self.frns.len());
            if start >= end {
                continue;
            }
            let found = self.frns[start..end]
                .binary_search_by(|probe| (probe & RECORD_NUMBER_MASK).cmp(&number));
            if let Ok(position) = found {
                let index = start + position;
                // The record may have been used again since the table was
                // read: the number is the same, but the sequence is a new one
                // — and then it is another file.
                if self.frns[index] == frn {
                    return Some(index as u32);
                }
                return None;
            }
        }
        None
    }

    /// The map from the file reference number to the entry, built on demand.
    ///
    /// It is only needed when the numbers are **not** in order — then the
    /// binary search cannot be used and every entry has to be looked up.
    fn ensure_frn_index(&mut self) {
        if !self.frn_index.is_empty() {
            return;
        }
        let mut map = launcher_core::hash::fast_map_with_capacity(self.entries.len());
        for (index, frn) in self.frns.iter().enumerate() {
            map.insert(*frn, index as u32);
        }
        self.frn_index = map;
    }

    /// Whether the file reference numbers are in order, so that a file can be
    /// found by a binary search.
    ///
    /// The master file table is read in the order of the numbers, so they
    /// are. The check is one pass over the numbers, and it saves the two
    /// hundred megabytes of memory that the map of nine million entries
    /// would take.
    pub fn frns_are_in_order(&self) -> bool {
        for range in &self.volume_ranges {
            let start = range.start as usize;
            let end = (range.end as usize).min(self.frns.len());
            for index in start..end.saturating_sub(1) {
                if (self.frns[index] & RECORD_NUMBER_MASK) >= (self.frns[index + 1] & RECORD_NUMBER_MASK)
                {
                    return false;
                }
            }
        }
        true
    }

    /// Add a file that was created.
    fn add_entry(&mut self, change: &usn::JournalChange) {
        // The folder has to be there; when it is not, the entry is dropped,
        // because its place in the tree would be unknown.
        let parent = self
            .entry_of_frn(change.parent_frn)
            .unwrap_or(NO_PARENT);
        let name_start = self.names.len() as u32;
        self.names.push_str(&change.name);
        let index = self.entries.len() as u32;
        self.entries.push(FsEntry {
            name_start,
            name_len: change.name.len().min(u16::MAX as usize) as u16,
            parent,
            attributes: change.attributes,
            depth: 0,
            size: 0,
            created: 0,
            modified: 0,
            accessed: 0,
        });
        self.excluded.push(false);
        self.meta_known.push(false);
        self.frns.push(change.frn);
        self.frn_index.insert(change.frn, index);
        self.masks.push(0);
        self.path_masks.push(0);
        self.refresh_entry(index);
    }

    /// Take a file that was deleted out of the index.
    fn remove_entry(&mut self, index: u32) {
        self.excluded[index as usize] = true;
        self.frn_index.remove(&self.frns[index as usize]);
    }

    /// Give a file its new name.
    fn rename_entry(&mut self, index: u32, name: &str) {
        let name_start = self.names.len() as u32;
        self.names.push_str(name);
        let entry = &mut self.entries[index as usize];
        entry.name_start = name_start;
        entry.name_len = name.len().min(u16::MAX as usize) as u16;
        self.refresh_entry(index);
    }

    /// Recompute what depends on one entry: the depth, the filter masks and
    /// the pinyin.
    fn refresh_entry(&mut self, index: u32) {
        // The depth.
        let mut depth = 0u16;
        let mut current = self.entries[index as usize].parent;
        let mut guard = 0usize;
        while current != NO_PARENT && guard < 256 {
            depth = depth.saturating_add(1);
            current = self.entries[current as usize].parent;
            guard += 1;
        }
        self.entries[index as usize].depth = depth;

        // The pinyin.
        let name = self.name(index).to_string();
        self.pinyin.remove(&index);
        if name.chars().any(is_cjk) {
            if let Some(pinyin) = launcher_core::pinyin::PinyinIndex::build(&name) {
                let rendered = pinyin.pinyin_lower().to_string();
                if !rendered.is_empty() {
                    self.pinyin.insert(index, rendered);
                }
            }
        }

        // The filter masks.
        let mut own = name_mask(&name);
        if let Some(pinyin) = self.pinyin.get(&index) {
            own |= name_mask(pinyin);
        }
        let parent = self.entries[index as usize].parent;
        let parent_mask = if parent == NO_PARENT {
            0
        } else {
            self.masks[parent as usize]
        };
        self.masks[index as usize] = own | parent_mask;

        let mut path_mask = own;
        let mut current = parent;
        let mut guard = 0usize;
        while current != NO_PARENT && guard < 256 {
            path_mask |= self.masks[current as usize];
            current = self.entries[current as usize].parent;
            guard += 1;
        }
        self.path_masks[index as usize] = path_mask;
    }

    /// The characters every plain term of the query requires.
    ///
    /// A term can only match a name that has all of them, so the filter of
    /// a folder tells whether one of the terms could match it.
    fn term_masks(&self, parsed: &search_syntax::ParsedSearch) -> Vec<u64> {
        let wildcards = parsed.options.wildcards;
        search_syntax::terms(&parsed.node)
            .into_iter()
            .map(|term| term_mask(term, wildcards))
            .filter(|mask| *mask != 0)
            .collect()
    }

    /// Whether the folder of an entry could match one of the terms.
    ///
    /// The filter of the folder holds the characters of its name, so if one
    /// of the terms needs a character the folder does not have, that term
    /// cannot match it — and the name of the folder does not have to be
    /// looked up at all.
    fn folder_can_match(&self, index: u32, term_masks: &[u64], filters: &[u64]) -> bool {
        if term_masks.is_empty() {
            return true;
        }
        let parent = self.entries[index as usize].parent;
        if parent == NO_PARENT {
            return false;
        }
        let folder = filters[parent as usize];
        term_masks
            .iter()
            .any(|mask| folder & mask == *mask)
    }

    /// Search the index.
    ///
    /// The query uses the Everything syntax, see
    /// [`launcher_core::search_syntax`]. The results are ranked by
    /// relevance rather than by name.
    pub fn search(&self, query: &str, limit: usize) -> Vec<FsHit> {
        self.search_with(query, limit, None, FileSort::default())
    }

    /// How many entries would survive the character filter of the query.
    ///
    /// This is the size of the set that the real evaluation has to walk
    /// through, so it says how much work a query is: a single letter passes
    /// millions of entries, a word with rare letters passes a handful.
    ///
    /// It is a diagnostic: the count is taken by walking the table of the
    /// filters once, which is the same work the search does.
    pub fn candidate_count(&self, query: &str) -> usize {
        let parsed = ParsedSearch::parse(query);
        if parsed.is_empty() {
            return 0;
        }
        let Some(mask) = required_mask(&parsed.node, parsed.options.wildcards) else {
            return 0;
        };
        if mask == 0 {
            return 0;
        }
        let filters = if parsed.options.match_path {
            &self.path_masks
        } else {
            &self.masks
        };
        filters
            .par_iter()
            .zip(self.excluded.par_iter())
            .filter(|(entry_mask, excluded)| !**excluded && **entry_mask & mask == mask)
            .count()
    }

    /// How many entries have a different mask in the two tables.
    ///
    /// The mask of an entry holds the characters of its own name **and** of
    /// the names above it, so the two tables should hold the same thing. If
    /// they do, one of them is a copy of the other and can go.
    ///
    /// It is a diagnostic.
    pub fn mask_divergence(&self) -> usize {
        self.masks
            .iter()
            .zip(self.path_masks.iter())
            .filter(|(own, path)| own != path)
            .count()
    }

    /// Search the index, fetching metadata for the candidates that need it.
    ///
    /// The provider is only called when the query filters on the size or on a
    /// timestamp, and only for the candidates that survived the character
    /// filter. That keeps the cost of `size:>1mb report` down to the handful
    /// of files whose name matches.
    pub fn search_with(
        &self,
        query: &str,
        limit: usize,
        metadata: Option<&(dyn Fn(u32) -> Option<FileMeta> + Sync)>,
        sort: FileSort,
    ) -> Vec<FsHit> {
        self.search_with_cancel(query, limit, metadata, sort, None)
    }

    /// Search the index, and stop early when the query goes out of date.
    ///
    /// One keystroke scans the whole table, and the next keystroke usually
    /// comes a few hundred milliseconds later. The result of the older query
    /// is of no use to anybody by then, so it stops as soon as it sees that a
    /// newer query has started.
    ///
    /// The token and the generation are the ones the query was started with.
    pub fn search_with_cancel(
        &self,
        query: &str,
        limit: usize,
        metadata: Option<&(dyn Fn(u32) -> Option<FileMeta> + Sync)>,
        sort: FileSort,
        cancel: Option<(&Cancel, u64)>,
    ) -> Vec<FsHit> {
        let parsed = {
            let _timer = launcher_core::profile::phase("index.fs.parse");
            ParsedSearch::parse(query)
        };
        if parsed.is_empty() {
            return Vec::new();
        }
        let term = search_syntax::primary_term(&parsed.node).cloned();
        // Sorting by the size or by the time needs the metadata as well.
        let wants_metadata = search_syntax::needs_metadata(&parsed.node)
            || matches!(sort, FileSort::Size | FileSort::Modified);
        // The duplicates and the children cannot be answered by looking at one
        // entry alone, so the index does the work itself.
        let duplicates = search_syntax::function_values(&parsed.node, "dupe");
        let children = search_syntax::function_values(&parsed.node, "child");

        // ---- the cheap filter ------------------------------------------------
        //
        // The characters a name has to have for the query to match. A query
        // that matches the whole path is checked against the filter that
        // holds the characters of every folder on the way down.
        let filter = required_mask(&parsed.node, parsed.options.wildcards);
        let filters = if parsed.options.match_path {
            &self.path_masks
        } else {
            &self.masks
        };

        // ---- the real evaluation ---------------------------------------------
        //
        // Only a score per candidate is kept here; the names and paths are
        // built for the few that survive, so the memory does not grow with
        // the size of the index.
        let options = parsed.options;
        // The characters every term needs. They tell whether the folder of a
        // candidate could match, and looking up the name of the folder is one
        // of the more expensive things a candidate costs.
        let term_masks = self.term_masks(&parsed);
        let score_of = |index: u32| -> Option<(i32, u32, u16)> {
            let name = self.name(index);
            let target = IndexTarget {
                index: self,
                entry: index,
                name,
                attributes: self.entries[index as usize].attributes,
                depth: self.depth(index),
                // Only a name with Chinese characters in it has a pinyin
                // rendering, and such a name is never plain ASCII. The check
                // keeps the lookup in the table of the pinyin renderings away
                // from the other candidates.
                alias: if name.is_ascii() {
                    None
                } else {
                    self.pinyin.get(&index).map(|text| text.as_str())
                },
                term_masks: &term_masks,
                filters,
                meta: if wants_metadata {
                    // The size and the times come from the table when it was
                    // read straight off the disk; only the entries from the
                    // change journal have to be looked up.
                    let known = self.meta.get(&index).copied().or_else(|| {
                        if self.meta_known.get(index as usize).copied().unwrap_or(false) {
                            self.entries[index as usize].meta()
                        } else {
                            None
                        }
                    });
                    known.or_else(|| metadata.and_then(|fetch| fetch(index)))
                } else {
                    None
                },
            };
            // The match and the score come out of the same walk over the
            // name, so a candidate is only looked at once.
            let quality = search_syntax::evaluate_scored(&parsed.node, &target, &options)?;

            // The duplicates: the name has to occur more than once.
            if !duplicates.is_empty() {
                let counts = self.name_counts();
                let name = target.name().to_lowercase();
                let shared = counts.get(&name).copied().unwrap_or(0);
                if shared < 2 {
                    return None;
                }
                // A value narrows the search down to a certain name.
                if let Some(wanted) = duplicates.first() {
                    if !wanted.is_empty()
                        && !search_syntax::contains_fold(&name, &wanted.to_lowercase())
                    {
                        return None;
                    }
                }
            }
            // The children: the folder has to hold a child with that name.
            if !children.is_empty() {
                if !target.is_dir() {
                    return None;
                }
                let map = self.child_map();
                let found = map.get(&index).map(|list| {
                    list.iter().any(|child| {
                        let name = self.name(*child).to_lowercase();
                        children.iter().any(|wanted| {
                            wanted.is_empty()
                                || search_syntax::contains_fold(&name, &wanted.to_lowercase())
                        })
                    })
                });
                if found != Some(true) {
                    return None;
                }
            }
            // Shorter names are more likely to be what the user meant.
            let bonus = (200 - (target.name().len() as i32)).clamp(0, 200);
            let score = if term.is_some() {
                quality.min(search_syntax::SCORE_EXACT) + bonus
            } else {
                bonus
            };
            Some((score, index, self.entries[index as usize].name_len))
        };

        // The ranking of the candidates.
        //
        // The length of the name is carried along, so that the comparison
        // does not have to look at the entry: with a million candidates that
        // would be a million random accesses into a table of hundreds of
        // megabytes.

        // The comparison of the order the user asked for.
        //
        // The orders by size and by time need the metadata of the entry, and
        // looking it up in the table of the metadata for **every comparison**
        // is hundreds of millions of lookups with a broad query. So the key
        // of every candidate is worked out once, and the comparison is done
        // on the numbers.
        let by_sort = |a: &(i32, u32, u16), b: &(i32, u32, u16)| -> std::cmp::Ordering {
            match sort {
                FileSort::Relevance => b
                    .0
                    .cmp(&a.0)
                    .then_with(|| b.2.cmp(&a.2))
                    .then_with(|| self.name(a.1).cmp(self.name(b.1))),
                FileSort::Name => self.name(a.1).cmp(self.name(b.1)),
                FileSort::Path => self.path(a.1).cmp(&self.path(b.1)),
                FileSort::Size => {
                    let size = |index: u32| {
                        self.meta
                            .get(&index)
                            .map(|m| m.size)
                            .unwrap_or_else(|| self.entries[index as usize].size)
                    };
                    size(b.1)
                        .cmp(&size(a.1))
                        .then_with(|| self.name(a.1).cmp(self.name(b.1)))
                }
                FileSort::Modified => {
                    let modified = |index: u32| {
                        self.meta
                            .get(&index)
                            .map(|m| m.modified)
                            .unwrap_or_else(|| self.entries[index as usize].modified as u64)
                    };
                    modified(b.1)
                        .cmp(&modified(a.1))
                        .then_with(|| self.name(a.1).cmp(self.name(b.1)))
                }
            }
        };

        // How many candidates are kept while the scan runs.
        //
        // The floor matters: with a small limit the list would be cut down
        // every few candidates, and the cutting down is more work than the
        // scan itself.
        let cap = limit.saturating_mul(4).max(256);
        // Adding one candidate to the best few.
        let keep_best = |mut best: Vec<(i32, u32, u16)>, item: (i32, u32, u16)| {
            best.push(item);
            // The list is cut down when it has grown a few times over the
            // size that is needed. Cutting it down costs a sort, so it is
            // done as rarely as the memory allows.
            if best.len() > cap * 8 {
                best.select_nth_unstable_by(cap, &by_sort);
                best.truncate(cap);
            }
            best
        };
        // Putting two lists of the best few together.
        let merge_best = |mut left: Vec<(i32, u32, u16)>, right: Vec<(i32, u32, u16)>| {
            left.extend(right);
            // The order puts the best first, so the ones before the place
            // that is picked are the best few.
            if left.len() > cap {
                left.select_nth_unstable_by(cap, &by_sort);
            }
            left.truncate(cap);
            left
        };

        // The filter and the evaluation run in one pass, so the list of
        // candidates never exists as a separate allocation.
        //
        // When the order is the relevance, only the best few are kept: the
        // list would otherwise grow with the number of hits, and a broad
        // query has millions of them.
        //
        // The environment variable `LAUNCHER_ALL_CANDIDATES` turns that off,
        // so that the two ways can be measured against each other in one run.
        // 看看有没有新的查询来了。
        //
        // 放在**候选**这一步,而不是每一条:一条要过九百万,候选只有几百万 ——
        // 差三倍,而放弃得一样快(过滤那一遍本来就跑不掉)。
        //
        // 但是「放弃」真的要放弃:以前只是把**那一条**候选扔掉,后面那几千条
        // 照样一个一个去匹配 —— 也就是说,一次被取代的按键仍然把剩下的候选
        // 全部匹配完,再把结果扔掉。现在过期了就记下来,后面的候选一个也不
        // 再匹配。
        let aborted = std::sync::atomic::AtomicBool::new(false);
        let stale = |index: usize| -> bool {
            if index & 1023 != 0 {
                return false;
            }
            let out = match cancel {
                Some((token, generation)) => token.is_stale(generation),
                None => false,
            };
            if out {
                aborted.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            out
        };
        let keep_all = std::env::var_os("LAUNCHER_ALL_CANDIDATES").is_some();
        let mut scored: Vec<(i32, u32, u16)> = {
            let _timer = launcher_core::profile::phase("index.fs.scan");
            match filter {
                Some(mask) if mask != 0 => {
                    let stream = filters
                        .par_iter()
                        .zip(self.excluded.par_iter())
                        .enumerate()
                        // The work is shared out in pieces of a decent size, so
                        // that the number of pieces stays small. Every piece has
                        // a list of the best few of its own, and the lists are put
                        // together at the end.
                        .with_min_len(16_384)
                        .filter(|(_, (entry_mask, excluded))| {
                            !**excluded && **entry_mask & mask == mask
                        })
                        .filter_map(|(index, _)| {
                            if aborted.load(std::sync::atomic::Ordering::Relaxed) {
                                return None;
                            }
                            if stale(index) {
                                return None;
                            }
                            score_of(index as u32)
                        });
                    // Every order keeps only the best few of every piece, so
                    // the list of the candidates never grows with the number of
                    // hits — a broad query has millions of them.
                    if !keep_all {
                        stream
                            .fold(|| Vec::<(i32, u32, u16)>::new(), keep_best)
                            .reduce(|| Vec::<(i32, u32, u16)>::new(), merge_best)
                    } else {
                        stream.collect()
                    }
                }
                _ => {
                    let stream = (0..self.entries.len() as u32)
                        .into_par_iter()
                        .with_min_len(16_384)
                        .filter(|index| !self.excluded[*index as usize])
                        .filter_map(|index| {
                            if aborted.load(std::sync::atomic::Ordering::Relaxed) {
                                return None;
                            }
                            if stale(index as usize) {
                                return None;
                            }
                            score_of(index)
                        });
                    // Every order keeps only the best few of every piece, so
                    // the list of the candidates never grows with the number of
                    // hits — a broad query has millions of them.
                    if !keep_all {
                        stream
                            .fold(|| Vec::<(i32, u32, u16)>::new(), keep_best)
                            .reduce(|| Vec::<(i32, u32, u16)>::new(), merge_best)
                    } else {
                        stream.collect()
                    }
                }
            }
        };

        // ---- keep the best few -----------------------------------------------
        //
        // The list is already short: the scan kept only the best few of
        // every piece, so sorting them is cheap.
        {
            let _timer = launcher_core::profile::phase("index.fs.sort");
            scored.sort_unstable_by(&by_sort);
            scored.truncate(limit);
        }

        // The names and the paths are built for the few that survived — the
        // path is put back together by walking up the tree, which is not
        // cheap, so it is done as late as possible.
        let _timer = launcher_core::profile::phase("index.fs.paths");
        scored
            .into_iter()
            .map(|(score, index, _)| {
                // The size and the time of the file are **in the index**:
                // they were read from the master file table when the table
                // was built, and they are kept in the table on the disk.
                //
                // So they are free: no file has to be opened to know how
                // big it is. The entries that came from the change journal
                // do not carry them, and those are the ones the bitmap says
                // are unknown.
                let known = self.meta_known.get(index as usize).copied().unwrap_or(false);
                let entry = &self.entries[index as usize];
                let (size, modified) = if known {
                    (Some(entry.size), Some(entry.modified as u64))
                } else {
                    (None, None)
                };
                FsHit {
                    name: self.name(index).to_string(),
                    path: self.path(index),
                    is_dir: entry.is_dir(),
                    size,
                    modified,
                    score,
                }
            })
            .collect()
    }
}

/// The filter bit of a character.
///
/// The letters and the digits get a bucket each, so that the filter is as
/// sharp as it can be: a query for `report` then really asks for the letters
/// r, e, p, o and t, and not for the digits that happen to sit in the same
/// bucket. The other characters share the buckets that are left.
///
/// The bucketing makes the filter weaker than an exact set of characters,
/// which is fine: a filter may only rule out candidates that cannot match,
/// never the other way round.
fn mask_bit(c: char) -> u64 {
    // Lower case the same way the matcher does, so that the mask of a query
    // and the mask of a name agree.
    let lower = if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    };
    if lower.is_ascii() {
        return 1u64 << ascii_bit(lower as u8);
    }
    // The rest is spread over the buckets that are left.
    1u64 << (37 + (lower as u32).wrapping_sub(128) % 27)
}

/// The bucket of an ASCII character.
///
/// The letters and the digits get one each; the punctuation shares the
/// buckets that are left.
fn ascii_bit(byte: u8) -> u32 {
    let lower = byte.to_ascii_lowercase();
    match lower {
        b'a'..=b'z' => (lower - b'a') as u32,
        b'0'..=b'9' => 26 + (lower - b'0') as u32,
        _ => 37 + (lower as u32).wrapping_sub(128) % 27,
    }
}

/// The filter mask of a whole name.
///
/// The names on a real volume are almost always plain ASCII, so the bytes are
/// walked directly and the UTF-8 decoder is only used for the few names that
/// are not.
fn name_mask(name: &str) -> u64 {
    let bytes = name.as_bytes();
    let mut mask = 0u64;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < 0x80 {
            mask |= 1u64 << ascii_bit(byte);
            index += 1;
            continue;
        }
        let Some(c) = name[index..].chars().next() else {
            break;
        };
        mask |= mask_bit(c);
        index += c.len_utf8();
    }
    mask
}

/// The characters a name has to contain for the query to match.
///
/// `None` means the query does not constrain the name, so no filter may be
/// used.
fn required_mask(node: &Node, wildcards: bool) -> Option<u64> {
    match node {
        Node::Term(term) => Some(term_mask(term, wildcards)),
        Node::And(children) => {
            let mut mask = 0u64;
            for child in children {
                if let Some(child_mask) = required_mask(child, wildcards) {
                    mask |= child_mask;
                }
            }
            if mask == 0 { None } else { Some(mask) }
        }
        // An `or` can be satisfied by either branch, which no single mask can
        // express, and a `not` says nothing about the name.
        _ => None,
    }
}

/// The characters a term requires of a name.
///
/// Wildcards are left out: `*.pdf` only requires the characters of `.pdf`.
fn term_mask(term: &Term, wildcards: bool) -> u64 {
    let mut mask = 0u64;
    for c in term.lower.chars() {
        if wildcards && (c == '*' || c == '?') {
            continue;
        }
        mask |= mask_bit(c);
    }
    mask
}

/// A cursor over the bytes of an index file.
///
/// The file is read in pieces, so the whole table never has to be in memory
/// a second time. On a big volume the file is over half a gigabyte.
struct FileReader {
    inner: std::io::BufReader<std::fs::File>,
}

impl FileReader {
    /// A reader over a file.
    fn new(file: std::fs::File) -> Self {
        Self {
            inner: std::io::BufReader::with_capacity(1 << 20, file),
        }
    }

    /// Fill the buffer with exactly `count` bytes.
    fn read_into(&mut self, buffer: &mut [u8]) -> Option<()> {
        use std::io::Read;
        self.inner.read_exact(buffer).ok()
    }

    /// Read a little endian number of `size` bytes.
    fn number(&mut self, size: usize) -> Option<u64> {
        let mut buf = [0u8; 8];
        self.read_into(&mut buf[..size])?;
        Some(u64::from_le_bytes(buf))
    }

    /// Read a length prefixed string.
    fn text(&mut self) -> Option<String> {
        let length = self.number(2)? as usize;
        let mut buf = vec![0u8; length];
        self.read_into(&mut buf)?;
        Some(String::from_utf8_lossy(&buf).to_string())
    }
}

/// True for a character that the pinyin table covers.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}

/// A writer that gathers the bytes in a piece and hands them out when the
/// piece is full.
///
/// The index of a big volume is over half a gigabyte, so it is written in
/// pieces: the whole table never has to be in memory a second time.
struct ChunkWriter<W: std::io::Write> {
    out: W,
    chunk: Vec<u8>,
}

impl<W: std::io::Write> ChunkWriter<W> {
    /// A writer that gathers a megabyte at a time.
    fn new(out: W) -> Self {
        Self {
            out,
            chunk: Vec::with_capacity(1 << 20),
        }
    }

    /// Add bytes to the piece.
    fn put(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.chunk.extend_from_slice(bytes);
        if self.chunk.len() >= (1 << 20) {
            self.flush_chunk()?;
        }
        Ok(())
    }

    /// Write the piece out.
    ///
    /// Every full megabyte is followed by a short pause. The index is
    /// written in the background — the change file every now and then, the
    /// whole table when the changes have used it up — and a burst of half a
    /// gigabyte at full speed is exactly the kind of thing that makes
    /// everything else on the disk wait: the disk queue fills up, and
    /// whatever the user is looking at stutters. A pause between the pieces
    /// keeps the queue shallow; the write takes a moment longer and nobody
    /// notices, because it was in the background anyway.
    fn flush_chunk(&mut self) -> std::io::Result<()> {
        let big = self.chunk.len() >= (1 << 20);
        if !self.chunk.is_empty() {
            self.out.write_all(&self.chunk)?;
            self.chunk.clear();
        }
        if big {
            std::thread::sleep(std::time::Duration::from_millis(6));
        }
        Ok(())
    }

    /// Write the last piece out.
    fn finish(mut self) -> std::io::Result<W> {
        self.flush_chunk()?;
        Ok(self.out)
    }
}

/// The priority the disk gives to the writes of the index.
///
/// `FILE_IO_PRIORITY_HINT` set to `LOW` for the handle of the file: the
/// disk puts these writes **behind** everything else. Together with the
/// pauses between the chunks (see [`ChunkWriter`]), writing the index is
/// something the machine can do in the background without ever getting in
/// the way of the picture on the screen.
#[cfg(target_os = "windows")]
mod io_priority {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileInformationByHandle(
            handle: *mut c_void,
            class: u32,
            information: *const c_void,
            size: u32,
        ) -> i32;
    }

    /// The `FileIoPriorityInfo` class.
    const FILE_IO_PRIORITY_INFO: u32 = 9;
    /// `FILE_IO_PRIORITY_HINT_LOW`.
    const LOW: i32 = 1;

    /// Tell the disk that the writes of this file may wait.
    pub fn make_background(file: &std::fs::File) {
        use std::os::windows::io::AsRawHandle;
        let hint: i32 = LOW;
        unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FILE_IO_PRIORITY_INFO,
                &hint as *const i32 as *const c_void,
                std::mem::size_of::<i32>() as u32,
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod io_priority {
    pub fn make_background(_file: &std::fs::File) {}
}

/// The index seen through the [`SearchTarget`] interface.
///
/// The parts that the query asks for are put together once, when the
/// candidate is looked at. Asking the index for them one by one would mean
/// a random access into a table of hundreds of megabytes for every part and
/// every term.
///
/// The path is the exception: it is only needed by the predicates that talk
/// about it, and building it means walking up the tree and joining the
/// parts. So it is built when it is asked for.
struct IndexTarget<'a> {
    index: &'a FsIndex,
    entry: u32,
    name: &'a str,
    attributes: u32,
    depth: u32,
    alias: Option<&'a str>,
    meta: Option<FileMeta>,
    /// The characters every plain term of the query needs.
    ///
    /// They tell whether the folder of the candidate could match a term at
    /// all — and looking up the name of the folder is a random access into a
    /// table of hundreds of megabytes.
    term_masks: &'a [u64],
    /// The filter of every entry.
    filters: &'a [u64],
}

impl SearchTarget for IndexTarget<'_> {
    fn name(&self) -> &str {
        self.name
    }

    fn parent_name(&self) -> &str {
        // The name of the folder is only needed when the name of the entry
        // does not match. Looking it up costs a random access into a table
        // of hundreds of megabytes, so it is done as late as possible —
        // for a broad query most of the candidates never need it.
        let index: &FsIndex = self.index;
        if !index.folder_can_match(self.entry, self.term_masks, self.filters) {
            return "";
        }
        index.parent_name(self.entry)
    }

    fn path(&self) -> String {
        self.index.path(self.entry)
    }

    fn alias(&self) -> Option<&str> {
        self.alias
    }

    fn is_dir(&self) -> bool {
        self.attributes & usn::FILE_ATTRIBUTE_DIRECTORY != 0
    }

    fn size(&self) -> u64 {
        self.meta.map(|m| m.size).unwrap_or(0)
    }

    fn created(&self) -> u64 {
        self.meta.map(|m| m.created).unwrap_or(0)
    }

    fn modified(&self) -> u64 {
        self.meta.map(|m| m.modified).unwrap_or(0)
    }

    fn accessed(&self) -> u64 {
        self.meta.map(|m| m.accessed).unwrap_or(0)
    }

    fn attributes(&self) -> u32 {
        self.attributes
    }

    fn depth(&self) -> u32 {
        self.depth
    }
}

/// The index while it is being filled.
///
/// The entries are pushed in one at a time, so the flat list of them never
/// exists on its own: the memory stays at the size of the index. On a volume
/// with millions of files that is the difference between a peak of twice the
/// size of the index and a peak of the size of the index itself.
pub struct EntrySink {
    index: FsIndex,
    /// Where the index of the entry of a record is kept.
    lookup: Lookup,
    /// The entries whose folder is not known yet.
    ///
    /// A folder can have a higher number than the files in it, so it is
    /// sometimes read later than the files that belong to it.
    pending: Vec<(u32, u64)>,
    /// The volume that is being filled.
    volume: u32,
    /// Where the entries of the volume that is being filled start.
    volume_start: u32,
    /// How many entries the volume holds, when that is known.
    ///
    /// The tables are made the right size the first time, so that they do
    /// not have to grow while the entries come in.
    expected: usize,
}

/// Where the index of the entry of a record is kept.
enum Lookup {
    /// A plain array with one place per record of the table.
    ///
    /// The table of a volume is read in order, so the numbers of the records
    /// grow and the place of a record is its number. This is the quickest way
    /// to find the folder of an entry and it costs four bytes per record of
    /// the table.
    Dense { table: Vec<u32> },
    /// A map, for the entries that do not come from a table.
    ///
    /// The change journal hands out the entries in the order of the changes,
    /// and the numbers of the records are all over the place, so a plain
    /// array would be mostly empty.
    Sparse(FrnMap),
}

impl Lookup {
    /// Remember the index of the entry of a record.
    fn insert(&mut self, number: u64, index: u32) {
        match self {
            Lookup::Dense { table } => {
                let number = number as usize;
                if number >= table.len() {
                    table.resize(number + 1, NO_PARENT);
                }
                table[number] = index;
            }
            Lookup::Sparse(map) => map.insert(number, index),
        }
    }

    /// The index of the entry of a record.
    fn lookup(&self, number: u64) -> Option<u32> {
        match self {
            Lookup::Dense { table } => {
                let number = number as usize;
                table
                    .get(number)
                    .copied()
                    .filter(|&index| index != NO_PARENT)
            }
            Lookup::Sparse(map) => map.lookup(number).copied(),
        }
    }
}

impl EntrySink {
    /// An empty index for the given volumes.
    fn new(volumes: &[usn::Volume]) -> Self {
        let mut index = FsIndex::default();
        for volume in volumes {
            index.volumes.push(IndexedVolume {
                root: volume.root.clone(),
                device: volume.device.clone(),
                label: volume.label.clone(),
            });
        }
        Self {
            index,
            lookup: Lookup::Sparse(FrnMap::with_capacity(1024)),
            pending: Vec::new(),
            volume: 0,
            volume_start: 0,
            expected: 0,
        }
    }

    /// Say how many entries the volume holds, when that is known.
    ///
    /// The tables are made the right size the first time, so that they do
    /// not have to grow while the entries come in.
    pub fn reserve(&mut self, entries: usize) {
        self.expected = entries;
        self.index.entries.reserve(entries);
        self.index.masks.reserve(entries);
        self.index.path_masks.reserve(entries);
        self.index.frns.reserve(entries);
        self.index.meta_known.reserve(entries);
        self.index.excluded.reserve(entries);
    }

    /// Say where the change journal of a volume stood when it was read.
    ///
    /// The number belongs to the volume of that number, and it is what makes
    /// the difference between reading the whole disk again and reading the
    /// few hundred kilobytes of the change journal.
    pub fn set_journal_point(&mut self, volume: u32, point: u64) {
        let points = &mut self.index.journal_points;
        if points.len() <= volume as usize {
            points.resize(volume as usize + 1, 0);
        }
        points[volume as usize] = point;
    }

    /// Start the entries of a volume.
    ///
    /// The `dense` flag says whether the entries come from a table, where the
    /// numbers of the records grow and a plain array can be used.
    pub fn begin_volume(&mut self, volume: u32, dense: bool) {
        self.volume = volume;
        self.volume_start = self.index.entries.len() as u32;
        self.pending.clear();
        self.lookup = if dense {
            Lookup::Dense {
                table: Vec::new(),
            }
        } else {
            Lookup::Sparse(FrnMap::with_capacity(self.expected.max(1024)))
        };
    }

    /// Add one entry, with the name that the table gives it.
    pub fn add(&mut self, name: &str, fields: &usn::RecordFields) {
        let name_start = self.index.names.len() as u32;
        self.index.names.push_str(name);
        self.push(usn::RawEntry {
            frn: (fields.sequence as u64) << 48 | (fields.number & 0x0000_FFFF_FFFF_FFFF),
            parent_frn: fields.parent,
            name_start,
            name_len: name.len() as u32,
            attributes: if fields.is_directory {
                usn::FILE_ATTRIBUTE_DIRECTORY
            } else {
                0
            },
            // The master file table does not say why the record changed.
            reason: 0,
            usn: 0,
            size: fields.size,
            created: fields.created as u32,
            modified: fields.modified as u32,
            accessed: fields.accessed as u32,
            meta_known: true,
        });
    }

    /// Add one entry that was read somewhere else.
    ///
    /// The name is put in the arena of the index, so the place of the name in
    /// the entry is worked out here.
    pub fn add_named(&mut self, name: &str, entry: &usn::RawEntry) {
        let name_start = self.index.names.len() as u32;
        self.index.names.push_str(name);
        let mut entry = *entry;
        entry.name_start = name_start;
        entry.name_len = name.len() as u32;
        self.push(entry);
    }

    /// Add one entry that was read somewhere else.
    pub fn add_raw(&mut self, entry: &usn::RawEntry) {
        self.push(*entry);
    }

    /// Add the entry to the tree.
    fn push(&mut self, entry: usn::RawEntry) {
        // The root of a volume has an empty name; it is the anchor of the
        // tree rather than an entry of its own.
        if entry.name_len == 0 {
            return;
        }
        // The number of the record is the lower half of the file reference
        // number; the sequence number is of no use for the tree.
        let number = entry.frn & 0x0000_FFFF_FFFF_FFFF;
        let index = self.index.entries.len() as u32;
        self.lookup.insert(number, index);
        self.index.push_entry(self.volume, &entry, NO_PARENT);

        // The folder of the entry, when it is already known.
        let parent = entry.parent_frn & 0x0000_FFFF_FFFF_FFFF;
        if parent == number {
            return;
        }
        match self.lookup.lookup(parent) {
            Some(folder) => self.index.entries[index as usize].parent = folder,
            None => self.pending.push((index, parent)),
        }
    }

    /// Finish the entries of a volume.
    ///
    /// The folders that were not known yet are looked up now that the whole
    /// table has been read.
    pub fn end_volume(&mut self) {
        for (index, parent) in self.pending.drain(..) {
            let folder = match self.lookup.lookup(parent) {
                Some(folder) if folder != index => folder,
                _ => NO_PARENT,
            };
            self.index.entries[index as usize].parent = folder;
        }
        self.index.volume_ranges.push(VolumeRange {
            start: self.volume_start,
            end: self.index.entries.len() as u32,
            volume: self.volume,
        });
    }

    /// The finished index.
    pub fn finish(mut self, exclusions: &Exclusions) -> FsIndex {
        // Every volume has a place in the change journal, even when that place
        // is not known — then it is zero, and the volume has to be read again.
        let count = self.index.volumes.len();
        if self.index.journal_points.len() < count {
            self.index.journal_points.resize(count, 0);
        }
        self.index.build_pinyin();
        self.index.build_depths();
        self.index.build_masks();
        self.index.apply_exclusions(exclusions);
        // The table was read in the order of the numbers, so they are in
        // order and a file can be found by a binary search.
        self.index.frns_in_order = self.index.frns_are_in_order();
        self.index
    }
}

impl FsIndex {
    /// Prepare the index for entries that are added one at a time.
    ///
    /// The full-disk index is read volume by volume, and the entries of a
    /// volume lie next to each other. The index of the user's own folders is
    /// built while the folders are walked, and it all lies on one volume: the
    /// drive the home folder is on.
    pub fn set_single_volume(&mut self, root: &str) {
        self.volumes.clear();
        self.volume_ranges.clear();
        self.volumes.push(IndexedVolume {
            root: root.to_owned(),
            device: String::new(),
            label: String::new(),
        });
        // The range covers every entry, also the ones that are added later.
        self.volume_ranges.push(VolumeRange {
            start: 0,
            end: u32::MAX,
            volume: 0,
        });
    }

    /// Add a file or a folder that was found while the program runs.
    ///
    /// The name goes into the arena and the entry is put at the end of the
    /// tree. The character filters are worked out as well, so that a query
    /// sees the new entry right away.
    pub fn add_live(
        &mut self,
        parent: u32,
        name: &str,
        is_dir: bool,
        size: u64,
        modified: u32,
        known: bool,
    ) -> u32 {
        let name_start = self.names.len() as u32;
        self.names.push_str(name);
        let index = self.entries.len() as u32;
        let depth = if parent == NO_PARENT {
            1
        } else {
            self.entries[parent as usize].depth.saturating_add(1)
        };
        self.entries.push(FsEntry {
            name_start,
            name_len: name.len().min(u16::MAX as usize) as u16,
            parent,
            attributes: if is_dir {
                usn::FILE_ATTRIBUTE_DIRECTORY
            } else {
                0
            },
            size,
            created: 0,
            modified,
            accessed: 0,
            depth,
        });
        self.meta_known.push(known);
        self.frns.push(0);
        self.excluded.push(false);
        // The character filters: the name of the entry, the name of its
        // folder, and the names of all the folders above it.
        let own = self.own_mask_of(index);
        let parent_own = if parent == NO_PARENT {
            0
        } else {
            self.own_mask_of(parent)
        };
        let parent_path = if parent == NO_PARENT {
            0
        } else {
            self.path_masks[parent as usize]
        };
        self.masks.push(own | parent_own);
        self.path_masks.push(parent_path | own);
        index
    }

    /// The character filter of the name of one entry.
    fn own_mask_of(&self, index: u32) -> u64 {
        let name = self.name(index);
        let mut mask = name_mask(name);
        if let Some(pinyin) = self.pinyin.get(&index) {
            mask |= name_mask(pinyin);
        }
        mask
    }

    /// Mark an entry as gone.
    ///
    /// The entry stays in the tree — a tree cannot be cut in half — but it is
    /// left out of the results from now on.
    pub fn mark_excluded(&mut self, index: u32) {
        if let Some(slot) = self.excluded.get_mut(index as usize) {
            *slot = true;
        }
    }

    /// Whether an entry is left out.
    pub fn is_excluded(&self, index: u32) -> bool {
        self.excluded.get(index as usize).copied().unwrap_or(false)
    }

    /// The folder of an entry.
    pub fn parent_of(&self, index: u32) -> u32 {
        self.entries
            .get(index as usize)
            .map(|entry| entry.parent)
            .unwrap_or(NO_PARENT)
    }

    /// Change the size and the time of an entry.
    pub fn set_meta_live(&mut self, index: u32, size: u64, modified: u32, known: bool) {
        if let Some(entry) = self.entries.get_mut(index as usize) {
            entry.size = size;
            entry.modified = modified;
        }
        if let Some(flag) = self.meta_known.get_mut(index as usize) {
            *flag = known;
        }
    }
}

/// Whether a node tree mentions a path, which decides if the slow path
/// matching has to run.
pub fn needs_path(node: &Node) -> bool {
    match node {
        Node::Function {
            name,
            value,
            ..
        } => {
            matches!(
                name.as_str(),
                "parent" | "infolder" | "nosubfolders" | "child" | "shell"
            ) || name == "path"
                || value.contains('\\')
                || value.contains('/')
        }
        Node::And(children) | Node::Or(children) => children.iter().any(needs_path),
        Node::Not(inner) => needs_path(inner),
        _ => false,
    }
}

/// A map from a file reference number to an entry index.
///
/// `std::collections::HashMap` hashes with SipHash, which is more than this
/// needs: the keys are already well spread out integers. An open addressing
/// table with a multiply-shift hash is about twice as fast to build and takes
/// less memory, and the table is thrown away as soon as the tree is built.
struct FrnMap {
    /// The keys. `u64::MAX` marks an empty slot.
    keys: Vec<u64>,
    /// The values, only meaningful where the key is set.
    values: Vec<u32>,
    /// The number of occupied slots.
    len: usize,
}

impl FrnMap {
    /// A table with room for `expected` entries.
    fn with_capacity(expected: usize) -> Self {
        let capacity = (expected * 2).next_power_of_two().max(16);
        Self {
            keys: vec![u64::MAX; capacity],
            values: vec![0; capacity],
            len: 0,
        }
    }

    /// The slot a key belongs in.
    fn slot(&self, key: u64) -> usize {
        // A multiply and a shift spread the bits without a table of buckets.
        let hash = key
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(31)
            .wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        ((hash >> 40) as usize) & (self.keys.len() - 1)
    }

    /// Remember `value` for `key`.
    fn insert(&mut self, key: u64, value: u32) {
        if (self.len + 1) * 2 > self.keys.len() {
            self.grow();
        }
        let mut slot = self.slot(key);
        while self.keys[slot] != u64::MAX {
            if self.keys[slot] == key {
                self.values[slot] = value;
                return;
            }
            slot = (slot + 1) & (self.keys.len() - 1);
        }
        self.keys[slot] = key;
        self.values[slot] = value;
        self.len += 1;
    }

    /// The value that was stored for `key`.
    fn lookup(&self, key: u64) -> Option<&u32> {
        let mut slot = self.slot(key);
        while self.keys[slot] != u64::MAX {
            if self.keys[slot] == key {
                return Some(&self.values[slot]);
            }
            slot = (slot + 1) & (self.keys.len() - 1);
        }
        None
    }

    /// Make the table twice as large.
    fn grow(&mut self) {
        let capacity = self.keys.len() * 2;
        let old_keys = std::mem::replace(&mut self.keys, vec![u64::MAX; capacity]);
        let old_values = std::mem::replace(&mut self.values, vec![0; capacity]);
        for (key, value) in old_keys.into_iter().zip(old_values) {
            if key == u64::MAX {
                continue;
            }
            let mut slot = self.slot(key);
            while self.keys[slot] != u64::MAX {
                slot = (slot + 1) & (self.keys.len() - 1);
            }
            self.keys[slot] = key;
            self.values[slot] = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usn::{FILE_ATTRIBUTE_DIRECTORY, UsnEntry};

    fn entry(frn: u64, parent: u64, name: &str, dir: bool) -> UsnEntry {
        UsnEntry {
            frn,
            parent_frn: parent,
            name: name.to_string(),
            attributes: if dir { FILE_ATTRIBUTE_DIRECTORY } else { 0 },
        }
    }

    fn sample() -> FsIndex {
        let volume = usn::Volume {
            root: "C:\\".to_string(),
            device: "\\\\.\\C:".to_string(),
            label: "SYSTEM".to_string(),
        };
        let records = vec![
            // The root of the volume has an empty name, exactly as the MFT
            // enumeration reports it.
            entry(5, 5, "", true),
            entry(100, 5, "Users", true),
            entry(101, 100, "sisyphus", true),
            entry(102, 101, "Downloads", true),
            entry(103, 102, "report.pdf", false),
            entry(104, 101, "notes.txt", false),
        ];
        let (names, raw) = usn::collect_raw(&records);
        FsIndex::build(&[volume], &[(0, raw)], names, &Default::default())
    }

    #[test]
    fn index_holds_every_entry() {
        let index = sample();
        // The empty root is not an entry of its own.
        assert_eq!(index.len(), 5);
        assert_eq!(index.volumes().len(), 1);
    }

    /// The same table as [`sample`], but a file was renamed, one was added
    /// and one is gone.
    fn sample_changed() -> FsIndex {
        let volume = usn::Volume {
            root: "C:\\".to_string(),
            device: "\\\\.\\C:".to_string(),
            label: "SYSTEM".to_string(),
        };
        let records = vec![
            entry(5, 5, "", true),
            entry(100, 5, "Users", true),
            entry(101, 100, "sisyphus", true),
            entry(102, 101, "Documents", true),
            entry(103, 102, "report.pdf", false),
            entry(105, 101, "new.txt", false),
        ];
        let (names, raw) = usn::collect_raw(&records);
        FsIndex::build(&[volume], &[(0, raw)], names, &Default::default())
    }

    /// The same table as [`sample`], but one file was moved to another
    /// folder: its name and its size are the same.
    fn sample_moved() -> FsIndex {
        let volume = usn::Volume {
            root: "C:\\".to_string(),
            device: "\\\\.\\C:".to_string(),
            label: "SYSTEM".to_string(),
        };
        let records = vec![
            entry(5, 5, "", true),
            entry(100, 5, "Users", true),
            entry(101, 100, "sisyphus", true),
            entry(102, 101, "Downloads", true),
            entry(103, 102, "report.pdf", false),
            entry(104, 100, "notes.txt", false),
        ];
        let (names, raw) = usn::collect_raw(&records);
        FsIndex::build(&[volume], &[(0, raw)], names, &Default::default())
    }

    /// The files of an index that are really there.
    fn live(index: &FsIndex) -> Vec<String> {
        let mut names = Vec::new();
        for entry in 0..index.len() as u32 {
            if !index.is_excluded(entry) {
                names.push(index.name(entry).to_string());
            }
        }
        names
    }

    /// The same table as [`sample_changed`], but another file was renamed
    /// and one was added.
    fn sample_changed_again() -> FsIndex {
        let volume = usn::Volume {
            root: "C:\\".to_string(),
            device: "\\\\.\\C:".to_string(),
            label: "SYSTEM".to_string(),
        };
        let records = vec![
            entry(5, 5, "", true),
            entry(100, 5, "Users", true),
            entry(101, 100, "sisyphus", true),
            entry(102, 101, "Documents", true),
            entry(103, 102, "report.docx", false),
            entry(105, 101, "new.txt", false),
            entry(106, 101, "later.txt", false),
        ];
        let (names, raw) = usn::collect_raw(&records);
        FsIndex::build(&[volume], &[(0, raw)], names, &Default::default())
    }

    #[test]
    fn the_changes_of_two_rounds_stay_together() {
        // The changes file holds the difference between the table that is on
        // the disk and the disk itself. The changes of every round are added
        // up — a change that is left out of the file is a change that is lost
        // — and the number of the table stays the same, or the file would
        // belong to a table that does not exist.
        let base = sample();
        let table_path = std::env::temp_dir().join(format!(
            "fs-index-two-rounds-{}-{}.bin",
            std::process::id(),
            base.len()
        ));
        let changes_path = table_path.with_extension("delta");
        let base_stamp = base.save(&table_path).expect("the table is saved");

        // The first round: the table is read again and the differences are
        // written to the changes file.
        let first = sample_changed();
        let first_changes = first.changes_since(&base);
        base.save_changes(&changes_path, base_stamp, &first_changes)
            .expect("the changes of the first round are saved");

        // The second round: the table is read back, the changes are applied,
        // and the new differences go into the file **with** the old ones.
        let mut table = FsIndex::load(&table_path).expect("the table is read back");
        assert_eq!(table.built_at(), base_stamp, "the number of the table moved");
        let (written_at, read_back) =
            FsIndex::load_changes(&changes_path, base_stamp).expect("the changes are read back");
        table.apply_index_changes(&read_back);
        table.set_fresh_at(written_at);
        let second = sample_changed_again();
        let second_changes = second.changes_since(&table);
        let mut all = read_back.clone();
        all.extend(second_changes.iter().cloned());
        table
            .save_changes(&changes_path, base_stamp, &all)
            .expect("the changes of the second round are saved");

        // The third round: the machine was restarted. The table comes back,
        // and the changes come back with it — **all** of them.
        let mut table = FsIndex::load(&table_path).expect("the table is read back");
        let (_, read_back) = FsIndex::load_changes(&changes_path, table.built_at())
            .expect("the changes of the second round are still there");
        assert_eq!(
            read_back.len(),
            all.len(),
            "the changes of the first round were lost"
        );
        table.apply_index_changes(&read_back);
        let mut left = live(&table);
        let mut right = live(&second);
        left.sort();
        right.sort();
        assert_eq!(left, right, "the table does not hold the files it should");

        let _ = std::fs::remove_file(&table_path);
        let _ = std::fs::remove_file(&changes_path);
    }

    #[test]
    fn the_difference_between_two_tables_is_found() {
        let older = sample();
        let newer = sample_changed();
        let changes = newer.changes_since(&older);
        // The file that was renamed, the file that was added and the file
        // that is gone.
        assert_eq!(changes.len(), 3, "{changes:?}");
        let renamed = changes
            .iter()
            .find(|change| change.frn == 102)
            .expect("the renamed file");
        assert_eq!(renamed.name, "Documents");
        assert!(!renamed.deleted);
        let added = changes
            .iter()
            .find(|change| change.frn == 105)
            .expect("the added file");
        assert_eq!(added.name, "new.txt");
        assert!(!added.deleted);
        let gone = changes
            .iter()
            .find(|change| change.frn == 104)
            .expect("the deleted file");
        assert!(gone.deleted);
    }

    #[test]
    fn the_changes_bring_the_old_table_to_the_new_one() {
        let mut older = sample();
        let newer = sample_changed();
        let changes = newer.changes_since(&older);
        older.apply_index_changes(&changes);
        // The two tables hold the same files now.
        let mut left = live(&older);
        let mut right = live(&newer);
        left.sort();
        right.sort();
        assert_eq!(left, right);
        // And the new file is found by a query.
        let hits = older.search("new", 10);
        assert!(
            hits.iter().any(|hit| hit.name == "new.txt"),
            "the new file was not found: {hits:?}"
        );
        // The file that is gone is not found any more.
        let hits = older.search("notes", 10);
        assert!(
            !hits.iter().any(|hit| hit.name == "notes.txt"),
            "the deleted file is still there: {hits:?}"
        );
    }

    /// The same table as [`sample`], but the record of `notes.txt` was used
    /// again: the number is the same, the sequence is not.
    fn sample_recreated() -> FsIndex {
        let volume = usn::Volume {
            root: "C:\\".to_string(),
            device: "\\\\.\\C:".to_string(),
            label: "SYSTEM".to_string(),
        };
        let records = vec![
            entry(5, 5, "", true),
            entry(100, 5, "Users", true),
            entry(101, 100, "sisyphus", true),
            entry(102, 101, "Downloads", true),
            entry(103, 102, "report.pdf", false),
            entry((1u64 << 48) | 104, 101, "notes.txt", false),
        ];
        let (names, raw) = usn::collect_raw(&records);
        FsIndex::build(&[volume], &[(0, raw)], names, &Default::default())
    }

    #[test]
    fn a_file_that_was_deleted_and_created_again_is_found() {
        let mut older = sample();
        // The file is deleted.
        older.apply_index_changes(&[IndexChange {
            frn: 104,
            parent_frn: 101,
            name: "notes.txt".to_string(),
            deleted: true,
            size: 0,
            created: 0,
            modified: 0,
            accessed: 0,
            attributes: 0,
            meta_known: false,
        }]);
        let hits = older.search("notes", 10);
        assert!(
            !hits.iter().any(|hit| hit.name == "notes.txt"),
            "the deleted file is still found: {hits:?}"
        );
        // The record is used again: the same number, a new sequence.
        let newer = sample_recreated();
        let changes = newer.changes_since(&older);
        assert_eq!(changes.len(), 1, "{changes:?}");
        older.apply_index_changes(&changes);
        // And the file is there again — the mark of the deletion is gone.
        let hits = older.search("notes", 10);
        assert!(
            hits.iter().any(|hit| hit.name == "notes.txt"),
            "the file that came back is not found: {hits:?}"
        );
    }

    #[test]
    fn the_changes_survive_a_round_trip_through_a_file() {        let older = sample();
        let newer = sample_changed();
        let changes = newer.changes_since(&older);
        let path = std::env::temp_dir().join(format!(
            "fs-index-changes-{}-{}.bin",
            std::process::id(),
            changes.len()
        ));
        older
            .save_changes(&path, older.built_at(), &changes)
            .expect("the changes are saved");
        let (written_at, read_back) =
            FsIndex::load_changes(&path, older.built_at()).expect("the changes are read back");
        assert_eq!(read_back, changes);
        assert!(written_at > 0);
        // Changes that belong to another table are not used.
        assert!(FsIndex::load_changes(&path, older.built_at() + 1).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_table_that_grew_by_a_few_entries_is_still_compared_right() {
        // A table that came back from the disk can have entries at the end
        // that a fresh reading does not have in that place — the change
        // journal puts its files at the end. The comparison has to walk the
        // two tables by their record numbers, not by their places.
        let mut older = sample();
        older.apply_index_changes(&[IndexChange {
            frn: 900,
            parent_frn: 101,
            name: "added-by-the-journal.txt".to_string(),
            deleted: false,
            size: 10,
            created: 0,
            modified: 0,
            accessed: 0,
            attributes: 0,
            meta_known: true,
        }]);
        let newer = sample();
        let (changes, too_many) = newer.changes_since_limited(&older, 200_000);
        assert!(!too_many, "the comparison gave up");
        // The file that the journal added is not in the fresh reading.
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].frn, 900);
        assert!(changes[0].deleted);
    }

    #[test]
    fn two_tables_in_a_different_order_are_still_compared_right() {
        // The same files, but the table that was put together in another order.
        // The comparison looks the files up by the number of their record, so
        // the order must not matter — otherwise everything looks changed, and
        // the whole table is written again for nothing.
        let volume = usn::Volume {
            root: "C:\\".to_string(),
            device: "\\\\.\\C:".to_string(),
            label: "SYSTEM".to_string(),
        };
        let records = vec![
            entry(5, 5, "", true),
            entry(100, 5, "Users", true),
            entry(101, 100, "sisyphus", true),
            entry(102, 101, "Downloads", true),
            entry(103, 102, "report.pdf", false),
            entry(104, 101, "notes.txt", false),
        ];
        let (names, raw) = usn::collect_raw(&records);
        let straight = FsIndex::build(&[volume.clone()], &[(0, raw.clone())], names.clone(), &Default::default());

        // The same files, but the other way around: the names are in the
        // arena in the other order, so the entries are too.
        let mut turned = raw.clone();
        turned.reverse();
        let (names2, raw2) = usn::collect_raw(&records);
        let mut turned_names = String::new();
        for entry in &raw2 {
            turned_names.push_str(&names2[entry.name_start as usize..(entry.name_start + entry.name_len) as usize]);
        }
        let reversed = FsIndex::build(&[volume], &[(0, turned)], turned_names, &Default::default());

        // The two tables hold the same files, but in another order.
        let (changes, too_many) = straight.changes_since_limited(&reversed, 200_000);
        assert!(!too_many, "the comparison gave up: {changes:?}");
        assert!(
            changes.is_empty(),
            "the tables that hold the same files were not recognized: {changes:?}"
        );
    }

    #[test]
    fn a_file_at_the_end_that_is_another_one_now_is_a_change() {
        // Both tables have a file at the end that the change journal brought
        // in, and it is a different file now. The two are compared like any
        // other pair — and the places of the two must not be mixed up.
        let mut older = sample();
        older.apply_index_changes(&[IndexChange {
            frn: 900,
            parent_frn: 101,
            name: "old.txt".to_string(),
            deleted: false,
            size: 10,
            created: 0,
            modified: 0,
            accessed: 0,
            attributes: 0,
            meta_known: true,
        }]);
        let mut newer = sample();
        newer.apply_index_changes(&[IndexChange {
            frn: 900,
            parent_frn: 101,
            name: "new.txt".to_string(),
            deleted: false,
            size: 20,
            created: 0,
            modified: 0,
            accessed: 0,
            attributes: 0,
            meta_known: true,
        }]);
        let (changes, too_many) = newer.changes_since_limited(&older, 200_000);
        assert!(!too_many, "the comparison gave up: {changes:?}");
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].frn, 900);
        assert_eq!(changes[0].name, "new.txt");
    }

    #[test]
    fn a_table_that_was_saved_and_read_back_is_the_same_table() {        // The table that comes back from the disk is compared with a table
        // that was read fresh. If the two are not the same, every entry looks
        // like a change — and the whole table is written again for nothing.
        let built = sample();
        let path = std::env::temp_dir().join(format!(
            "fs-index-round-trip-{}-{}.bin",
            std::process::id(),
            built.len()
        ));
        built.save(&path).expect("the table is saved");
        let read_back = FsIndex::load(&path).expect("the table is read back");
        let _ = std::fs::remove_file(&path);

        let (changes, too_many) = built.changes_since_limited(&read_back, 200_000);
        assert!(!too_many, "the comparison gave up: {changes:?}");
        assert!(changes.is_empty(), "the table that came back is another one: {changes:?}");
    }

    #[test]
    fn a_table_that_did_not_change_has_no_changes() {
        let older = sample();
        let newer = sample();
        assert!(newer.changes_since(&older).is_empty());
    }

    #[test]
    fn a_file_that_was_moved_is_a_change() {        let older = sample();
        let newer = sample_moved();
        let changes = newer.changes_since(&older);
        // The file that moved: it has another name in the other table, but
        // the same name and the same size — only the folder is different.
        assert_eq!(changes.len(), 1, "{changes:?}");
        let moved = &changes[0];
        assert_eq!(moved.frn, 104);
        assert_eq!(moved.name, "notes.txt");
        assert_eq!(moved.parent_frn, 100);
        // And the change brings the old table to the new one.
        let mut older = sample();
        older.apply_index_changes(&changes);
        let mut left = live(&older);
        let mut right = live(&newer);
        left.sort();
        right.sort();
        assert_eq!(left, right);
    }

    #[test]
    fn the_entries_that_are_streamed_make_the_same_index() {
        let volume = usn::Volume {
            root: "C:\\".to_string(),
            device: "\\\\.\\C:".to_string(),
            label: "SYSTEM".to_string(),
        };
        let records = vec![
            entry(5, 5, "", true),
            entry(100, 5, "Users", true),
            entry(101, 100, "sisyphus", true),
            entry(102, 101, "Downloads", true),
            entry(103, 102, "report.pdf", false),
            entry(104, 101, "notes.txt", false),
        ];
        let (names, raw) = usn::collect_raw(&records);
        let built =
            FsIndex::build(&[volume.clone()], &[(0, raw.clone())], names.clone(), &Default::default());

        // The same entries, but handed over one at a time.
        let streamed = FsIndex::build_from(&[volume], &Default::default(), |sink| {
            sink.begin_volume(0, true);
            for record in &raw {
                let name = &names[record.name_start as usize
                    ..(record.name_start + record.name_len) as usize];
                sink.add_named(name, record);
            }
            sink.end_volume();
        });

        assert_eq!(streamed.len(), built.len());
        let mut streamed_paths: Vec<String> =
            (0..streamed.len() as u32).map(|i| streamed.path(i)).collect();
        let mut built_paths: Vec<String> =
            (0..built.len() as u32).map(|i| built.path(i)).collect();
        streamed_paths.sort();
        built_paths.sort();
        assert_eq!(streamed_paths, built_paths);
    }

    #[test]
    fn a_folder_that_comes_later_is_still_found() {
        // The number of a folder can be higher than the numbers of the files
        // in it, so the folder is sometimes read later than the files.
        let volume = usn::Volume {
            root: "C:\\".to_string(),
            device: "\\\\.\\C:".to_string(),
            label: "SYSTEM".to_string(),
        };
        let records = vec![
            entry(5, 5, "", true),
            entry(100, 5, "Users", true),
            entry(101, 100, "sisyphus", true),
            // The folder of the file comes after the file itself.
            entry(103, 102, "report.pdf", false),
            entry(102, 101, "Downloads", true),
        ];
        let (names, raw) = usn::collect_raw(&records);
        let index =
            FsIndex::build(&[volume.clone()], &[(0, raw.clone())], names.clone(), &Default::default());
        let streamed = FsIndex::build_from(&[volume], &Default::default(), |sink| {
            sink.begin_volume(0, true);
            for record in &raw {
                let name = &names[record.name_start as usize
                    ..(record.name_start + record.name_len) as usize];
                sink.add_named(name, record);
            }
            sink.end_volume();
        });
        let mut paths: Vec<String> = (0..streamed.len() as u32).map(|i| streamed.path(i)).collect();
        paths.sort();
        assert!(paths.contains(&"C:\\Users\\sisyphus\\Downloads\\report.pdf".to_string()));
        // The index that is built from the list finds the same thing.
        let mut other: Vec<String> = (0..index.len() as u32).map(|i| index.path(i)).collect();
        other.sort();
        assert_eq!(paths, other);
    }

    #[test]
    fn paths_are_rebuilt_from_the_tree() {
        let index = sample();
        let mut paths: Vec<String> = (0..index.len() as u32).map(|i| index.path(i)).collect();
        paths.sort();
        assert!(
            paths
                .iter()
                .any(|p| p.ends_with("Users\\sisyphus\\Downloads\\report.pdf")),
            "{paths:?}"
        );
    }

    #[test]
    fn searching_finds_a_file_by_name() {
        let index = sample();
        let hits = index.search("report", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "report.pdf");
        assert!(hits[0].path.ends_with("Downloads\\report.pdf"));
    }

    #[test]
    fn searching_finds_a_file_in_a_folder() {
        let index = sample();
        let hits = index.search("Downloads", 10);
        assert!(hits.iter().any(|h| h.name == "report.pdf"), "{hits:?}");
    }

    #[test]
    fn folder_filter_only_returns_folders() {
        let index = sample();
        let hits = index.search("folder:", 50);
        assert!(hits.iter().all(|h| h.is_dir), "{hits:?}");
        assert!(hits.iter().any(|h| h.name == "Downloads"));
    }

    #[test]
    fn negation_removes_results() {
        let index = sample();
        let hits = index.search("!report", 50);
        assert!(!hits.iter().any(|h| h.name == "report.pdf"));
    }

    #[test]
    fn exact_names_rank_first() {
        let index = sample();
        let hits = index.search("notes", 10);
        assert_eq!(hits[0].name, "notes.txt");
    }

    #[test]
    fn a_path_query_finds_the_file_by_its_folder() {
        // The path of a candidate is only built when the query really asks
        // for it, so the path queries have to be checked as well.
        let index = sample();
        let hits = index.search("path:Downloads", 10);
        assert!(
            hits.iter().any(|hit| hit.name == "report.pdf"),
            "the path query lost the file: {:?}",
            hits.iter().map(|hit| &hit.name).collect::<Vec<_>>()
        );
        // A plain word looks at the name of the folder as well, the way
        // Everything does it.
        let hits = index.search("Downloads", 10);
        assert!(
            hits.iter().any(|hit| hit.name == "report.pdf"),
            "the plain word lost the file in the folder: {:?}",
            hits.iter().map(|hit| &hit.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_folder_that_carries_the_term_is_still_found() {
        // The name of the folder is only looked up when the folder could
        // match, so the files in such a folder have to be found all the same.
        let index = sample();
        let hits = index.search("Downloads", 10);
        assert!(
            hits.iter().any(|hit| hit.name == "report.pdf"),
            "the folder match was lost: {:?}",
            hits.iter().map(|hit| &hit.name).collect::<Vec<_>>()
        );
        // And the folder itself is found by its own name.
        let hits = index.search("sisyphus", 10);
        assert!(hits.iter().any(|hit| hit.name == "notes.txt"));
    }

    #[test]
    fn the_filter_does_not_hide_a_match() {
        // The filter has to accept every entry that could match, including
        // the ones whose folder carries the term.
        let index = sample();
        for query in ["report", "Downloads", "Users", "notes", "pdf", "txt"] {
            let hits = index.search(query, 50);
            assert!(!hits.is_empty(), "the filter hid every match for {query:?}");
        }
    }

    #[test]
    fn the_best_candidates_are_the_ones_that_are_kept() {
        // Many entries match, so the scan has to choose. The ones that are
        // kept have to be the best ones and not just any of them.
        let volume = usn::Volume {
            root: "C:\\".to_string(),
            device: "\\\\.\\C:".to_string(),
            label: "SYSTEM".to_string(),
        };
        let mut records = vec![entry(5, 5, "", true)];
        let mut frn = 100u64;
        for file in 0..500u64 {
            records.push(entry(
                frn,
                5,
                &format!("report{file:04}.txt"),
                false,
            ));
            frn += 1;
        }
        // One entry that carries the term exactly.
        records.push(entry(frn, 5, "report.txt", false));

        let (names, raw) = usn::collect_raw(&records);
        let index = FsIndex::build(&[volume], &[(0, raw)], names, &Default::default());
        let hits = index.search("report", 5);
        assert_eq!(hits.len(), 5);
        // The name that carries the term exactly is the best one.
        assert_eq!(hits[0].name, "report.txt");
        // The rest are the shortest names.
        for hit in hits.iter().skip(1) {
            assert_eq!(hit.name.len(), 14, "{}", hit.name);
        }
        // And the whole list gives the same order.
        let every = index.search("report", 600);
        assert_eq!(every[0].name, hits[0].name);
        assert_eq!(every[1].name, hits[1].name);
    }

    #[test]
    fn a_large_index_answers_quickly() {
        // A quarter of a million entries in a three level tree: enough to
        // show that the scan stays in the millisecond range.
        let volume = usn::Volume {
            root: "C:\\".to_string(),
            device: "\\\\.\\C:".to_string(),
            label: "SYSTEM".to_string(),
        };
        let mut records = Vec::with_capacity(250_000);
        records.push(entry(5, 5, "", true));
        let mut frn = 100u64;
        for folder in 0..500u64 {
            let folder_frn = frn;
            frn += 1;
            records.push(entry(folder_frn, 5, &format!("dir{folder}"), true));
            for file in 0..500u64 {
                records.push(entry(
                    frn,
                    folder_frn,
                    &format!("file{folder}_{file}.txt"),
                    false,
                ));
                frn += 1;
            }
        }
        let started = std::time::Instant::now();
        let (names, raw) = usn::collect_raw(&records);
        let index = FsIndex::build(&[volume], &[(0, raw)], names, &Default::default());
        let build = started.elapsed();
        // The empty root is not an entry of its own.
        assert_eq!(index.len(), 250_500);

        let started = std::time::Instant::now();
        let hits = index.search("file12_34", 30);
        let elapsed = started.elapsed();
        assert!(!hits.is_empty(), "expected a match");
        // The whole table is scanned, so this is the worst case.
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "the scan took {elapsed:?}"
        );
        println!(
            "built {} entries in {build:?}, scanned them in {elapsed:?}",
            index.len()
        );
    }

    #[test]
    fn an_index_survives_a_round_trip_through_a_file() {
        let dir = std::env::temp_dir().join("ideas-launcher-fsindex-cache-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("fs-index.bin");
        let index = sample();
        index.save(&path).expect("the index should be written");

        let loaded = FsIndex::load(&path).expect("the index should be read back");
        assert_eq!(loaded.len(), index.len());
        assert_eq!(loaded.volume_signature(), index.volume_signature());
        // The names and the tree have to survive as well.
        let mut before: Vec<String> = (0..index.len() as u32).map(|i| index.path(i)).collect();
        let mut after: Vec<String> = (0..loaded.len() as u32).map(|i| loaded.path(i)).collect();
        before.sort();
        after.sort();
        assert_eq!(before, after);
        // And the filter has to be there, or every query would scan the
        // whole table.
        let hits = loaded.search("report", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "report.pdf");

        // The file reference numbers have to survive as well: the change
        // journal talks in them, so without them the index that was read
        // back could not be kept up to date.
        assert_eq!(loaded.frns.len(), index.frns.len());
        assert_eq!(loaded.frns, index.frns);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_cache_is_ignored() {
        let dir = std::env::temp_dir().join("ideas-launcher-fsindex-broken-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fs-index.bin");
        std::fs::write(&path, b"not an index at all").unwrap();
        assert!(FsIndex::load(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_size_filter_uses_the_metadata() {
        let index = sample();
        // Without a provider the size is unknown, so nothing matches.
        assert!(index.search("size:>1kb", 10).is_empty());

        // With a provider the filter works.
        let provider = |_: u32| Some(FileMeta {
            size: 4096,
            created: 0,
            modified: 0,
            accessed: 0,
            attributes: 0,
        });
        let hits = index.search_with("size:>1kb", 10, Some(&provider), FileSort::default());
        assert!(hits.iter().any(|h| h.name == "report.pdf"));

        // And a filter that asks for more than the file has matches nothing.
        let hits = index.search_with("size:>1mb", 10, Some(&provider), FileSort::default());
        assert!(hits.is_empty());
    }

    #[test]
    fn the_duplicates_are_found() {
        let index = sample();
        // Every name in the sample is unique, so nothing is a duplicate.
        assert!(index.search("dupe:", 50).is_empty());
    }

    #[test]
    fn the_children_are_found() {
        let index = sample();
        // The Downloads folder holds report.pdf.
        let hits = index.search("child:report", 50);
        assert!(hits.iter().any(|h| h.name == "Downloads"), "{hits:?}");
        // A name that is not there matches nothing.
        assert!(index.search("child:nothing", 50).is_empty());
    }

    #[test]
    fn the_frn_map_stores_and_finds_everything() {        let mut map = FrnMap::with_capacity(10);
        let keys = [1u64, 2, 3, 17, 1 << 40, u64::MAX - 1];
        for (index, key) in keys.iter().enumerate() {
            map.insert(*key, index as u32);
        }
        for (index, key) in keys.iter().enumerate() {
            assert_eq!(map.lookup(*key), Some(&(index as u32)));
        }
        assert_eq!(map.lookup(99), None);
    }

    #[test]
    fn the_numbers_of_the_table_are_in_order() {
        // The table is read in the order of the numbers, so a file can be
        // found by a binary search — and the map of nine million entries is
        // not needed.
        let index = sample();
        assert!(index.frns_are_in_order());
    }

    #[test]
    fn only_the_changes_that_mattered_are_written_down() {
        // **A file that is rewritten over and over.**
        //
        // A program that writes the same file a thousand times leaves a
        // thousand records in the change journal, and **none of them changes
        // the table**: the file is there, with the same name, in the same
        // folder — and the journal does not even carry its size and its time.
        //
        // Writing them down would make the changes file a hundred times
        // bigger than it has to be, and it is written again from the
        // beginning every time — so the cost is paid over and over. On a
        // machine that writes a lot, that was the loudest thing on the disk.
        let mut index = sample();
        // 200 is not in the sample: the file is new.
        let frn = 200u64;
        let mut changes = Vec::new();
        for _ in 0..1000 {
            changes.push(usn::JournalChange {
                frn,
                parent_frn: 102,
                name: "年度报告.pdf".to_string(),
                reason: usn::reason::FILE_CREATE,
                attributes: 0,
            });
        }
        let mut important = Vec::new();
        let touched = index.apply_changes_collecting(&changes, &mut important);
        // The file is new, so the table changed — but only once.
        assert_eq!(touched, 1);
        assert_eq!(
            important.len(),
            1,
            "{important:?} — only the change that really changed the table is worth writing down"
        );
    }

    #[test]
    fn a_change_of_the_journal_finds_its_file() {
        let mut index = sample();
        // The high 16 bits of the number are the **sequence** of the record:
        // it grows every time the record is used again. The low 48 bits are
        // the number of the record. A file that is still there has the same
        // number in the journal and in the table — the whole 64 bits are the
        // name of the file.
        let frn = 103u64;
        let changes = vec![usn::JournalChange {
            frn,
            parent_frn: 102,
            name: "年度报告.pdf".to_string(),
            reason: usn::reason::RENAME_NEW_NAME,
            attributes: 0,
        }];
        assert_eq!(index.apply_changes(&changes), 1);
        let hits = index.search("年度报告", 10);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].name, "年度报告.pdf");
        // The old name is gone.
        assert!(index.search("report", 10).is_empty());
    }

    #[test]
    fn a_file_that_takes_a_used_record_number_is_a_new_file() {
        let mut index = sample();
        // The record 103 was used by "report.pdf". The file was deleted, the
        // record was given to another file — and the sequence grew, so the
        // number of the new file is not the number of the old one.
        //
        // The two are different files, and the new one has to show up.
        let changes = vec![
            usn::JournalChange {
                frn: 103,
                parent_frn: 102,
                name: "report.pdf".to_string(),
                reason: usn::reason::FILE_DELETE,
                attributes: 0,
            },
            usn::JournalChange {
                frn: 0x0001_0000_0000_0067,
                parent_frn: 102,
                name: "新文件.txt".to_string(),
                reason: usn::reason::FILE_CREATE,
                attributes: 0,
            },
        ];
        assert_eq!(index.apply_changes(&changes), 2);
        // The new file is there, with its place.
        let hits = index.search("新文件", 10);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(
            hits[0].path.contains("Downloads"),
            "the place is wrong: {}",
            hits[0].path
        );
        // The old file is gone.
        assert!(index.search("report", 10).is_empty());
    }

    #[test]
    fn a_deleted_file_is_left_out() {
        let mut index = sample();
        let changes = vec![usn::JournalChange {
            frn: 103,
            parent_frn: 102,
            name: "report.pdf".to_string(),
            reason: usn::reason::FILE_DELETE,
            attributes: 0,
        }];
        assert_eq!(index.apply_changes(&changes), 1);
        assert!(index.search("report", 10).is_empty());
        // The other files are still there.
        assert_eq!(index.search("notes", 10).len(), 1);
    }

    #[test]
    fn a_created_file_is_added_with_its_place() {
        let mut index = sample();
        let changes = vec![usn::JournalChange {
            frn: 200,
            parent_frn: 102,
            name: "新文件.txt".to_string(),
            reason: usn::reason::FILE_CREATE,
            attributes: 0,
        }];
        assert_eq!(index.apply_changes(&changes), 1);
        let hits = index.search("新文件", 10);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(
            hits[0].path.contains("Downloads"),
            "the place is wrong: {}",
            hits[0].path
        );
    }
}
