//! File and folder searching.
//!
//! Two engines sit behind this plugin:
//!
//! * the **full-disk index**, which reads the NTFS master file table and can
//!   therefore see every file on every volume, the way Everything does, and
//! * the **live index**, which watches the folders of the user and is up to
//!   date within milliseconds: a file that was created a second ago is found
//!   right away, and a file that was deleted is gone from the results.
//!
//! The full-disk index answers with the Everything search syntax, so
//! `size:>1mb`, `dm:today`, `file:`, `*.pdf` and `!folder` all work. See
//! [`launcher_core::search_syntax`].
//!
//! The live index needs no privileges, so it works for a launcher that runs
//! as the user — which is the normal case. The full-disk index needs
//! administrative privileges, and says so when it does not get them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use launcher_core::cancel::Cancel;
use launcher_core::matcher::StringMatcher;
use launcher_core::plugin::{PluginResult, QueryPlugin};
use launcher_core::query::{GLOBAL_PLUGIN_WILDCARD, Query};
use launcher_core::result::Result as LauncherResult;
use launcher_core::result::ResultIcon;
use launcher_core::settings::{FileSort, Settings};
use launcher_index::files::{FileIndex, SearchOptions};
use launcher_index::fsindex::{Exclusions, FsIndex};
use launcher_index::usn::FileMeta;
use launcher_index::watch::Watcher;
use parking_lot::RwLock;

/// How many results the plugin returns when the user has not said otherwise.
///
/// The window shows a handful of rows, but the list is scrolled through — so
/// the results are **not** cut off at the height of the window: a file that
/// is further down has to be findable. Ten thousand is more than any search
/// that is worth looking at, and it keeps a runaway query from filling the
/// memory.
const RESULT_LIMIT: usize = 10_000;

/// How many files may be opened for the metadata of a query.
///
/// A filter on the size or on a timestamp has to look at the file itself.
/// The budget keeps a query such as `size:>1mb` — which matches every file —
/// from opening half a million files.
const METADATA_BUDGET: u64 = 20_000;

/// How long a saved table is used before it is read again.
///
/// Without the change journal following, the table is a picture of the disk
/// from the moment it was taken. A few hours is long enough for the files of
/// one working day and short enough that the table does not drift away.
const CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Why the full-disk index is not there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexProblem {
    /// There is no full-disk index and nothing is wrong.
    None,
    /// The launcher is not running as administrator.
    NeedsAdministrator,
    /// There is no NTFS volume on this machine.
    NoNtfsVolume,
    /// Something else went wrong.
    Failed(String),
}

/// How far the building of the index has come.
#[derive(Debug, Default)]
pub struct IndexProgress {
    /// How many entries the index holds.
    pub entries: AtomicU64,
    /// How many bytes of the master file table have been read.
    pub bytes_done: AtomicU64,
    /// How many bytes the master file table has.
    pub bytes_total: AtomicU64,
    /// What is being read right now.
    pub phase: RwLock<String>,
}

/// A look at the progress, for the user interface.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    /// How many entries the index holds so far.
    pub entries: usize,
    /// How far the work is done, between 0 and 1.
    ///
    /// `None` when the total is not known — then the bar is a moving one.
    pub fraction: Option<f32>,
    /// What is being read right now.
    pub phase: String,
}

/// The file-search plugin.
pub struct FilePlugin {
    /// The full-disk index, once it has been built.
    full: Arc<RwLock<Option<FsIndex>>>,
    /// The index of the user's own folders, kept up to date by the watcher.
    live: Arc<FileIndex>,
    matcher: Arc<StringMatcher>,
    /// The query the user is looking at right now.
    ///
    /// A change in the file system only matters when the name of the file
    /// that changed could match it — otherwise the results would stay the
    /// same, and the window would not have to be redrawn at all.
    last_term: Arc<RwLock<String>>,
    /// How many changes could have an effect on the results.
    relevant: Arc<AtomicU64>,
    /// Set while the full-disk index is being built.
    building: Arc<AtomicBool>,
    /// Whether the thread that follows the change journal is already running.
    ///
    /// There is only ever one of them: before, every rebuild started
    /// another one, and they all read the whole journal of every volume.
    monitor_running: Arc<AtomicBool>,
    /// Whether the thread that follows the change journal can **read** it.
    ///
    /// Reading the journal needs administrative privileges, and the handle of
    /// the volume has to be opened for reading. When that does not work, the
    /// full-disk index is a picture of the disk from the moment it was taken
    /// — and then the folders of the user have to be walked through, because
    /// they are the only ones that are up to date.
    monitor_working: Arc<AtomicBool>,
    /// Whether the thread that follows the change journal has made up its
    /// mind about it.
    ///
    /// The folders of the user are only walked through when the answer is
    /// "no", and waiting for the answer is better than walking through them
    /// for nothing.
    monitor_verdict: Arc<AtomicBool>,
    /// The number of the change in the journal of every volume, at the moment
    /// the index was taken.
    ///
    /// The journal is the only thing that can bring the index up to date
    /// afterwards, and it only knows the changes that came after that
    /// moment. So the numbers are written next to the index and read back
    /// with it.
    snapshot: Arc<RwLock<Vec<(String, i64)>>>,
    /// The changes that are not in the saved table yet.
    ///
    /// The table is written once and the changes are written after it, so
    /// this is the difference between the two. It is what the file with the
    /// changes holds, and it is also what the change journal adds to.
    pending: Arc<std::sync::Mutex<Vec<launcher_index::fsindex::IndexChange>>>,
    /// The number that the saved table was written with.
    ///
    /// The changes are written next to the table, and they only belong to
    /// that one table — so the number has to be remembered, and it is not
    /// the same as the moment the table in memory was read.
    base_stamp: Arc<AtomicU64>,
    /// The service that holds the index of the whole machine.
    ///
    /// When it is there, the launcher asks it for the files and does not
    /// hold the index itself: the window stays a small program.
    service: crate::service_client::ServiceClient,
    /// Why the full-disk index is not there.
    problem: Arc<RwLock<IndexProblem>>,
    /// Where the full-disk index is kept between runs.
    cache: PathBuf,
    /// How many files may still be opened for metadata.
    metadata_budget: Arc<AtomicU64>,
    /// How far the building of the index has come.
    progress: Arc<IndexProgress>,
    /// What the index leaves out.
    exclusions: Arc<RwLock<Exclusions>>,
    /// How the results are ordered.
    sort: Arc<RwLock<FileSort>>,
    /// The volumes that are indexed, by their letter. Empty means all of them.
    volumes: Arc<RwLock<Vec<String>>>,
    /// How long a saved table is used.
    max_age: Arc<RwLock<std::time::Duration>>,
    /// Whether the full-disk index is wanted at all.
    enabled: Arc<AtomicBool>,
    /// How many rows the plugin may return.
    ///
    /// The engine keeps only as many rows as the list can show, so there is
    /// no point in returning more.
    limit: Arc<RwLock<usize>>,
    /// Lets a query that has been overtaken stop early.
    cancel: Cancel,
    /// The watchers that keep the live index up to date.
    watchers: Arc<RwLock<Vec<Watcher>>>,
    /// The folders that are looked at, from the last refresh.
    ///
    /// They are needed when the thread that follows the change journal breaks
    /// down: then the folders of the user have to be walked through after
    /// all, and there is nowhere else to learn which folders they are.
    last_options: Arc<RwLock<Option<SearchOptions>>>,
}

impl FilePlugin {
    /// Take a different way to the service.
    ///
    /// It is for the tests: a machine that has the service would answer
    /// them, and a test that means to look at the work of the launcher alone
    /// would get the index of the whole machine in it.
    pub fn with_service(mut self, service: crate::service_client::ServiceClient) -> Self {
        self.service = service;
        self
    }
    /// Create the plugin over a shared live index.
    pub fn new(index: Arc<FileIndex>) -> Self {        Self {
            full: Arc::new(RwLock::new(None)),
            live: index,
            matcher: Arc::new(StringMatcher::new()),
            last_term: Arc::new(RwLock::new(String::new())),
            relevant: Arc::new(AtomicU64::new(0)),
            building: Arc::new(AtomicBool::new(false)),
            monitor_running: Arc::new(AtomicBool::new(false)),
            monitor_working: Arc::new(AtomicBool::new(false)),
            monitor_verdict: Arc::new(AtomicBool::new(false)),
            snapshot: Arc::new(RwLock::new(Vec::new())),
            pending: Arc::new(std::sync::Mutex::new(Vec::new())),
            base_stamp: Arc::new(AtomicU64::new(0)),
            service: crate::service_client::ServiceClient::new(),
            problem: Arc::new(RwLock::new(IndexProblem::None)),
            cache: std::path::PathBuf::new(),
            metadata_budget: Arc::new(AtomicU64::new(METADATA_BUDGET)),
            progress: Arc::new(IndexProgress::default()),
            exclusions: Arc::new(RwLock::new(Exclusions::default())),
            sort: Arc::new(RwLock::new(FileSort::default())),
            volumes: Arc::new(RwLock::new(Vec::new())),
            max_age: Arc::new(RwLock::new(CACHE_MAX_AGE)),
            enabled: Arc::new(AtomicBool::new(true)),
            limit: Arc::new(RwLock::new(RESULT_LIMIT)),
            cancel: Cancel::new(),
            watchers: Arc::new(RwLock::new(Vec::new())),
            last_options: Arc::new(RwLock::new(None)),
        }
    }

    /// Apply the settings that steer the index.
    pub fn configure(&self, settings: &Settings) {
        self.enabled
            .store(settings.enable_full_disk_index, Ordering::SeqCst);
        *self.exclusions.write() = Exclusions {
            paths: settings
                .excluded_paths
                .iter()
                .map(|path| path.trim().trim_end_matches('\\').to_lowercase())
                .collect(),
            extensions: settings
                .excluded_extensions
                .iter()
                .map(|extension| extension.trim().trim_start_matches('.').to_lowercase())
                .collect(),
        };
        *self.sort.write() = settings.file_sort;
        *self.volumes.write() = settings
            .indexed_volumes
            .iter()
            .map(|volume| volume.trim().trim_end_matches(':').to_uppercase())
            .collect();
        *self.max_age.write() =
            std::time::Duration::from_secs(settings.full_disk_index_max_age_hours * 3600);
        // The list shows a window over the results; the results themselves are
        // all kept, so that scrolling down finds the one that is further
        // down. Only a runaway query is cut off.
        *self.limit.write() = settings.max_results_to_show.clamp(100, RESULT_LIMIT);
    }

    /// Read the table again from the beginning.
    ///
    /// The saved table is thrown away and the master file table is read once
    /// more. This is what the button in the settings and the entry in the tray
    /// menu do.
    ///
    /// The table that is in use stays in use until the new one is there: a
    /// rebuild that cannot be done must not leave the launcher without an
    /// index.
    pub fn rebuild(&self) {
        *self.problem.write() = IndexProblem::None;
        // The service holds the table when it is there, so it is also the one
        // that reads it again.
        if self.service.rebuild() {
            log::info!("the service was asked to read the table again");
            return;
        }
        self.start_full_index(true);
    }

    /// Set where the full-disk index is kept between runs.
    ///
    /// Reading the master file table takes minutes, so the table is written to
    /// a file and read back on the next start.
    pub fn with_cache(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache = path.into();
        self
    }

    /// Share the token that lets an overtaken query stop early.
    ///
    /// The launcher holds one of these and bumps it for every keystroke, so
    /// that the scan of the previous keystroke gives up instead of working for
    /// an answer nobody will see.
    pub fn with_cancel(mut self, cancel: Cancel) -> Self {
        self.cancel = cancel;
        self
    }

    /// Start a background refresh of both indexes.
    ///
    /// The full-disk index reads the master file table and only works with
    /// administrative privileges, so it is built on its own thread and
    /// quietly skipped when it is not possible.
    ///
    /// The folders of the user are **not** walked through when the full-disk
    /// index is there **and** the thread that follows the change journal can
    /// read it: the full-disk index then knows every file of the machine,
    /// including the ones in those folders, and it keeps itself up to date.
    /// Walking through the folders as well would do the same work twice, and
    /// the folders of the user hold more than a million files.
    ///
    /// When the journal cannot be read — no administrative privileges, or a
    /// volume without one — the folders are walked through as before, because
    /// then they are the only ones that are up to date.
    pub fn refresh(&self, options: SearchOptions) -> bool {
        self.start_watcher(&options);
        *self.last_options.write() = Some(options.clone());
        let relevant = self.relevant.clone();
        // The service is asked first. It holds the index of the whole machine
        // **and** keeps it up to date, so the folders of the user are not
        // walked through at all: they hold more than a million files, the walk
        // takes a quarter of a minute, and while it runs the window is busy
        // enough that moving the mouse over the list is slow.
        //
        // The question is answered over the pipe, and the pipe is right
        // there, so it costs nothing.
        let service = self.service.clone();
        {
            let live = self.live.clone();
            let full = self.full.clone();
            let working = self.monitor_working.clone();
            let verdict = self.monitor_verdict.clone();
            let relevant = relevant.clone();
            let options = options.clone();
            std::thread::Builder::new()
                .name("file-scan-later".into())
                .spawn(move || {
                    if !service.is_definitely_gone() {
                        log::info!(
                            "the service holds the index of the machine — the folders of the user \
                             are not walked through"
                        );
                        live.mark_finished();
                        relevant.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                    // The full-disk index is read back from its cache in half
                    // a second, so it is given that moment. If it is not
                    // there — a fresh build needs the privileges and a
                    // quarter of an hour — the folders of the user are
                    // walked through as before.
                    let deadline = std::time::Instant::now()
                        + std::time::Duration::from_millis(2_500);
                    while std::time::Instant::now() < deadline {
                        if full.read().is_some() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    if full.read().is_some() {
                        // The index is there. Now the question is whether the
                        // thread that follows the change journal can read it
                        // — only then is the index able to keep itself up to
                        // date, and only then are the folders of the user
                        // covered by it.
                        let deadline = std::time::Instant::now()
                            + std::time::Duration::from_secs(15);
                        while std::time::Instant::now() < deadline {
                            if verdict.load(Ordering::SeqCst) {
                                break;
                            }
                            // The service may have come up in the meantime —
                            // and it keeps the index up to date just as well.
                            if service.is_there() {
                                log::info!(
                                    "the service holds the index of the machine — the folders of \
                                     the user are not walked through"
                                );
                                live.mark_finished();
                                relevant.fetch_add(1, Ordering::SeqCst);
                                return;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        if working.load(Ordering::SeqCst) {
                            log::info!(
                                "the full-disk index is there and keeps itself up to date — the \
                                 folders of the user are not walked through again"
                            );
                            // There is nothing to walk through, and the window
                            // must not wait for a walk that is not going to
                            // happen.
                            live.mark_finished();
                            // The results on the screen are complete now.
                            relevant.fetch_add(1, Ordering::SeqCst);
                            return;
                        }
                        log::info!(
                            "the change journal cannot be read — the folders of the user are \
                             walked through after all"
                        );
                    }
                    // **One last look before the walk.** The waits above take
                    // seconds, and the service may well have come up in the
                    // meantime. The walk reads more than a million files and
                    // takes three quarters of a minute — and it does that
                    // while the user is looking at the results.
                    if !service.is_definitely_gone() {
                        log::info!(
                            "the service holds the index of the machine — the folders of the user \
                             are not walked through"
                        );
                        live.mark_finished();
                        relevant.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                    // The index of the user's folders is filled while it is
                    // walked through, so the results on the screen grow with
                    // it. The window is told about it now and then, so that
                    // the query runs again and the new files show up.
                    {
                        let live = live.clone();
                        let relevant = relevant.clone();
                        std::thread::Builder::new()
                            .name("file-scan-ticker".into())
                            .spawn(move || {
                                let mut seen = live.version();
                                while live.is_refreshing() {
                                    std::thread::sleep(std::time::Duration::from_millis(700));
                                    let now = live.version();
                                    if now != seen {
                                        seen = now;
                                        relevant.fetch_add(1, Ordering::SeqCst);
                                    }
                                }
                            })
                            .ok();
                    }
                    live.refresh_with(options, move || {
                        // The scan found the files that were there before the
                        // launcher started — the watcher cannot report those.
                        // The results may have changed, so the query runs
                        // again.
                        relevant.fetch_add(1, Ordering::SeqCst);
                    });
                })
                .ok();
        }
        self.start_full_index(false);
        true
    }

    /// Watch the folders of the user, so that the index is never out of date.
    ///
    /// The watcher needs no privileges, so this works without an elevated
    /// launcher. It is the reason a file that was created a moment ago is
    /// found right away.
    fn start_watcher(&self, options: &SearchOptions) {
        if !self.watchers.read().is_empty() {
            return;
        }
        self.live.set_excluded(options.excluded_directories.clone());
        // The home folder comes first: the folders in it are watched with it.
        let mut roots: Vec<PathBuf> = Vec::new();
        roots.extend(options.shallow_roots.iter().cloned());
        roots.extend(options.roots.iter().cloned());
        let matcher = self.matcher.clone();
        let last_term = self.last_term.clone();
        let relevant = self.relevant.clone();
        let mut watchers = Vec::new();
        for (index, root) in roots.iter().enumerate() {
            // A folder that lies in one of the ones before it is watched with
            // it, and does not need a watcher of its own.
            if roots[..index].iter().any(|outer| root.starts_with(outer)) {
                continue;
            }
            let watcher = {
                let matcher = matcher.clone();
                let last_term = last_term.clone();
                let relevant = relevant.clone();
                match self.live.watch_with(root.clone(), move |path, action| {
                    // Only a change that could show up in the results wakes
                    // the window up. The machine writes files all the time,
                    // and redrawing the list for each of them would make the
                    // window flicker.
                    if may_match(&matcher, &last_term, &path) {
                        log::debug!("a change that matters: {}", path.display());
                        relevant.fetch_add(1, Ordering::SeqCst);
                    }
                    let _ = action;
                }) {
                    Some(watcher) => watcher,
                    None => {
                        log::warn!("{} could not be watched", root.display());
                        continue;
                    }
                }
            };
            watchers.push(watcher);
        }
        if watchers.is_empty() {
            return;
        }
        *self.watchers.write() = watchers;
    }

    /// Build the full-disk index in the background.
    ///
    /// A table that was written by an earlier run is used when it is still
    /// fresh and the set of volumes has not changed.
    ///
    /// With `force` the saved table is ignored: that is what a rebuild does.
    /// The table that is in use stays in use until the new one is ready.
    pub fn start_full_index(&self, force: bool) -> bool {
        if !self.enabled.load(Ordering::SeqCst) {
            return false;
        }
        if self.building.swap(true, Ordering::SeqCst) {
            return false;
        }
        if !force && self.full.read().is_some() {
            self.building.store(false, Ordering::SeqCst);
            return false;
        }
        let full = self.full.clone();
        let building = self.building.clone();
        let monitor_running = self.monitor_running.clone();
        let monitor_working = self.monitor_working.clone();
        let monitor_verdict = self.monitor_verdict.clone();
        let snapshot = self.snapshot.clone();
        let pending = self.pending.clone();
        let base_stamp = self.base_stamp.clone();
        let service = self.service.clone();
        let relevant = self.relevant.clone();
        let on_broken = self.fallback_scan();
        let problem = self.problem.clone();
        let cache = self.cache.clone();
        let progress = self.progress.clone();
        // The settings are read here, because the thread outlives this call.
        let exclusions = self.exclusions.read().clone();
        let volumes = self.volumes.read().clone();
        let max_age = *self.max_age.read();
        progress.entries.store(0, Ordering::Relaxed);
        progress.bytes_done.store(0, Ordering::Relaxed);
        progress.bytes_total.store(0, Ordering::Relaxed);
        *progress.phase.write() = "准备中".to_string();
        std::thread::Builder::new()
            .name("fs-index".into())
            .spawn(move || {
                // The service holds the index of the whole machine. When it
                // is there, the launcher does not read the disk and does
                // not write to it: it asks the service for the files.
                //
                // The service is asked twice: it may be on its way up — it
                // is started with the machine, and the launcher may be
                // started at the same time — and being wrong about it costs
                // the whole table of the disk.
                let mut service_ready = service.is_there();
                for _ in 0..6 {
                    if service_ready {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    service_ready = service.is_there();
                }
                if service_ready {
                    let status = service.status();
                    log::info!(
                        "the service holds the index of the machine — the launcher does not read \
                         the disk itself"
                    );
                    if let Some(status) = status {
                        launcher_core::profile::count("index.fs.entries", status.entries);
                        progress.entries.store(status.entries, Ordering::Relaxed);
                    }
                    building.store(false, Ordering::SeqCst);
                    return;
                }
                if !force {
                    // A table from an earlier run saves the minutes that
                    // reading the master file table takes.
                    let mut changes_read = Vec::new();
                    match load_cached_index(&cache, max_age, &mut changes_read) {
                        CachedIndex::Fresh(index, points) => {
                            log::info!(
                                "the full-disk index was loaded from the cache: {} entries",
                                index.len()
                            );
                            launcher_core::profile::count("index.fs.entries", index.len() as u64);
                            progress.entries.store(index.len() as u64, Ordering::Relaxed);
                            *snapshot.write() = points;
                            // The changes that came after the table was saved
                            // are remembered, so that they can be written
                            // again after the next build.
                            if let Ok(mut pending) = pending.lock() {
                                pending.extend(changes_read);
                            }
                            // The number the saved table was written with:
                            // the changes belong to that table.
                            base_stamp.store(index.built_at(), Ordering::Relaxed);
                            *full.write() = Some(index);
                            building.store(false, Ordering::SeqCst);
                            // The change journal is followed even when the table
                            // came from the cache: without it the index would
                            // be a picture of the disk from the moment it was taken.
                            if full.read().is_some() {
                                spawn_monitor(
                                    full.clone(),
                                    monitor_running.clone(),
                                    monitor_working.clone(),
                                    monitor_verdict.clone(),
                                    snapshot.clone(),
                                    pending.clone(),
                                    relevant.clone(),
                                    on_broken.clone(),
                                );
                            }
                            return;
                        }
                        CachedIndex::Stale(index, points) => {
                            // The table is too old to be trusted, but it is a lot
                            // better than nothing: reading the master file table
                            // again needs administrative privileges, and without
                            // them the file search would be gone entirely.
                            log::info!(
                                "the saved full-disk index is too old, but it is used until the \
                                 master file table can be read again: {} entries",
                                index.len()
                            );
                            launcher_core::profile::count("index.fs.entries", index.len() as u64);
                            progress.entries.store(index.len() as u64, Ordering::Relaxed);
                            *snapshot.write() = points;
                            *full.write() = Some(index);
                        }
                        CachedIndex::None => {}
                    }
                }
                // Whether there is a table on the disk to build on.
                let has_saved = full.read().is_some();
                match {
                    let _timer = launcher_core::profile::phase("index.fs.build");
                    build_full_index(&progress, &exclusions, &volumes)
                } {
                    Ok((index, points)) => {
                        let entries = index.len();
                        // The difference between the table that is in memory
                        // and the one that was just read.
                        let fresh_changes = {
                            let guard = full.read();
                            match guard.as_ref() {
                                Some(older) => index.changes_since(older),
                                None => Vec::new(),
                            }
                        };
                        // The table in memory was read just now, so it is as
                        // fresh as it can be. The moment is written into it,
                        // so that its age is the age of the reading and not
                        // the age of the saved table.
                        let now = index.stamp();
                        let mut index = index;
                        index.set_fresh_at(now);
                        *snapshot.write() = points.clone();
                        *full.write() = Some(index);
                        // The changes that are not in the saved table yet:
                        // the ones that were read back when the program
                        // started, the ones the change journal brought in,
                        // and the ones that were just found.
                        let mut to_write = pending.lock().map(|list| list.clone()).unwrap_or_default();
                        to_write.extend(fresh_changes.iter().cloned());
                        if !has_saved {
                            // There was no table to compare with, so the whole
                            // thing is written.
                            if let Some(index) = full.read().as_ref() {
                                match index.save(&cache) {
                                    Err(error) => {
                                        log::warn!(
                                            "the full-disk index could not be saved: {error}"
                                        );
                                    }
                                    Ok(stamp) => {
                                        log::info!(
                                            "the full-disk index was saved: {entries} entries"
                                        );
                                        // The changes belong to the table that
                                        // was just written.
                                        base_stamp.store(stamp, Ordering::Relaxed);
                                        let _ = std::fs::remove_file(changes_path(&cache));
                                        if let Ok(mut pending) = pending.lock() {
                                            pending.clear();
                                        }
                                        if let Err(error) =
                                            save_snapshot_point(&cache, stamp, &points)
                                        {
                                            log::warn!(
                                                "the point in the change journal was not saved: \
                                                 {error}"
                                            );
                                        }
                                    }
                                }
                            }
                        } else if to_write.is_empty() {
                            // Nothing on the disk has changed since the table
                            // was saved, so the saved table is still the right
                            // one — and it does not have to be written again.
                            //
                            // That write is the biggest one the program ever
                            // does: three quarters of a gigabyte for a big
                            // volume, four times a day. It is also the one
                            // that wears the disk.
                            log::info!(
                                "the full-disk index is unchanged — the saved table was left alone"
                            );
                        } else if to_write.len() * 50 > entries.max(1) {
                            // The changes are many — more than a fiftieth of
                            // the table. Then the table is written again, and
                            // the changes are dropped: that keeps the files on
                            // the disk small, and the list of changes in
                            // memory with them.
                            if let Some(index) = full.read().as_ref() {
                                match index.save(&cache) {
                                    Err(error) => {
                                        log::warn!(
                                            "the full-disk index could not be saved: {error}"
                                        );
                                    }
                                    Ok(written) => {
                                        log::info!(
                                            "the full-disk index was written again: {entries} \
                                             entries, {} changes"
                                            , to_write.len()
                                        );
                                        base_stamp.store(written, Ordering::Relaxed);
                                        let _ = std::fs::remove_file(changes_path(&cache));
                                        if let Ok(mut pending) = pending.lock() {
                                            pending.clear();
                                        }
                                        if let Err(error) =
                                            save_snapshot_point(&cache, written, &points)
                                        {
                                            log::warn!(
                                                "the point in the change journal was not saved: \
                                                 {error}"
                                            );
                                        }
                                    }
                                }
                            }
                        } else {
                            // Only the changes are written. That is a few
                            // megabytes instead of three quarters of a
                            // gigabyte, and it is what the disk is asked to
                            // do from now on.
                            let base_stamp = base_stamp.load(Ordering::Relaxed);
                            if let Some(index) = full.read().as_ref() {
                                match index.save_changes(
                                    &changes_path(&cache),
                                    base_stamp,
                                    &to_write,
                                ) {
                                    Err(error) => {
                                        log::warn!("the changes could not be saved: {error}");
                                    }
                                    Ok(()) => {
                                        log::info!(
                                            "only the changes were saved: {} of {entries} entries",
                                            to_write.len()
                                        );
                                        if let Ok(mut pending) = pending.lock() {
                                            *pending = to_write;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(issue) => {
                        if full.read().is_some() {
                            // The old table is still there, so the file search
                            // keeps working. Only the reason is written down.
                            log::warn!(
                                "the full-disk index could not be rebuilt ({issue:?}) — the saved \
                                 one is used"
                            );
                            *problem.write() = issue;
                        } else {
                            log::warn!("the full-disk index could not be built: {issue:?}");
                            *problem.write() = issue;
                        }
                    }
                }
                building.store(false, Ordering::SeqCst);
                // The change journal is followed once the index is there.
                if full.read().is_some() {
                    spawn_monitor(
                        full.clone(),
                        monitor_running.clone(),
                        monitor_working.clone(),
                        monitor_verdict.clone(),
                        snapshot.clone(),
                        pending.clone(),
                        relevant.clone(),
                        on_broken.clone(),
                    );
                }
            })
            .ok();
        true
    }

    /// Follow the change journal and keep the index up to date.
    ///
    /// This is what Everything does with
    /// `_db_monitor_ntfs_process_usn_records`: without it the index would be
    /// a picture of the disk from the moment it was taken, and a file that
    /// was created a minute ago would not be found.
    pub fn start_monitor(&self) -> bool {
        if !self.enabled.load(Ordering::SeqCst) || !self.has_full_index() {
            return false;
        }
        spawn_monitor(
            self.full.clone(),
            self.monitor_running.clone(),
            self.monitor_working.clone(),
            self.monitor_verdict.clone(),
            self.snapshot.clone(),
            self.pending.clone(),
            self.relevant.clone(),
            self.fallback_scan(),
        )
    }

    /// What the launcher does when the thread that follows the change journal
    /// breaks down.
    ///
    /// The folders of the user are then the only ones that are up to date, so
    /// they are walked through after all.
    fn fallback_scan(&self) -> Arc<dyn Fn() + Send + Sync> {
        let live = self.live.clone();
        let options = self.last_options.clone();
        let relevant = self.relevant.clone();
        let service = self.service.clone();
        Arc::new(move || {
            // **The service holds the index of the whole machine**, so there is
            // nothing to walk through: the walk reads more than a million
            // files and takes three quarters of a minute, and while it runs
            // the disk is busy and the window is slow.
            //
            // This is checked here as well as in the place that decides to
            // walk, because this is the last line of defence: a walk that
            // happens by accident is very expensive.
            //
            // The question is not "is the service there **right now**" but
            // "is the service **provably** gone" — a pipe that is busy for a
            // moment looks exactly like a service that is not there, and the
            // walk must not start on the word of that.
            if !service.is_definitely_gone() {
                log::info!(
                    "the service holds the index of the machine — the folders of the user \
                     are not walked through"
                );
                live.mark_finished();
                relevant.fetch_add(1, Ordering::SeqCst);
                return;
            }
            let Some(options) = options.read().clone() else {
                log::info!(
                    "the folders of the user are not walked through: there are no options for it"
                );
                live.mark_finished();
                return;
            };
            log::info!("the service is gone — the folders of the user are walked through");
            let live = live.clone();
            let relevant = relevant.clone();
            live.refresh_with(options, move || {
                relevant.fetch_add(1, Ordering::SeqCst);
            });
        })
    }

    /// True when the full-disk index is ready.
    pub fn has_full_index(&self) -> bool {
        self.full.read().is_some()
    }

    /// True when the full-disk index could not be built because the launcher
    /// is not running as administrator.
    pub fn needs_administrator(&self) -> bool {
        matches!(&*self.problem.read(), IndexProblem::NeedsAdministrator)
    }

    /// Why the full-disk index is not there.
    pub fn problem(&self) -> IndexProblem {
        self.problem.read().clone()
    }

    /// Whether the full-disk index is still being built.
    pub fn is_building(&self) -> bool {
        self.building.load(Ordering::SeqCst)
    }

    /// How many entries the full-disk index holds.
    pub fn full_len(&self) -> usize {
        self.full.read().as_ref().map(|i| i.len()).unwrap_or(0)
    }

    /// How far the building of the index has come.
    pub fn progress(&self) -> Progress {
        let entries = self.progress.entries.load(Ordering::Relaxed) as usize;
        let done = self.progress.bytes_done.load(Ordering::Relaxed);
        let total = self.progress.bytes_total.load(Ordering::Relaxed);
        let fraction = if total > 0 {
            Some((done as f32 / total as f32).clamp(0.0, 1.0))
        } else {
            None
        };
        Progress {
            entries,
            fraction,
            phase: self.progress.phase.read().clone(),
        }
    }

    /// How many entries the live index holds.
    pub fn live_len(&self) -> usize {
        self.live.len()
    }

    /// How many changes the live index has seen.
    ///
    /// A query that was answered before a change is out of date.
    pub fn live_version(&self) -> u64 {
        self.live.version()
    }

    /// How many changes could have an effect on the results.
    ///
    /// Only the changes whose name could match the query the user is looking
    /// at are counted — the window is only redrawn for those.
    pub fn relevant_version(&self) -> u64 {
        self.relevant.load(Ordering::SeqCst)
    }

    /// Whether the scan of the user's folders is still running.
    pub fn is_scanning(&self) -> bool {
        !self.live.progress().done.load(Ordering::SeqCst)
    }

    /// How many entries the scan of the user's folders has found so far.
    pub fn scanned(&self) -> usize {
        self.live.progress().found.load(Ordering::Relaxed) as usize
    }

    /// Whether a refresh of the live index is running.
    pub fn is_refreshing(&self) -> bool {
        self.live.is_refreshing()
    }

    /// How many entries the live index holds.
    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Forget the saved table, so the next start reads the table again.
    pub fn clear_cache(&self) {
        let _ = std::fs::remove_file(&self.cache);
    }
}

/// Follow the change journal of every volume and keep the index alive.
///
/// There is **one** of these threads, and it never ends. Before, every
/// rebuild started another one — and each of them read the whole change
/// journal of every volume every five seconds and wrote into the same index.
fn spawn_monitor(
    full: Arc<RwLock<Option<FsIndex>>>,
    running: Arc<std::sync::atomic::AtomicBool>,
    working: Arc<std::sync::atomic::AtomicBool>,
    verdict: Arc<std::sync::atomic::AtomicBool>,
    snapshot: Arc<RwLock<Vec<(String, i64)>>>,
    pending: Arc<std::sync::Mutex<Vec<launcher_index::fsindex::IndexChange>>>,
    relevant: Arc<AtomicU64>,
    on_broken: Arc<dyn Fn() + Send + Sync>,
) -> bool {
    let volumes = launcher_index::usn::ntfs_volumes();
    if volumes.is_empty() {
        // The launcher has to know that there is no journal, or it waits for
        // an answer that never comes.
        verdict.store(true, Ordering::SeqCst);
        working.store(false, Ordering::SeqCst);
        return false;
    }
    if running.swap(true, Ordering::SeqCst) {
        log::info!("the thread that follows the change journal is already running");
        return true;
    }
    std::thread::Builder::new()
        .name("fs-monitor".into())
        .spawn(move || {
            // The number of the change that was handled last, per volume.
            let mut seen: Vec<i64> = vec![0; volumes.len()];
            // The point the index was taken at — the changes that came after
            // it are in the journal, and that is where the reading starts.
            let mut base: Vec<(String, i64)> = Vec::new();
            // Which volumes could not be read, so that it is only said once.
            let mut broken: Vec<bool> = vec![false; volumes.len()];
            // Whether the reason why the journal cannot be read was said.
            let mut said_why = false;
            loop {
                // A new index was built: the reading starts at the point that
                // one was taken at.
                let points = snapshot.read().clone();
                if points != base {
                    base = points.clone();
                    for (index, volume) in volumes.iter().enumerate() {
                        seen[index] = points
                            .iter()
                            .find(|(root, _)| root.eq_ignore_ascii_case(&volume.root))
                            .map(|(_, usn)| *usn)
                            .unwrap_or(0);
                    }
                    log::info!(
                        "the thread that follows the change journal starts at the point the \
                         index was taken at"
                    );
                }
                let mut any_read = false;
                let mut behind = false;
                for (index, volume) in volumes.iter().enumerate() {
                    // Where the journal stands **before** it is read: the
                    // reading has to reach that point, or the journal is
                    // read wrongly.
                    //
                    // `first_usn` is the number of the oldest record that is
                    // still in the journal: everything between the snapshot
                    // and it is gone for good, and then the index cannot be
                    // brought up to date — the folders of the user have to be
                    // walked through after all.
                    let Some(state) = launcher_index::usn::journal_state(volume) else {
                        // The volume has no change journal — it is not NTFS,
                        // or the journal was turned off. Nothing to follow
                        // there.
                        if !broken[index] {
                            broken[index] = true;
                            log::info!(
                                "{} has no change journal — the files of it stay as they were",
                                volume.root
                            );
                        }
                        continue;
                    };
                    let head = state.next_usn;
                    let oldest = state.first_usn;
                    let point = base
                        .iter()
                        .find(|(root, _)| root.eq_ignore_ascii_case(&volume.root))
                        .map(|(_, usn)| *usn)
                        .unwrap_or(0);
                    if point > 0 && oldest > point + 1 {
                        if !broken[index] {
                            broken[index] = true;
                            log::warn!(
                                "the change journal of {} does not hold the changes that came \
                                 after the index was taken — the index of that volume stays as it \
                                 is",
                                volume.root
                            );
                        }
                        continue;
                    }
                    // The journal is read until it has nothing more to say —
                    // a megabyte at a time, and as fast as the disk hands it
                    // over.
                    loop {
                        let read = launcher_index::usn::read_journal(volume, seen[index]);
                        let (changes, next) = match read {
                            Ok(pair) => pair,
                            Err(code) => {
                                if !broken[index] {
                                    broken[index] = true;
                                    log::warn!(
                                        "the change journal of {} cannot be read (error {code}) — \
                                         the files of that volume stay as they were",
                                        volume.root
                                    );
                                }
                                break;
                            }
                        };
                        any_read = true;
                        if broken[index] {
                            broken[index] = false;
                            log::info!("the change journal of {} is read again", volume.root);
                        }
                        if next <= seen[index] {
                            // There is nothing new in it.
                            break;
                        }
                        seen[index] = next;
                        if changes.is_empty() {
                            continue;
                        }
                        // The changes are read **outside** the lock: the lock
                        // belongs to the drawing thread, and it must not wait
                        // for a change journal with thousands of entries in it.
                        let mut guard = full.write();
                        let Some(index) = guard.as_mut() else {
                            break;
                        };
                        let touched = index.apply_changes(&changes);
                        if touched > 0 {
                            log::info!("the change journal brought {touched} entries up to date");
                            relevant.fetch_add(1, Ordering::SeqCst);
                        }
                        // The changes are remembered, so that they can be
                        // written to the disk: the table that was saved does
                        // not have them.
                        if let Ok(mut pending) = pending.lock() {
                            pending.extend(journal_changes(&changes));
                        }
                    }
                    if seen[index] + 1 < head {
                        behind = true;
                    }
                }
                // The index is only up to date when the journal could be read
                // **and** the reading reached the end of it. A volume that is
                // still behind has a journal that is read wrongly.
                if any_read && !behind {
                    if !working.swap(true, Ordering::SeqCst) {
                        log::info!(
                            "the change journal is read — the full-disk index keeps itself up to \
                             date"
                        );
                    }
                } else if working.swap(false, Ordering::SeqCst) {
                    log::warn!(
                        "the change journal is not read — the full-disk index keeps what it has"
                    );
                    // The folders of the user have to take over: they are the
                    // only ones that are up to date now.
                    on_broken();
                } else if !said_why {
                    // The first round says it once, whatever the answer is.
                    said_why = true;
                    log::warn!(
                        "the change journal of this system cannot be read — the full-disk index \
                         keeps what it has, and the folders of the user are watched instead"
                    );
                }
                verdict.store(true, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        })
        .ok();
    true
}

/// What a saved table can be used for.
enum CachedIndex {
    /// It is young enough to be used as it is.
    Fresh(FsIndex, Vec<(String, i64)>),
    /// It is too old to be trusted, but it is still better than nothing —
    /// reading the master file table again needs administrative privileges,
    /// and without them the files would disappear entirely.
    Stale(FsIndex, Vec<(String, i64)>),
    /// There is none, or it belongs to other volumes.
    None,
}

/// The point in the change journal that a saved table was taken at.
///
/// The journal is the only thing that can bring a table up to date after it
/// was taken, and the journal only knows the changes that came after that
/// moment. So the numbers are written next to the table and read back with
/// it.
///
/// The file is a small text file:
///
/// ```text
/// stamp 1794139203
/// C:\ 37566430240
/// D:\ 0
/// ```
fn snapshot_point_path(cache: &std::path::Path) -> std::path::PathBuf {
    cache.with_extension("point")
}

/// Write the point that a table was taken at.
fn save_snapshot_point(
    cache: &std::path::Path,
    stamp: u64,
    points: &[(String, i64)],
) -> std::io::Result<()> {
    let mut text = format!("stamp {stamp}\n");
    for (root, usn) in points {
        text.push_str(&format!("{root} {usn}\n"));
    }
    let path = snapshot_point_path(cache);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

/// Read the point that a table was taken at.
///
/// The stamp says which table the point belongs to: a point that was written
/// for another table is of no use, because the changes it names have been
/// applied to that one already.
fn load_snapshot_point(cache: &std::path::Path, stamp: u64) -> Vec<(String, i64)> {
    let path = snapshot_point_path(cache);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut points = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("stamp ") {
            if rest.trim().parse::<u64>().unwrap_or(0) != stamp {
                // The point belongs to another table.
                return Vec::new();
            }
            continue;
        }
        let Some((root, usn)) = line.rsplit_once(' ') else {
            continue;
        };
        let Ok(usn) = usn.trim().parse::<i64>() else {
            continue;
        };
        points.push((root.trim().to_string(), usn));
    }
    points
}

/// The file with the changes since the saved table.
///
/// It sits next to the table itself: `fs-index.bin` and `fs-index.delta`.
fn changes_path(path: &std::path::Path) -> std::path::PathBuf {
    path.with_extension("delta")
}

/// Turn the changes of the change journal into the changes that are written
/// to the disk.
///
/// The journal does not carry the size and the times, so those are left
/// alone: the values that are in the table stay.
fn journal_changes(
    changes: &[launcher_index::usn::JournalChange],
) -> Vec<launcher_index::fsindex::IndexChange> {
    launcher_index::fsindex::journal_changes(changes)
}

/// Read a saved table back, when it is still usable.
fn load_cached_index(
    path: &PathBuf,
    max_age: std::time::Duration,
    changes: &mut Vec<launcher_index::fsindex::IndexChange>,
) -> CachedIndex {
    if path.as_os_str().is_empty() {
        return CachedIndex::None;
    }
    let index = {
        let _timer = launcher_core::profile::phase("index.fs.cache_load");
        FsIndex::load(path)
    };
    let Some(mut index) = index else {
        return CachedIndex::None;
    };
    // The changes that came after the table was saved are read back and
    // applied to it.
    //
    // They are the difference between the saved table and the disk, and they
    // are a few megabytes — where the table itself is three quarters of a
    // gigabyte. That is the whole point: the table is written once, and from
    // then on only what changed is written.
    match FsIndex::load_changes(&changes_path(path), index.built_at()) {
        Some((written_at, read_back)) => {
            let touched = index.apply_index_changes(&read_back);
            index.set_fresh_at(written_at);
            changes.extend(read_back);
            log::info!(
                "the changes since the saved table were applied: {touched} entries, written at \
                 {written_at}"
            );
        }
        None => {}
    }
    // The point in the journal that the table was taken at. Without it the
    // journal has to be read from its beginning, which is slower but still
    // correct.
    let points = load_snapshot_point(path, index.built_at());
    // The table is only good while the volumes are the same.
    let volumes: Vec<String> = launcher_index::usn::ntfs_volumes()
        .iter()
        .map(|v| format!("{}|{}", v.device, v.root))
        .collect();
    if volumes.is_empty() || volumes != index.volume_signature() {
        return CachedIndex::None;
    }
    // And only while it is young enough. A table that is read after a week
    // would be a table of files that no longer exist.
    //
    // An age of zero means that the table is never read again on its own:
    // the button in the settings is then the only way to bring it up to date.
    let age = index.age();
    if !max_age.is_zero() && age > max_age {
        log::info!("the saved full-disk index is {age:?} old — reading it again");
        return CachedIndex::Stale(index, points);
    }
    log::info!("the saved full-disk index is {age:?} old — using it");
    CachedIndex::Fresh(index, points)
}

/// Read the master file table of every NTFS volume.
///
/// The point in the change journal that the table was taken at comes with it,
/// so that the thread that follows the journal knows where to start.
fn build_full_index(
    progress: &IndexProgress,
    exclusions: &Exclusions,
    wanted: &[String],
) -> Result<(FsIndex, Vec<(String, i64)>), IndexProblem> {
    let mut volumes = launcher_index::usn::ntfs_volumes();
    if volumes.is_empty() {
        return Err(IndexProblem::NoNtfsVolume);
    }
    // The user may pick the volumes that are indexed.
    if !wanted.is_empty() {
        volumes.retain(|volume| {
            let letter = volume
                .device
                .trim_start_matches("\\\\.\\")
                .trim_end_matches(':')
                .to_uppercase();
            wanted.iter().any(|pick| pick.trim_end_matches(':') == letter)
        });
        if volumes.is_empty() {
            return Err(IndexProblem::NoNtfsVolume);
        }
    }
    // The entries are handed over to the index as they are read, so the flat
    // list of them never exists on its own: the memory stays at the size of
    // the index.
    let mut needs_admin = false;
    let mut failure = None;
    let mut indexed = 0usize;
    // How many entries the volumes that were read before hold.
    let mut base = 0u64;
    // The point in the change journal that the index is taken at.
    //
    // It is asked **before** the table is read: every change that comes after
    // this moment is in the journal, and the thread that follows the journal
    // starts there. The changes that came while the table was read are in the
    // journal as well, so nothing is lost.
    let points: Vec<(String, i64)> = volumes
        .iter()
        .map(|volume| {
            let usn = launcher_index::usn::journal_state(volume)
                .map(|state| state.next_usn)
                .unwrap_or(0);
            (volume.root.clone(), usn)
        })
        .collect();
    let index = FsIndex::build_from(&volumes, exclusions, |sink| {
        for (index, volume) in volumes.iter().enumerate() {
            let report = |count: usize| {
                progress
                    .entries
                    .store(base + count as u64, Ordering::Relaxed);
            };
            // The fast path: the table is read straight off the disk, in one go.
            sink.begin_volume(index as u32, true);
            *progress.phase.write() = format!("读取主文件表 {}", volume.root);
            let read = launcher_index::usn::enumerate_mft_with_bytes(
                volume,
                Some(&report),
                Some(&|done: u64, total: u64| {
                    progress.bytes_done.store(done, Ordering::Relaxed);
                    progress.bytes_total.store(total, Ordering::Relaxed);
                }),
                |name, fields| sink.add(name, &fields),
            );
            match read {
                Ok(_) => {
                    sink.end_volume();
                    indexed += 1;
                    base = progress.entries.load(Ordering::Relaxed);
                    continue;
                }
                Err(error) => {
                    log::info!(
                        "{} could not be read straight off the disk ({error}) — using the change journal",
                        volume.root
                    );
                }
            }
            // The slow path: the change journal.
            sink.begin_volume(index as u32, false);
            *progress.phase.write() = format!("读取变更日志 {}", volume.root);
            match launcher_index::usn::enumerate_volume_with(
                volume,
                Some(&report),
                |name, entry| sink.add_named(name, &entry),
            ) {
                Ok(_) => {
                    sink.end_volume();
                    indexed += 1;
                    base = progress.entries.load(Ordering::Relaxed);
                }
                Err(launcher_index::usn::UsnError::NeedsAdministrator(code)) => {
                    needs_admin = true;
                    failure = Some(format!("{} 需要管理员权限({code})", volume.root));
                }
                Err(error) => {
                    failure = Some(format!("{} {error}", volume.root));
                }
            }
        }
    });
    if indexed == 0 {
        return Err(if needs_admin {
            IndexProblem::NeedsAdministrator
        } else {
            IndexProblem::Failed(failure.unwrap_or_else(|| "没有可索引的卷".to_string()))
        });
    }
    Ok((index, points))
}

impl QueryPlugin for FilePlugin {
    fn id(&self) -> &str {
        "file"
    }

    fn name(&self) -> &str {
        "Files"
    }

    fn action_keywords(&self) -> &[String] {
        static KEYWORDS: std::sync::LazyLock<Vec<String>> =
            std::sync::LazyLock::new(|| vec![GLOBAL_PLUGIN_WILDCARD.to_string()]);
        &KEYWORDS
    }

    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
        let term = query.search.trim();
        // The watcher needs to know what the user is looking at, so that it
        // can tell whether a change is worth waking the window up for.
        *self.last_term.write() = term.to_string();
        if term.is_empty() {
            return Ok(Vec::new());
        }
        let limit = *self.limit.read();
        // The budget for opening files is **for one query**: a query that
        // filters on the size or on a date opens as many files as it needs,
        // and the next query starts with a full budget again. Before, the
        // budget was for the whole session, and after twenty thousand files
        // the filters stopped working for the rest of the day.
        self.metadata_budget.store(METADATA_BUDGET, Ordering::Relaxed);
        let mut results: Vec<LauncherResult> = Vec::new();
        // The same file can be in both indexes; the first one that is found
        // is the one that is kept.
        // The paths that are already in the list are remembered as **hashes**:
        // a query with ten thousand hits must not make ten thousand copies of
        // a path just to throw them away again.
        let mut seen: HashSet<u64> = HashSet::new();

        // The index of the user's folders comes **first**.
        //
        // When the full-disk index is there, that small index holds the
        // files that changed just now — the watcher put them there — and
        // those are the fresh ones. The full-disk index is a picture of the
        // disk from the moment it was taken, and it is up to five seconds
        // old. So the fresh answer has to be the one that is kept, and the
        // paths are remembered as **hashes**: a query with ten thousand hits
        // must not make ten thousand copies of a path just to throw them
        // away again.
        for result in self.query_live(term, limit, query.generation) {
            if seen.insert(launcher_index::files::path_key(Path::new(&result.copy_text))) {
                results.push(result);
            }
        }

        // The full-disk index knows every file of the machine. What the small
        // index does not know is asked of it — the same file is only asked
        // for once.
        {
            // The service holds the index of the whole machine. When it is
            // there, the launcher does not hold that index itself: it asks,
            // and the answer comes back in a millisecond.
            //
            // **The pipe carries at most a thousand hits.** The search
            // itself walks the whole index — nine million entries — but
            // the *answer* is a thousand names and paths at most. Every
            // hit costs two string allocations on each side of the pipe
            // and one copy through the pipe itself; ten thousand of them
            // made a query take a hundred and twenty-six milliseconds
            // where the search itself needs thirty. Nobody looks past a
            // thousand file results for one query — and if the list ever
            // runs out, the answer to that is a better query, not a
            // longer pipe.
            if let Some(hits) = self.service.query(term, limit.min(1_000) as u32) {
                for hit in hits {
                    if self.live.is_deleted(Path::new(&hit.path)) {
                        continue;
                    }
                    if seen.insert(launcher_index::files::path_key(Path::new(&hit.path))) {
                        results.push(service_result(hit));
                    }
                }
            } else {
            let full = self.full.read();
            if let Some(index) = full.as_ref() {
                // A filter on the size or on a timestamp has to look at the file
                // itself. The provider opens the file and caches the answer.
                let budget = self.metadata_budget.clone();
                let provider = |entry: u32| -> Option<FileMeta> {
                    // The budget runs out after a while; a query that matches
                    // every file would otherwise open half a million of them.
                    let mut left = budget.load(Ordering::Relaxed);
                    loop {
                        if left == 0 {
                            return None;
                        }
                        match budget.compare_exchange_weak(
                            left,
                            left - 1,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        ) {
                            Ok(_) => break,
                            Err(current) => left = current,
                        }
                    }
                    let path = index.path(entry);
                    let meta = std::fs::metadata(&path).ok()?;
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let created = meta
                        .created()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let accessed = meta
                        .accessed()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    Some(FileMeta {
                        size: meta.len(),
                        created,
                        modified,
                        accessed,
                        attributes: 0,
                    })
                };
                let hits = {
                    let _timer = launcher_core::profile::phase("query.file.search");
                    index.search_with_cancel(
                        term,
                        limit,
                        Some(&provider),
                        *self.sort.read(),
                        Some((&self.cancel, query.generation)),
                    )
                };
                for hit in hits {
                    // The table is a picture of the disk from the moment it
                    // was taken. A file that was deleted after that is still
                    // in it, and the watcher knows that it is gone.
                    if self.live.is_deleted(Path::new(&hit.path)) {
                        continue;
                    }
                    if seen.insert(launcher_index::files::path_key(Path::new(&hit.path))) {
                        results.push(file_result(hit));
                    }
                }
            }
            }
        }

        // The live index knows the folders of the user, and it is never out
        // of date. Its results come second, so that the full-disk index
        // keeps its place at the top.
        for result in self.query_live(term, limit, query.generation) {
            if seen.insert(launcher_index::files::path_key(Path::new(&result.copy_text))) {
                results.push(result);
            }
        }

        // An unstable sort: the rows with the same score are put in any order
        // here, and the engine sorts them again with a stable order.
        results.sort_unstable_by_key(|result| std::cmp::Reverse(result.score));
        results.truncate(limit);
        Ok(results)
    }
}

impl FilePlugin {
    /// The live index: the folders of the user, as they are right now.
    ///
    /// The rank of a result is the same as the one of the full-disk index, so
    /// that the two can be mixed: the score of the fuzzy match plus the bonus
    /// for a short name.
    fn query_live(&self, term: &str, limit: usize, generation: u64) -> Vec<LauncherResult> {
        // The live index is the same kind of tree as the full-disk one, so
        // the same search answers the query — and the same rules hold about
        // what a path means.
        let hits = self.live.search(
            term,
            limit,
            *self.sort.read(),
            Some((&self.cancel, generation)),
        );
        let mut results: Vec<LauncherResult> = Vec::with_capacity(hits.len());
        for hit in hits {
            // The file may be gone already: the watcher knows about the
            // files that were deleted.
            if self.live.is_deleted(Path::new(&hit.path)) {
                continue;
            }
            results.push(file_result(hit));
        }
        results
    }
}

/// Whether a change in the file system could show up in the results.
///
/// The machine writes files all the time — the browser, the editor, the
/// build. Redrawing the list for each of those would make the window
/// flicker, so a change is only worth waking the window up for when the name
/// of the file could match what the user is looking for.
fn may_match(matcher: &StringMatcher, last_term: &RwLock<String>, path: &Path) -> bool {
    // The launcher's own files do not count.
    //
    // The index cache, the log and the history live in the folders of the
    // user, and the launcher watches those folders. So the launcher would
    // see its own writes — and a query would follow every log line it
    // writes, which is a loop that never ends.
    let own = crate::paths::data_directory();
    if path.starts_with(&own) {
        return false;
    }
    let term = last_term.read();
    if term.is_empty() {
        return false;
    }
    let prepared = matcher.prepare(&term);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    // Only a **proper** match counts.
    //
    // The question here is not "would this file show up in the results" —
    // it is "is it worth asking the service again". The search may be set
    // to "宽松" or "显示全部", and then almost every path matches: "clash"
    // is a subsequence of half the paths on the machine.
    //
    // A machine that writes thousands of files a second — a training job,
    // a build — would then have the window ask the service again and
    // again, and the service would answer over and over while the disk is
    // busy with the very files that are being written.
    let worth_asking = |hit: &launcher_core::matcher::MatchResult| {
        hit.score >= launcher_core::matcher::SearchPrecision::Regular.min_score()
    };
    if let Some(hit) = matcher.match_prepared_lower(&prepared, &name) {
        if worth_asking(&hit) {
            return true;
        }
    }
    // The file may be in a folder that matches, or the path itself may.
    if let Some(hit) = matcher.match_prepared_lower(&prepared, &path.to_string_lossy()) {
        if worth_asking(&hit) {
            return true;
        }
    }
    false
}

/// One file, as a row of the result list.
///
/// The icon is the one of the file. A file that has none of its own gets the
/// icon of the program that opens it — a text file should show the text
/// editor, not a letter.
fn file_result(hit: launcher_index::fsindex::FsHit) -> LauncherResult {
    let path = hit.path;
    LauncherResult {
        title: hit.name,
        subtitle: path.clone(),
        score: hit.score,
        plugin_id: "file".into(),
        copy_text: path.clone(),
        icon: ResultIcon::Path(PathBuf::from(path)),
        size: hit.size,
        modified: hit.modified,
        ..Default::default()
    }
}

/// One file that the service answered with, as a row of the result list.
fn service_result(hit: launcher_core::service::FileHit) -> LauncherResult {
    let path = hit.path;
    LauncherResult {
        title: hit.name,
        subtitle: path.clone(),
        score: hit.score,
        plugin_id: "file".into(),
        copy_text: path.clone(),
        icon: ResultIcon::Path(PathBuf::from(path)),
        size: hit.size,
        modified: hit.modified,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fuzzy_match_does_not_worth_asking_again() {
        // The search may be set to "宽松" or "显示全部", and then almost
        // every path on the machine matches: "clash" is a subsequence of
        // the path below. A machine that writes thousands of files a second
        // would then have the window ask the service again and again.
        let matcher = StringMatcher::with_precision(
            launcher_core::matcher::SearchPrecision::None,
        );
        let term = RwLock::new("clash".to_string());
        let training = Path::new(
            "C:\\Users\\someone\\Documents\\projects\\TaCauchy\\outputs\\learning\\step_v2384\\experimental_step_0.npz",
        );
        assert!(
            !may_match(&matcher, &term, training),
            "a fuzzy match must not make the window ask again"
        );
    }

    #[test]
    fn a_real_match_is_worth_asking_again() {
        let matcher = StringMatcher::with_precision(
            launcher_core::matcher::SearchPrecision::None,
        );
        let term = RwLock::new("clash".to_string());
        assert!(may_match(
            &matcher,
            &term,
            Path::new("C:\\Users\\someone\\Documents\\clash\\notes.txt")
        ));
    }

    #[test]
    fn the_launchers_own_files_do_not_count() {
        let matcher = StringMatcher::with_precision(
            launcher_core::matcher::SearchPrecision::None,
        );
        let term = RwLock::new("clash".to_string());
        let own = crate::paths::data_directory().join("Logs").join("clash.log");
        assert!(!may_match(&matcher, &term, &own));
    }

    #[test]
    fn empty_index_returns_nothing() {
        let plugin = FilePlugin::new(Arc::new(FileIndex::new()));
        // The service must not answer here: this test is about the work of
        // the launcher alone, and a machine that has the service would
        // answer it with the index of the whole machine.
        let plugin = plugin.with_service(crate::service_client::ServiceClient::disabled());
        assert!(plugin.query(&Query::plain("anything")).unwrap().is_empty());
    }

    #[test]
    fn matches_indexed_files() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileplugin-test");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("needle.txt"), b"x").unwrap();

        let index = FileIndex::new();
        index.refresh(SearchOptions {
            roots: vec![dir.clone()],
            shallow_roots: Vec::new(),
            excluded_directories: Vec::new(),
            excluded_extensions: Vec::new(),
            max_depth: 3,
            max_results: 100,
            timeout: std::time::Duration::from_secs(5),
        });
        // The refresh runs on a worker thread; wait for it.
        for _ in 0..100 {
            if !index.is_refreshing() && !index.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let plugin = FilePlugin::new(Arc::new(index));
        let results = plugin.query(&Query::plain("needle")).unwrap();
        assert!(!results.is_empty(), "expected the indexed file to match");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_was_created_now_is_found() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileplugin-live");
        let _ = std::fs::create_dir_all(&dir);
        let index = Arc::new(FileIndex::new());
        let plugin = FilePlugin::new(index.clone());

        // The file appears after the plugin was made — this is what the
        // watcher does.
        let file = dir.join("appeared.txt");
        std::fs::write(&file, b"x").unwrap();
        index.apply(file, launcher_index::watch::ChangeAction::Added);
        // The changes are put into the index by rounds; the test wants the
        // next round to come right away.
        index.flush();

        let results = plugin.query(&Query::plain("appeared")).unwrap();
        assert!(!results.is_empty(), "expected the new file to match");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_was_deleted_now_is_gone() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileplugin-gone");
        let _ = std::fs::create_dir_all(&dir);
        let index = Arc::new(FileIndex::new());
        let plugin = FilePlugin::new(index.clone());
        let file = dir.join("vanished.txt");
        std::fs::write(&file, b"x").unwrap();
        index.apply(file.clone(), launcher_index::watch::ChangeAction::Added);
        index.flush();
        assert!(!plugin.query(&Query::plain("vanished")).unwrap().is_empty());

        std::fs::remove_file(&file).unwrap();
        index.apply(file, launcher_index::watch::ChangeAction::Removed);
        index.flush();
        assert!(
            plugin.query(&Query::plain("vanished")).unwrap().is_empty(),
            "the deleted file is still in the results"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
