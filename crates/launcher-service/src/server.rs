//! The service that keeps the index of the whole machine.
//!
//! Everything does it like this: the part that holds the index runs as a
//! Windows service, and the part with the window asks it for the files. The
//! service is started when the machine starts, it reads the master file table
//! once, and from then on it follows the change journal of the volumes — so
//! the index is always a few seconds old, and the window never has to read
//! the disk itself.
//!
//! The window is then a small program: it does not hold nine million file
//! names in its memory, and it never writes to the disk.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::RwLock;

use launcher_core::service::{FileHit, Request, Response, Status};
use launcher_index::fsindex::{Exclusions, FsIndex, FsHit};
use launcher_index::usn;

use crate::winapi::*;

/// The name of the service in the control manager.
pub const SERVICE_NAME: &str = "IdeasLauncherService";

/// The name the service shows in the list of services.
pub const DISPLAY_NAME: &str = "IDEAS Launcher 索引服务";

/// How much of the journal is read in one round.
///
/// The backlog of a machine that has been writing a lot can be hundreds of
/// megabytes. Reading all of it at once would keep the disk busy for
/// minutes on end; reading a part of it now and the rest in the next round
/// spreads the work out, and the index catches up while everything else goes
/// on. **The table is never more than a minute behind, and the disk is
/// never busy for more than a moment.**
const JOURNAL_ROUNDS_PER_ROUND: usize = 8;

/// How often the changes are put into the index, in seconds.
///
/// The setting says it; the environment variable `IDEAS_UPDATE_INTERVAL`
/// says it too, and wins — it is there for the tests, and for anybody who
/// wants the index to be fresher (or quieter) than the setting says.
fn journal_interval(settings_path: &std::path::Path) -> std::time::Duration {
    if let Ok(seconds) = std::env::var("IDEAS_UPDATE_INTERVAL") {
        if let Ok(seconds) = seconds.trim().parse::<u64>() {
            return std::time::Duration::from_secs(seconds.clamp(1, 3600));
        }
    }
    let settings = launcher_core::settings::Settings::load(settings_path);
    std::time::Duration::from_secs(settings.index_update_interval.clamp(1, 3600))
}

/// The state the service shares with its threads.
pub struct Service {
    /// The index of the whole machine.
    index: RwLock<Option<FsIndex>>,
    /// Whether the index is being built right now.
    building: AtomicBool,
    /// How many changes the journal has brought in.
    changes: AtomicU64,
    /// The changes that are not in the saved table yet.
    ///
    /// The changes file holds the difference between the saved table and the
    /// disk, so the changes of every round are **added up**: the file that is
    /// written holds all of them.
    pending: std::sync::Mutex<Vec<launcher_index::fsindex::IndexChange>>,
    /// The number of the table that is on the disk.
    ///
    /// The changes file is written against this number, so that the changes
    /// of another table are never used. The table on the disk keeps this
    /// number for as long as it lives — only a whole new table gets a new
    /// one.
    ///
    /// It is **not** the number of the table that is in memory: that one is
    /// read again every round and would carry a new number every time, and
    /// the changes file would then belong to a table that does not exist.
    base_stamp: AtomicU64,
    /// Whether the change journal is being followed.
    journal: AtomicBool,
    /// Whether the changes file was read back.
    ///
    /// It holds the difference between the saved table and the disk, so it
    /// covers the stretch of the change journal that has already been
    /// forgotten — and that is what makes it possible to go on without
    /// reading the disk again.
    has_changes: AtomicBool,
    /// Where the change journal of every volume stood the last time.
    ///
    /// The journal is followed from here, so that nothing that happened while
    /// the table was being read is lost.
    journal_from: std::sync::Mutex<Vec<(String, i64)>>,
    /// Whether the first table has been read.
    ///
    /// The change journal waits for it: while the table is being read, the
    /// two would change the same table, and the changes of the journal could
    /// be lost when the table that was just read replaces the old one.
    ready: AtomicBool,
    /// Whether the service should stop.
    stop: AtomicBool,
    /// The folder of the user the service serves.
    profile: String,
}

impl Service {
    /// A service that serves the files of one user.
    pub fn new(profile: String) -> Self {
        Self {
            index: RwLock::new(None),
            pending: std::sync::Mutex::new(Vec::new()),
            base_stamp: AtomicU64::new(0),
            building: AtomicBool::new(false),
            changes: AtomicU64::new(0),
            journal: AtomicBool::new(false),
            has_changes: AtomicBool::new(false),
            journal_from: std::sync::Mutex::new(Vec::new()),
            ready: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            profile,
        }
    }

    /// Where the index is kept.
    ///
    /// It is the folder of the user the service serves, so that the service
    /// and the window talk about the same files.
    fn cache_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.profile)
            .join("AppData")
            .join("Roaming")
            .join("IdeasLauncher")
            .join("Cache")
            .join("fs-index.bin")
    }

    /// The settings of the user the service serves.
    fn settings_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.profile)
            .join("AppData")
            .join("Roaming")
            .join("IdeasLauncher")
            .join("Settings")
            .join("Settings.json")
    }

    /// Read the saved table back, if there is one.
    ///
    /// The table is the picture of the disk from the moment it was taken.
    /// It is served right away, and the journal brings it up to date while
    /// the machine is running.
    pub fn load_saved(&self) -> bool {
        self.load_saved_inner(true)
    }

    /// Read the saved table back without touching anything on the disk.
    ///
    /// This is what the diagnostics use: a file of another table is of no
    /// use, but throwing it away is not the job of a diagnostic — least of
    /// all while the service is writing the same folder.
    pub fn read_saved(&self) -> bool {
        self.load_saved_inner(false)
    }

    /// Read the saved table back.
    fn load_saved_inner(&self, write: bool) -> bool {
        let path = self.cache_path();
        let Some(mut index) = FsIndex::load(&path) else {
            log::info!("there is no saved table");
            self.base_stamp.store(0, Ordering::SeqCst);
            return false;
        };
        // The number of the table that is on the disk. The changes file is
        // written against it, so it has to be remembered on its own and not
        // taken from the table that is in memory.
        let base = index.built_at();
        self.base_stamp.store(base, Ordering::SeqCst);
        // The changes that came after the table was saved are read back and
        // applied to it.
        match FsIndex::load_changes(&self.changes_path(), base) {
            Some((written_at, changes)) => {
                let touched = index.apply_index_changes(&changes);
                index.set_fresh_at(written_at);
                // The changes that were read back are the ones that are not in
                // the saved table yet — they are written again with the next
                // round, or they would be lost.
                if let Ok(mut pending) = self.pending.lock() {
                    pending.extend(changes);
                }
                self.has_changes.store(true, Ordering::SeqCst);
                log::info!("the changes since the saved table were applied: {touched} entries");
            }
            None => {
                // The changes file is missing, or it belongs to another table.
                // A file of another table is of no use and would only get in
                // the way — it goes.
                if write {
                    let _ = std::fs::remove_file(self.changes_path());
                }
            }
        }
        log::info!("the saved table was read back: {} entries", index.len());
        *self.index.write().unwrap_or_else(|error| error.into_inner()) = Some(index);
        true
    }

    /// Give the memory of the process back to the system.
    ///
    /// Building the index allocates and frees hundreds of megabytes in
    /// pieces — the entries, the names, the masks, the temporary records of
    /// the master file table. The allocator of Windows does not return the
    /// freed pages to the system on its own (the private memory of the
    /// service was **twice** its working set), and what it does return it
    /// returns slowly. This tells the system: the big allocations are over,
    /// the leftovers are garbage — take them.
    pub fn trim_memory(&self) {
        unsafe {
            // `HeapCompact` walks the heap and returns the free blocks at
            // the end of each segment to the system.
            let heap = GetProcessHeap();
            if !heap.is_null() {
                HeapCompact(heap, 0);
            }
            // `SetProcessWorkingSetSize(-1, -1)` trims the working set: the
            // pages that have not been touched lately are removed from the
            // physical memory. They stay committed and come back on the
            // next touch — nothing is lost — but the number the user sees
            // in the task manager says what the service *needs*, not what
            // it happened to touch while it was building.
            let process = GetCurrentProcess();
            if !process.is_null() {
                SetProcessWorkingSetSize(
                    process,
                    usize::MAX,
                    usize::MAX,
                );
            }
        }
    }

    /// Where the changes since the saved table are kept.
    fn changes_path(&self) -> std::path::PathBuf {
        self.cache_path().with_extension("delta")
    }

    /// Read the master file table of every NTFS volume.
    ///
    /// This is the slow part — a few tens of seconds — and it happens in a
    /// thread of its own, so that the service answers questions while it
    /// reads.
    ///
    /// When the table is already there, only the **changes** are written:
    /// the whole table is three quarters of a gigabyte, and writing it every
    /// few hours is the one thing that really wears the disk.
    pub fn build(&self) {
        self.build_inner(true);
        // The reading of the master file table allocated and freed hundreds
        // of megabytes in temporary pieces — give the leftovers back.
        self.trim_memory();
        // The table is there now, so the change journal may follow it.
        self.ready.store(true, Ordering::SeqCst);
    }

    /// Read the master file table again and only look at what changed.
    ///
    /// Nothing is written to the disk: this is what the diagnostics use, and
    /// a diagnostic must not change the state of the machine — least of all
    /// the file that the running service is writing at the same time.
    pub fn inspect(&self) {
        self.build_inner(false);
    }

    /// Read the master file table again.
    fn build_inner(&self, write: bool) {
        if self.building.swap(true, Ordering::SeqCst) {
            return;
        }
        let volumes = usn::ntfs_volumes();
        if volumes.is_empty() {
            log::warn!("there is no NTFS volume to index");
            self.building.store(false, Ordering::SeqCst);
            return;
        }
        let exclusions = self.exclusions();
        // The entries are handed over to the index as they are read, so the
        // flat list of them never exists on its own: the memory stays at the
        // size of the index.
        let fresh = FsIndex::build_from(&volumes, &exclusions, |sink| {
            for (number, volume) in volumes.iter().enumerate() {
                // Where the change journal stood **before** the table of this
                // volume is read. The table carries that place with it, so
                // that it can be brought up to date later on without reading
                // the disk again.
                //
                // The place is taken before the reading and not after it: a
                // change that came while the table was being read is then
                // read from the journal once more, and a change that is
                // already in the table is simply read twice. A change that is
                // missed cannot be read again.
                let point = usn::journal_state(volume)
                    .map(|state| state.next_usn as u64)
                    .unwrap_or(0);
                sink.set_journal_point(number as u32, point);
                // The fast path: the master file table is read straight off
                // the disk, in one go. The slow path would read the change
                // journal — and that is not always possible.
                sink.begin_volume(number as u32, true);
                let read = usn::enumerate_mft_with_bytes(
                    volume,
                    None,
                    None,
                    |name, fields| sink.add(name, &fields),
                );
                match read {
                    Ok(stats) => {
                        log::info!(
                            "{} was read: {} records in {} ms",
                            volume.root,
                            stats.records,
                            stats.read_ms
                        );
                        sink.end_volume();
                    }
                    Err(error) => {
                        log::warn!("{} could not be read: {error:?}", volume.root);
                    }
                }
            }
        });
        log::info!("the table was read: {} entries", fresh.len());
        if fresh.is_empty() {
            // Nothing could be read. The table that is on the disk is still
            // the truth — and it must not be replaced by an empty one.
            log::warn!("the table is empty — the saved one was left alone");
            self.building.store(false, Ordering::SeqCst);
            return;
        }

        // What changed since the table that is on the disk.
        let started = std::time::Instant::now();
        let (changes, too_many) = {
            let guard = self.index.read().unwrap_or_else(|error| error.into_inner());
            match guard.as_ref() {
                Some(older) => fresh.changes_since_limited(older, 200_000),
                None => (Vec::new(), false),
            }
        };
        log::info!("the difference was worked out in {:?}", started.elapsed());
        if too_many {
            // The table on the disk and the one that was just read do not
            // belong together — the whole table is written again.
            log::warn!("the tables differ in too many places — the whole table is written again");
            // The first few differences are written down, so that a
            // difference that should not be there can be seen.
            for change in changes.iter().take(5) {
                log::info!(
                    "  changed: {:?} (frn 0x{:x}), deleted: {}",
                    change.name,
                    change.frn,
                    change.deleted
                );
            }
        } else if !changes.is_empty() {
            for change in changes.iter().take(5) {
                log::info!(
                    "  changed: {:?} (frn 0x{:x}), deleted: {}",
                    change.name,
                    change.frn,
                    change.deleted
                );
            }
        }
        // The number of the table that is on the disk. The changes file is
        // written against it — the table that is in memory was read just now
        // and carries a new number every round, and the changes would then
        // belong to a table that does not exist.
        let base_stamp = self.base_stamp.load(Ordering::SeqCst);
        // The changes that are not in the saved table yet: the ones that were
        // read back when the service started and the ones that were just
        // found. They go into the file **all together** — the file holds the
        // difference between the saved table and the disk, so a change that is
        // left out of it is a change that is lost.
        let mut all = self.pending.lock().map(|list| list.clone()).unwrap_or_default();
        all.extend(changes.iter().cloned());
        match what_to_write(base_stamp, changes.len(), all.len(), fresh.len(), too_many) {
            Writing::Nothing => {
                // Nothing on the disk has changed: the saved table is still
                // the right one, and not a byte is written.
                log::info!("the table is unchanged — nothing was written");
                self.building.store(false, Ordering::SeqCst);
                return;
            }
            Writing::Changes => {
                if !write {
                    log::info!(
                        "{} changes would be written ({} of them from the earlier rounds)",
                        all.len(),
                        all.len() - changes.len()
                    );
                } else if let Err(error) =
                    fresh.save_changes(&self.changes_path(), base_stamp, &all)
                {
                    log::warn!("the changes could not be saved: {error}");
                } else {
                    let count = all.len();
                    if let Ok(mut pending) = self.pending.lock() {
                        *pending = all;
                    }
                    log::info!("only the changes were saved: {count} entries");
                }
            }
            Writing::Whole => {
                if !write {
                    log::info!("the whole table would be written");
                } else {
                    match fresh.save(&self.cache_path()) {
                        Err(error) => log::warn!("the table could not be saved: {error}"),
                        Ok(stamp) => {
                            log::info!("the table was saved");
                            // The new table has a new number, and it holds the
                            // changes of the old one — so the changes file has
                            // nothing left to do and goes.
                            self.base_stamp.store(stamp, Ordering::SeqCst);
                            let _ = std::fs::remove_file(self.changes_path());
                            // The table that was written holds everything, so
                            // there is nothing left to remember.
                            if let Ok(mut pending) = self.pending.lock() {
                                pending.clear();
                            }
                        }
                    }
                }
            }
        }
        let mut fresh = fresh;
        fresh.set_fresh_at(fresh.stamp());
        *self.index.write().unwrap_or_else(|error| error.into_inner()) = Some(fresh);
        self.building.store(false, Ordering::SeqCst);
    }

    /// What the settings say should be left out.
    fn exclusions(&self) -> Exclusions {
        let settings = launcher_core::settings::Settings::load(&self.settings_path());
        Exclusions {
            paths: settings
                .excluded_paths
                .iter()
                .map(|path| path.to_lowercase())
                .collect(),
            extensions: settings
                .excluded_extensions
                .iter()
                .map(|extension| extension.to_lowercase())
                .collect(),
        }
    }

    /// Follow the change journal of every volume.
    ///
    /// This is what keeps the table a few seconds old without reading the
    /// disk again. The journal is a ring buffer in the file system: every
    /// change of every file is written into it, and reading it is a few
    /// hundred kilobytes — nothing like reading the whole table.
    pub fn follow_journal(&self) {
        let volumes = usn::ntfs_volumes();
        if volumes.is_empty() {
            return;
        }
        self.journal.store(true, Ordering::SeqCst);
        // The journal is followed from the place it was at when the service
        // started — or from the place the table carries with it. Everything
        // that happened since then is read, and nothing before it is read
        // twice.
        let mut written = 0usize;
        let mut last_write = std::time::Instant::now();
        // How many times in a row the journal of a volume could not be read.
        // The first time it is written down, and then only every sixtieth —
        // a volume without a journal would otherwise fill the log.
        let mut failures: Vec<(String, u32)> = Vec::new();
        while !self.stop.load(Ordering::SeqCst) {
            for volume in &volumes {
                let after = self.journal_point(&volume.root);
                match self.follow_one(volume, after) {
                    Ok(next) => {
                        if next > after {
                            self.set_journal_point(&volume.root, next);
                        }
                        failures.retain(|(root, _)| root != &volume.root);
                    }
                    Err(code) => {
                        // A volume whose journal cannot be read does not stop
                        // the others: it only means that the files of that
                        // volume stay as they were.
                        let count = match failures.iter_mut().find(|(root, _)| root == &volume.root) {
                            Some(slot) => {
                                slot.1 += 1;
                                slot.1
                            }
                            None => {
                                failures.push((volume.root.clone(), 1));
                                1
                            }
                        };
                        if count == 1 || count % 60 == 0 {
                            log::warn!(
                                "the change journal of {} cannot be read ({code})",
                                volume.root
                            );
                        }
                    }
                }
            }
            // The changes are written now and then. The file holds the
            // difference between the saved table and the disk, and it is
            // written when it has grown by a quarter — so the writing stays
            // small even when the changes pile up.
            //
            // **It is written at most once per round of the journal**, and
            // only when there is something worth writing: a machine that
            // writes nothing must not have the disk touched at all.
            let pending = self.pending.lock().map(|list| list.len()).unwrap_or(0);
            if pending > written + written / 4 + 1000
                && last_write.elapsed() >= std::time::Duration::from_secs(60)
            {
                written = self.write_changes();
                last_write = std::time::Instant::now();
            }
            // The changes have become a fifth of the table: the whole table
            // is written again and the changes are dropped with it. The table
            // is already in memory, so the disk is not read for this — only
            // written.
            if self.rewrite_base() {
                written = 0;
            }
            std::thread::sleep(journal_interval(&self.settings_path()));
        }
        // The changes that are left are written before the service goes, so
        // that they are not lost.
        self.write_changes();
        self.journal.store(false, Ordering::SeqCst);
    }

    /// Read the change journal of one volume and bring the table up to date.
    ///
    /// The journal is read until it has nothing more to say — a megabyte at a
    /// time, and as fast as the disk hands it over.
    ///
    /// Returns where the journal stands afterwards.
    fn follow_one(&self, volume: &usn::Volume, after: i64) -> Result<i64, u32> {
        let mut after = after;
        // The number of rounds is bounded, so that the other threads of the
        // service get their turn even when the journal has a long backlog.
        // **Only a part of the backlog is read in one round** — the rest is
        // read in the next one, and the work is spread out instead of
        // keeping the disk busy for minutes.
        for _ in 0..JOURNAL_ROUNDS_PER_ROUND {
            let (changes, next) = usn::read_journal(volume, after)?;
            if next <= after {
                break;
            }
            after = next;
            if changes.is_empty() {
                break;
            }
            {
                let mut guard = self
                    .index
                    .write()
                    .unwrap_or_else(|error| error.into_inner());
                if let Some(index) = guard.as_mut() {
                    // **Only the changes that really changed the table are
                    // written down.** A program that rewrites the same file
                    // a thousand times leaves a thousand records in the
                    // journal, and none of them changes the table. Writing
                    // them down would make the changes file a hundred times
                    // bigger than it has to be — and it is written again
                    // from the beginning every time, so the cost would be
                    // paid over and over.
                    let mut important = Vec::new();
                    let touched = index.apply_changes_collecting(&changes, &mut important);
                    if touched > 0 {
                        log::info!("the change journal brought {touched} entries up to date");
                        self.changes.fetch_add(touched as u64, Ordering::Relaxed);
                    }
                    // The changes are remembered, so that they can be written
                    // to the disk: the table that was saved does not have them.
                    if let Ok(mut pending) = self.pending.lock() {
                        pending.extend(important);
                    }
                }
            }
        }
        Ok(after)
    }

    /// Bring the table that was read back up to date with the change journal.
    ///
    /// The table carries the place of the change journal with it, so the
    /// changes that came after it can be read without touching the disk.
    /// That is what makes the service start without reading the whole table
    /// again: the disk is only read when there is no table yet.
    ///
    /// Returns whether the table is up to date. When it is not — there is no
    /// table, the place is not known, or the journal has forgotten the
    /// records since then — the table has to be read again.
    pub fn catch_up(&self) -> bool {
        let volumes = {
            let guard = self.index.read().unwrap_or_else(|error| error.into_inner());
            let Some(index) = guard.as_ref() else {
                log::info!("there is no table to bring up to date");
                return false;
            };
            let points = index.journal_points();
            if points.is_empty() {
                log::info!("the place of the change journal is not known — the table has to be read again");
                return false;
            }
            index
                .volumes()
                .iter()
                .zip(points.iter())
                .map(|(volume, point)| (volume.root.clone(), *point as i64))
                .collect::<Vec<_>>()
        };
        // The journal is a ring buffer: when the place is older than the
        // oldest record that is still in it, the records are gone. The
        // changes file covers that stretch — it holds the difference between
        // the saved table and the disk — so the journal is read from the
        // oldest record that is still there. Without the changes file there
        // is nothing to fill the gap with, and the table has to be read again.
        let has_changes = self.has_changes.load(Ordering::SeqCst);
        let mut starts = Vec::new();
        for (root, point) in &volumes {
            let Some(state) = usn::journal_state_at(root) else {
                // The volume has no journal at all: its files stay as they
                // are until the table is read again. That is no reason to
                // read the whole disk again now.
                log::warn!("{root} has no change journal — the files of that volume stay as they are");
                continue;
            };
            if state.next_usn == 0 {
                log::warn!("the change journal of {root} is empty — the files of that volume stay as they are");
                continue;
            }
            if *point > state.next_usn {
                // The journal was made new again, and its numbers start from
                // the beginning. The numbers of the table say nothing then.
                log::warn!(
                    "the change journal of {root} was made new again — the table has to be read again"
                );
                return false;
            }
            let mut start = *point;
            if start < state.first_usn {
                if !has_changes {
                    log::warn!(
                        "the change journal of {root} has forgotten the records since {point}, and \
                         there are no changes to fill the gap with — the table has to be read again"
                    );
                    return false;
                }
                log::info!(
                    "the change journal of {root} has forgotten the records since {point} — the \
                     changes file fills that gap"
                );
                start = state.first_usn;
            }
            starts.push((root.clone(), start));
        }
        if starts.is_empty() {
            log::warn!("no volume has a change journal — the table has to be read again");
            return false;
        }
        // The changes between the place of the table and now are read.
        for (root, start) in &starts {
            let volume = usn::Volume {
                root: root.clone(),
                device: String::new(),
                label: String::new(),
            };
            match self.follow_one(&volume, *start) {
                Ok(next) => self.set_journal_point(root, next),
                Err(code) => {
                    log::warn!("the change journal of {root} cannot be read ({code}) — the table has to be read again");
                    return false;
                }
            }
        }
        log::info!("the table was brought up to date with the change journal — the disk was not read");
        true
    }

    /// Where the change journal of a volume stood.
    fn journal_point(&self, root: &str) -> i64 {
        self.journal_from
            .lock()
            .ok()
            .and_then(|list| list.iter().find(|(known, _)| known == root).map(|(_, usn)| *usn))
            .unwrap_or(0)
    }

    /// Say where the change journal of a volume stands.
    fn set_journal_point(&self, root: &str, usn: i64) {
        if let Ok(mut list) = self.journal_from.lock() {
            match list.iter_mut().find(|(known, _)| known == root) {
                Some(slot) => slot.1 = usn,
                None => list.push((root.to_string(), usn)),
            }
        }
    }

    /// Write the changes that are not in the saved table yet.
    ///
    /// The changes file holds the difference between the saved table and the
    /// disk, so a change that is left out of it is a change that is lost.
    ///
    /// Returns how many changes were written.
    fn write_changes(&self) -> usize {
        let base_stamp = self.base_stamp.load(Ordering::SeqCst);
        if base_stamp == 0 {
            // There is no saved table, so there is nothing to write the
            // changes against.
            return 0;
        }
        let Ok(pending) = self.pending.lock() else {
            return 0;
        };
        if pending.is_empty() {
            return 0;
        }
        let guard = self.index.read().unwrap_or_else(|error| error.into_inner());
        let Some(index) = guard.as_ref() else {
            return 0;
        };
        match index.save_changes(&self.changes_path(), base_stamp, &pending) {
            Ok(()) => {
                let count = pending.len();
                log::info!("the changes were written: {count} entries");
                count
            }
            Err(error) => {
                log::warn!("the changes could not be saved: {error}");
                0
            }
        }
    }

    /// Write the table that is in memory as the new table on the disk.
    ///
    /// The table in memory is the saved table plus the changes of the change
    /// journal, so it is the picture of the disk as it is now. Writing it
    /// again makes the changes file empty — and the disk is not read at all,
    /// because the table is already in memory.
    ///
    /// This is what keeps the writing down: the whole table is three quarters
    /// of a gigabyte, and it is only written when the changes have become a
    /// fifth of the table.
    fn rewrite_base(&self) -> bool {
        let entries = {
            let guard = self.index.read().unwrap_or_else(|error| error.into_inner());
            let Some(index) = guard.as_ref() else {
                return false;
            };
            index.len()
        };
        let pending = self.pending.lock().map(|list| list.len()).unwrap_or(0);
        // The changes are not yet a fifth of the table: the changes file is
        // enough.
        if pending * 5 <= entries.max(1) {
            return false;
        }
        // The place of the journal is written into the table, so that the
        // table that is read back knows where to go on. It is taken before
        // the table is written, and it is never newer than what the table
        // holds.
        {
            let mut guard = self
                .index
                .write()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(index) = guard.as_mut() {
                let points: Vec<u64> = index
                    .volumes()
                    .iter()
                    .map(|volume| self.journal_point(&volume.root) as u64)
                    .collect();
                index.set_journal_points(points);
            }
        }
        // The table is written while only the read lock is held: the queries
        // keep working, and the journal thread waits for a moment.
        let guard = self.index.read().unwrap_or_else(|error| error.into_inner());
        let Some(index) = guard.as_ref() else {
            return false;
        };
        match index.save(&self.cache_path()) {
            Ok(stamp) => {
                log::info!("the table was written again: {entries} entries");
                drop(guard);
                self.base_stamp.store(stamp, Ordering::SeqCst);
                // The new table holds everything, so the changes file has
                // nothing left to do and goes.
                let _ = std::fs::remove_file(self.changes_path());
                if let Ok(mut pending) = self.pending.lock() {
                    pending.clear();
                }
                true
            }
            Err(error) => {
                log::warn!("the table could not be saved: {error}");
                false
            }
        }
    }

    /// Read the table again now and then.
    ///
    /// When the change journal is being followed, this is not needed: the
    /// table is a few seconds old at all times. When it is not there, the
    /// table is read again once a day — and then only the changes are
    /// written, so the disk is barely touched.
    pub fn refresh_every_day(&self) {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(24 * 3600));
            if self.journal.load(Ordering::SeqCst) {
                continue;
            }
            self.build();
        }
    }

    /// The hits of a query.
    pub fn query(&self, term: &str, limit: usize) -> Vec<FileHit> {
        let guard = self.index.read().unwrap_or_else(|error| error.into_inner());
        let Some(index) = guard.as_ref() else {
            return Vec::new();
        };
        let hits = index.search_with(term, limit, None, launcher_core::settings::FileSort::default());
        hits.iter().map(file_hit).collect()
    }

    /// What the service is doing.
    pub fn status(&self) -> Status {
        let guard = self.index.read().unwrap_or_else(|error| error.into_inner());
        Status {
            entries: guard.as_ref().map(|index| index.len() as u64).unwrap_or(0),
            built_at: guard
                .as_ref()
                .map(|index| index.built_at())
                .unwrap_or(0),
            journal: self.journal.load(Ordering::SeqCst),
            building: self.building.load(Ordering::SeqCst),
            changes: self.changes.load(Ordering::Relaxed),
        }
    }

    /// Whether the service should stop.
    pub fn should_stop(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Whether the first table has been read.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// Say that the table is there and may be changed.
    ///
    /// The change journal waits for this: while the table is being read, the
    /// two would change the same table, and the changes of the journal could
    /// be lost when the table that was just read replaces the old one.
    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }

    /// Note where the change journal of every volume stands.
    ///
    /// The journal is followed from here, so that the changes that come
    /// while the table is being read are not lost.
    pub fn remember_journal_points(&self) {
        for volume in usn::ntfs_volumes() {
            let usn = usn::journal_state(&volume)
                .map(|state| state.next_usn)
                .unwrap_or(0);
            self.set_journal_point(&volume.root, usn);
        }
    }

    /// Tell the service to stop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// What is written to the disk after a round.
#[derive(Debug, PartialEq, Eq)]
enum Writing {
    /// Nothing at all: the table that is on the disk is still the right one.
    Nothing,
    /// Only the changes since the table that is on the disk.
    Changes,
    /// The whole table.
    Whole,
}

/// What is written after a round.
///
/// The rules are simple:
///
/// * There is no table on the disk yet — the whole thing is written.
/// * The changes are more than a fiftieth of the table — the whole table is
///   written again, and the changes are dropped with it.
/// * Too many changes to keep them in memory — the whole table is written
///   again. That is the right answer for a table that really did change
///   completely, and it keeps the list of changes small.
/// * Nothing changed since the last round — not a byte is written.
/// * Otherwise — only the changes, a few megabytes instead of three quarters
///   of a gigabyte.
fn what_to_write(
    base_stamp: u64,
    new_changes: usize,
    total: usize,
    entries: usize,
    too_many: bool,
) -> Writing {
    if base_stamp == 0 {
        return Writing::Whole;
    }
    if too_many || total > 200_000 {
        return Writing::Whole;
    }
    if new_changes == 0 {
        return Writing::Nothing;
    }
    if total * 50 > entries.max(1) {
        return Writing::Whole;
    }
    Writing::Changes
}

/// One hit of the index, in the shape the wire uses.
fn file_hit(hit: &FsHit) -> FileHit {
    FileHit {
        name: hit.name.clone(),
        path: hit.path.clone(),
        is_dir: hit.is_dir,
        size: hit.size,
        modified: hit.modified,
        score: hit.score,
    }
}

/// Answer the requests that come in through the pipe.
pub fn answer(service: &std::sync::Arc<Service>, request: Request) -> Response {
    match request {
        Request::Query { text, limit } => Response::Hits {
            hits: service.query(&text, limit as usize),
        },
        Request::Status => Response::Status(service.status()),
        Request::Rebuild => {
            // The table is read again in a thread of its own: the answer comes
            // right away, and the work goes on in the background.
            let builder = std::sync::Arc::clone(service);
            std::thread::Builder::new()
                .name("index-rebuild".into())
                .spawn(move || {
                    builder.build();
                })
                .ok();
            Response::Status(service.status())
        }
        Request::Stop => {
            service.stop();
            Response::Status(service.status())
        }
    }
}

/// The pipe the service listens on.
pub fn pipe_name() -> Vec<u16> {
    wide(launcher_core::service::PIPE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_only_written_again_when_it_has_to() {
        // There is no table on the disk yet: the whole thing is written.
        assert_eq!(what_to_write(0, 0, 0, 1000, false), Writing::Whole);
        // A few changes: only the changes are written.
        assert_eq!(what_to_write(1, 10, 10, 1000, false), Writing::Changes);
        // Nothing changed since the last round: not a byte is written.
        assert_eq!(what_to_write(1, 0, 0, 1000, false), Writing::Nothing);
        // The changes are more than a fiftieth of the table: the whole table
        // is written again, and the changes are dropped with it.
        assert_eq!(what_to_write(1, 100, 100, 1000, false), Writing::Whole);
        // More changes than can be kept in memory: the whole table.
        assert_eq!(
            what_to_write(1, 200_001, 200_001, 10_000_000, false),
            Writing::Whole
        );
        // The two tables do not belong together: the whole table.
        assert_eq!(what_to_write(1, 1, 1, 10_000_000, true), Writing::Whole);
    }

    #[test]
    fn the_changes_of_the_earlier_rounds_are_counted_too() {
        // A few new changes on top of many that are already in the file: the
        // changes of **all** the rounds go into the file, and there are few
        // enough of them to be worth writing.
        assert_eq!(what_to_write(1, 10, 100, 10_000, false), Writing::Changes);
        // But when they are many, the whole table is written.
        assert_eq!(what_to_write(1, 10, 600, 10_000, false), Writing::Whole);
    }
}
