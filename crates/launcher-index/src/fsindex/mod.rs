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

// The one type is defined here; the impl blocks and the helpers live in
// child modules by concern. Every child sees the private fields, which is
// what keeps the split a pure code movement.
mod build;
mod core;
mod frn;
mod masks;
mod reader;
mod search;
mod sink;
mod writer;

pub use sink::EntrySink;
// The helpers live in child modules; re-exported so that the sibling
// modules can reach them through `use super::*`.
pub(in crate::fsindex) use frn::FrnMap;
pub(in crate::fsindex) use masks::{name_mask, required_mask, term_mask};
pub(in crate::fsindex) use reader::FileReader;
pub(in crate::fsindex) use search::IndexTarget;
pub(in crate::fsindex) use reader::is_cjk;
pub(in crate::fsindex) use writer::io_priority;
pub(in crate::fsindex) use writer::ChunkWriter;

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
