//! File and folder searching.
//!
//! The index of the user's own folders is kept up to date by watching them
//! (see [`crate::watch`]): a file that is created, renamed or deleted is in
//! the index within milliseconds, without reading the disk again and without
//! administrative privileges.
//!
//! The index is a **tree**, not a table of paths. The names of all the files
//! live in one arena, and an entry only knows the number of its folder, so
//! the path of a file is put together when it is needed and not before. That
//! is what makes it possible to hold *every* file of the home folder — around
//! nine hundred thousand of them — inside a hundred megabytes, and to look at
//! all of them in a few milliseconds.
//!
//! The names that are gone are remembered in a small list of tombstones, so
//! that the stale rows of the full-disk index can be left out of the results.
//!
//! Flow Launcher's `Explorer` plugin has three engines:
//!
//! * `WindowsIndex` — queries the Windows Search Service over OLE DB,
//! * `DirectoryInfoSearch` — walks directories itself,
//! * `Everything` — talks to `Everything.exe` over its IPC window.
//!
//! Only the second one is re-implemented here, because it needs no external
//! service and no COM. The walk is done on a background thread and the results
//! are ranked with the same fuzzy matcher used for programs.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use launcher_core::cancel::Cancel;
use launcher_core::settings::FileSort;
use parking_lot::RwLock;

use crate::fsindex::{FsHit, FsIndex, NO_PARENT};
use crate::watch::{ChangeAction, Watcher};

/// Which directories to search, and what to skip.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// The roots to walk.
    pub roots: Vec<PathBuf>,
    /// The roots that are only looked at one level deep.
    ///
    /// The home folder is one of them: the folders below it are walked
    /// through on their own, so the loose files next to them are the only
    /// thing that is missing from there.
    pub shallow_roots: Vec<PathBuf>,
    /// Directory names that are never descended into.
    pub excluded_directories: Vec<String>,
    /// File extensions that are never returned.
    pub excluded_extensions: Vec<String>,
    /// How deep to descend.
    pub max_depth: usize,
    /// How many entries the index holds.
    ///
    /// The index of the user's own folders holds **every** file in them, so
    /// this is a good deal more than the number of rows the list shows. The
    /// number is only here so that a folder that holds millions of files
    /// cannot fill the memory.
    pub max_results: usize,
    /// How long the walk may take before it gives up.
    pub timeout: Duration,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            roots: default_roots(),
            shallow_roots: default_shallow_roots(),
            excluded_directories: default_excluded_directories(),
            excluded_extensions: Vec::new(),
            // Nine hundred thousand files are about a hundred megabytes and a
            // minute of walking. The depth is deep enough for the deepest
            // folder tree that anybody has.
            max_depth: 64,
            max_results: 4_000_000,
            timeout: Duration::from_secs(600),
        }
    }
}

/// The directories that are searched by default.
///
/// They are where the files of a working day are, and they are walked
/// through before anything else, so that a walk that runs out of time still
/// has the important ones.
pub fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for dir in ["Desktop", "Documents", "Downloads", "Pictures", "Music", "Videos"] {
            roots.push(home.join(dir));
        }
    }
    roots
}

/// The directories that are only looked at one level deep.
pub fn default_shallow_roots() -> Vec<PathBuf> {
    dirs::home_dir().into_iter().collect()
}

/// Directory names that are skipped: build output, caches, and the places
/// where a launcher would find thousands of irrelevant files.
pub fn default_excluded_directories() -> Vec<String> {
    [
        "$RECYCLE.BIN",
        "System Volume Information",
        "node_modules",
        "target",
        "bin",
        "obj",
        ".git",
        ".svn",
        ".hg",
        "AppData",
        "Windows",
        "Program Files",
        "Program Files (x86)",
        "ProgramData",
        ".cache",
        ".cargo",
        ".rustup",
        ".nuget",
        ".vscode",
        ".idea",
        // Python 和 JavaScript 的「装好的依赖」跟 `target`、`node_modules`
        // 是一回事:几十万个文件,没有一个是人放进去的。
        ".venv",
        "venv",
        "__pycache__",
        ".pytest_cache",
        ".mypy_cache",
        ".ruff_cache",
        ".tox",
        ".nox",
        "site-packages",
        "conda-meta",
        ".ipynb_checkpoints",
        ".next",
        ".nuxt",
        ".gradle",
        ".terraform",
        "vendor",
        "Pods",
        ".turbo",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// How many deleted paths are remembered.
///
/// The list is only used to leave the stale rows of the full-disk index out
/// of the results, so it does not have to be complete — only recent.
const TOMBSTONE_LIMIT: usize = 20_000;

/// How many changes are remembered while a walk is running.
const PENDING_LIMIT: usize = 200_000;

/// The paths that are gone.
///
/// The paths are kept as **hashes**, not as paths: the question «is this path
/// gone?» is asked for every row of every result of every query, and a hash
/// of the path answers it without touching the string at all.
#[derive(Debug, Default)]
struct Tombstones {
    paths: HashSet<u64>,
    order: VecDeque<(u64, PathBuf)>,
}

impl Tombstones {
    /// Remember that a path is gone.
    fn insert(&mut self, path: PathBuf) {
        let key = path_key(&path);
        if self.paths.insert(key) {
            self.order.push_back((key, path));
            while self.order.len() > TOMBSTONE_LIMIT {
                if let Some((old, _)) = self.order.pop_front() {
                    self.paths.remove(&old);
                }
            }
        }
    }

    /// Whether a path is known to be gone.
    fn contains(&self, path: &Path) -> bool {
        self.paths.contains(&path_key(path))
    }
}

/// The hash of a path, in a way that does not allocate.
///
/// Windows does not care about the case of a path, so the letters are folded
/// to lower case first — otherwise the same file would be two paths.
pub fn path_key(path: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= (*byte).to_ascii_lowercase() as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// How far the walk over the user's folders has come.
#[derive(Debug, Default)]
pub struct ScanProgress {
    /// How many entries have been found.
    pub found: AtomicU64,
    /// How many folders have been walked into.
    pub folders: AtomicU64,
    /// Whether the walk is over.
    pub done: AtomicBool,
}

/// The entries of the index, with the tables that go with them.
///
/// The entries themselves are in the compact tree of [`FsIndex`]; the two
/// tables here are what the watcher needs to find a file again without
/// walking the whole tree.
#[derive(Debug, Default)]
pub struct Tree {
    /// The entries: the names in one arena, the tree beside them.
    index: FsIndex,
    /// The folders of the tree, by path.
    ///
    /// Only the folders are here. A file is found through its folder, and
    /// there are many times fewer folders than files.
    folders: HashMap<PathBuf, u32>,
    /// The children of every folder.
    children: HashMap<u32, Vec<u32>>,
}

impl Tree {
    /// How many entries the tree holds.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// How many bytes the names of the entries take.
    pub fn name_bytes(&self) -> usize {
        self.index.name_bytes()
    }

    /// How many bytes the entries take, apart from their names.
    pub fn entry_bytes(&self) -> usize {
        self.index.entry_bytes()
    }

    /// The files that match a query.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        sort: FileSort,
        cancel: Option<(&Cancel, u64)>,
    ) -> Vec<FsHit> {
        self.index.search_with_cancel(query, limit, None, sort, cancel)
    }

    /// The entry of a child of a folder.
    fn child_index(&self, parent: u32, name: &str) -> Option<u32> {
        let children = self.children.get(&parent)?;
        children
            .iter()
            .copied()
            .find(|&child| self.index.name(child) == name)
    }

    /// Add a file or a folder to a folder of the tree.
    fn add_child(
        &mut self,
        parent: u32,
        name: &str,
        is_dir: bool,
        size: u64,
        modified: u32,
        known: bool,
    ) -> u32 {
        let index = self.index.add_live(parent, name, is_dir, size, modified, known);
        self.children.entry(parent).or_default().push(index);
        index
    }

    /// Mark an entry and everything below it as gone.
    fn exclude_subtree(&mut self, index: u32) {
        let mut stack = vec![index];
        while let Some(current) = stack.pop() {
            self.index.mark_excluded(current);
            if let Some(children) = self.children.get(&current) {
                stack.extend(children.iter().copied());
            }
        }
    }

    /// The folder that holds a path, when it is known.
    fn folder_index(&self, path: &Path) -> Option<u32> {
        let parent = path.parent()?;
        self.folders.get(parent).copied()
    }

    /// How many entries lie under a folder.
    pub fn count_under(&self, path: &Path) -> usize {
        let Some(index) = self.folders.get(path).copied() else {
            return 0;
        };
        let mut count = 0usize;
        let mut stack = vec![index];
        while let Some(current) = stack.pop() {
            count += 1;
            if let Some(children) = self.children.get(&current) {
                stack.extend(children.iter().copied());
            }
        }
        count
    }

    /// Whether the tree knows a path.
    pub fn has_path(&self, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        let Some(index) = self.folders.get(parent).copied() else {
            return false;
        };
        let Some(name) = path.file_name() else {
            return false;
        };
        match self.child_index(index, &name.to_string_lossy()) {
            Some(entry) => !self.index.is_excluded(entry),
            None => false,
        }
    }

    /// The folder that holds a path, made when it is not known yet.
    ///
    /// A file that appears in a folder that the index does not know yet —
    /// a folder that was created a moment ago, say — is put into a chain of
    /// folders that is made on the spot, so that its path is right.
    fn ensure_folder(&mut self, path: &Path) -> Option<u32> {
        let parent = path.parent()?;
        if let Some(index) = self.folders.get(parent) {
            return Some(*index);
        }
        // The nearest folder that is known.
        let mut ancestor = parent.parent();
        let mut base = None;
        while let Some(current) = ancestor {
            if let Some(index) = self.folders.get(current) {
                base = Some((current.to_path_buf(), *index));
                break;
            }
            ancestor = current.parent();
        }
        let (mut at, mut index) = match base {
            Some(found) => found,
            None => {
                // The tree knows nothing yet: the chain starts at the root
                // of the volume, and the volume is made from the path.
                let root = match self.index.volumes().first() {
                    Some(volume) => PathBuf::from(&volume.root),
                    None => {
                        let root = volume_root(path)?;
                        self.index.set_single_volume(&root);
                        PathBuf::from(root)
                    }
                };
                (root, NO_PARENT)
            }
        };
        // The folders between the ancestor and the one that is wanted.
        let mut chain: Vec<&Path> = Vec::new();
        let mut current = parent;
        loop {
            if current == at {
                break;
            }
            chain.push(current);
            current = current.parent()?;
        }
        for folder in chain.into_iter().rev() {
            let name = folder.file_name()?.to_string_lossy().to_string();
            at.push(folder.file_name()?);
            index = self.add_child(index, &name, true, 0, 0, true);
            self.folders.insert(at.clone(), index);
        }
        Some(index)
    }
}

/// The file index, kept up to date by the watcher.
#[derive(Debug, Default)]
pub struct FileIndex {
    /// The tree of the entries.
    tree: Arc<RwLock<Tree>>,
    /// The changes that came in while a walk was running.
    ///
    /// The walk builds a new tree and puts it in the place of the old one at
    /// the end. The changes that came in the meantime are remembered here and
    /// applied to the new tree, so that a file that was created while the
    /// walk was running is not lost.
    pending: Arc<RwLock<Vec<(PathBuf, ChangeAction)>>>,
    /// The changes that are waiting for their turn.
    ///
    /// **A change does not go into the index right away.** It waits here
    /// until the next round, and then all the waiting changes are applied in
    /// one go: one lock, one version, one round of work. A program that
    /// writes a hundred thousand files therefore costs the index **one**
    /// round, not a hundred thousand.
    waiting: Arc<RwLock<Vec<(PathBuf, ChangeAction)>>>,
    /// How often the waiting changes are applied, in seconds.
    update_interval: Arc<AtomicU64>,
    /// Set when the index is dropped.
    stopped: Arc<AtomicBool>,
    /// The paths that are gone.
    tombstones: Arc<RwLock<Tombstones>>,
    /// How many changes have been applied.
    ///
    /// A query that was answered before a change is out of date.
    version: Arc<AtomicU64>,
    /// Set while a walk is running.
    refreshing: Arc<AtomicBool>,
    /// When the index was last built.
    built_at: Arc<RwLock<Option<Instant>>>,
    /// The folders that are left out.
    excluded: Arc<RwLock<Vec<String>>>,
    /// How far the walk has come.
    progress: Arc<ScanProgress>,
}

impl FileIndex {
    /// Create an empty index.
    pub fn new() -> Self {
        let mut index = Self::default();
        index.update_interval = Arc::new(AtomicU64::new(default_update_interval()));
        index
    }

    /// How often the changes are put into the index, in seconds.
    ///
    /// The default is a minute. It is long enough that a program which
    /// writes thousands of files does not keep the index busy, and short
    /// enough that a file is found again soon.
    pub fn set_update_interval(&self, seconds: u64) {
        self.update_interval
            .store(seconds.clamp(1, 3600), Ordering::SeqCst);
    }

    /// The thread that puts the waiting changes into the index.
    ///
    /// It is started together with the watcher, and it stops when the index
    /// is dropped.
    fn start_updater(self: &Arc<Self>) {
        let index = self.clone();
        std::thread::Builder::new()
            .name("file-index-updater".into())
            .spawn(move || {
                while !index.stopped.load(Ordering::SeqCst) {
                    let seconds = index.update_interval.load(Ordering::SeqCst).max(1);
                    // The wait is in small pieces, so that the thread stops
                    // right away when the index is dropped.
                    let mut left = seconds;
                    while left > 0 && !index.stopped.load(Ordering::SeqCst) {
                        let step = left.min(1);
                        std::thread::sleep(Duration::from_secs(step));
                        left -= step;
                    }
                    if index.stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    index.flush_waiting();
                }
            })
            .ok();
    }

    /// How many entries the index holds.
    pub fn len(&self) -> usize {
        self.tree.read().index.len()
    }

    /// True when the index holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether a walk is currently running.
    pub fn is_refreshing(&self) -> bool {
        self.refreshing.load(Ordering::SeqCst)
    }

    /// How many changes have been applied to the index.
    ///
    /// A query that was answered before a change is out of date.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// How far the walk over the user's folders has come.
    pub fn progress(&self) -> &ScanProgress {
        &self.progress
    }

    /// Say that there is nothing to walk through.
    ///
    /// The full-disk index knows the folders of the user as well, and it
    /// keeps itself up to date — so they are not walked through a second
    /// time. The window must not wait for a walk that is not going to
    /// happen.
    pub fn mark_finished(&self) {
        self.progress.done.store(true, Ordering::SeqCst);
        self.refreshing.store(false, Ordering::SeqCst);
    }

    /// Whether a path is known to be gone.
    ///
    /// The full-disk index is a picture of the disk from the moment it was
    /// taken. A file that was deleted after that is still in it, and this is
    /// how it is left out of the results.
    pub fn is_deleted(&self, path: &Path) -> bool {
        self.tombstones.read().contains(path)
    }

    /// Set the folders that are left out.
    ///
    /// The watcher drops the changes in these folders, so that the thousands
    /// of files in a build folder do not cost anything.
    pub fn set_excluded(&self, directories: Vec<String>) {
        *self.excluded.write() = directories;
    }

    /// The files that match a query.
    ///
    /// The lock is held for as long as the caller walks the table, so that a
    /// change cannot slip in between two entries of one query.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        sort: FileSort,
        cancel: Option<(&Cancel, u64)>,
    ) -> Vec<FsHit> {
        let tree = self.tree.read();
        tree.index.search_with_cancel(query, limit, None, sort, cancel)
    }

    /// Whether the index knows a path.
    pub fn contains(&self, path: &Path) -> bool {
        let tree = self.tree.read();
        let Some(parent) = path.parent() else {
            return false;
        };
        let Some(index) = tree.folders.get(parent).copied() else {
            return false;
        };
        let Some(name) = path.file_name() else {
            return false;
        };
        match tree.child_index(index, &name.to_string_lossy()) {
            Some(entry) => !tree.index.is_excluded(entry),
            None => false,
        }
    }

    /// Walk the folders of the user in the background.
    ///
    /// Returns `false` when a walk is already running.
    pub fn refresh(&self, options: SearchOptions) -> bool {
        self.refresh_with(options, || {})
    }

    /// Walk the folders of the user in the background, and say when it is
    /// over.
    ///
    /// The files that were found may change the results — the watcher cannot
    /// report them, because they were there before the launcher started.
    pub fn refresh_with(
        &self,
        options: SearchOptions,
        on_done: impl Fn() + Send + 'static,
    ) -> bool {
        if self.refreshing.swap(true, Ordering::SeqCst) {
            return false;
        }
        *self.excluded.write() = options.excluded_directories.clone();
        let tree = self.tree.clone();
        let pending = self.pending.clone();
        let refreshing = self.refreshing.clone();
        let built_at = self.built_at.clone();
        let progress = self.progress.clone();
        let version = self.version.clone();
        let tombstones = self.tombstones.clone();
        let excluded = self.excluded.clone();
        std::thread::Builder::new()
            .name("file-index".into())
            .spawn(move || {
                let began = Instant::now();
                // An index that is already there stays on show until the new
                // one is ready; an empty one is filled while the walk runs,
                // so that the first results come within a second.
                let mut sink = if tree.read().index.is_empty() {
                    Sink::Direct(&tree)
                } else {
                    Sink::Local(Tree::default())
                };
                walk_into(&mut sink, &options, &progress, &version);
                if let Some(fresh) = sink.into_local() {
                    let mut guard = tree.write();
                    *guard = fresh;
                }
                // The walk is over: the tables are squeezed, so that the
                // memory that the growth of them left over is given back.
                tree.write().index.shrink();
                // The heap is **not** compacted: it does not make the memory
                // any smaller (the private bytes stay where they are), and it
                // stops every thread of the process while it runs.
                log::info!(
                    "the folders of the user were walked through: {} entries in {:?}",
                    tree.read().index.len(),
                    began.elapsed()
                );
                // The changes that came in while the walk was running are
                // applied to the new tree, so that a file that was created in
                // the meantime is not lost.
                loop {
                    let changes: Vec<(PathBuf, ChangeAction)> = pending.write().drain(..).collect();
                    if changes.is_empty() {
                        break;
                    }
                    // The changes are applied **in batches**, and the file
                    // system is asked before the lock is taken: a batch of
                    // ten thousand files must not hold the lock for the whole
                    // time.
                    let excluded_list = excluded.read();
                    let mut graves = tombstones.write();
                    for (path, action) in changes {
                        let meta = match action {
                            ChangeAction::Removed => None,
                            _ => read_meta(&path),
                        };
                        let mut guard = tree.write();
                        apply_change(&mut guard, &path, action, meta, &excluded_list, &mut graves);
                    }
                }
                refreshing.store(false, Ordering::SeqCst);
                // One more round: the changes that came in while the last one
                // was running.
                let changes: Vec<(PathBuf, ChangeAction)> = pending.write().drain(..).collect();
                if !changes.is_empty() {
                    let excluded_list = excluded.read();
                    let mut graves = tombstones.write();
                    for (path, action) in changes {
                        let meta = match action {
                            ChangeAction::Removed => None,
                            _ => read_meta(&path),
                        };
                        let mut guard = tree.write();
                        apply_change(&mut guard, &path, action, meta, &excluded_list, &mut graves);
                    }
                }
                *built_at.write() = Some(Instant::now());
                version.fetch_add(1, Ordering::SeqCst);
                progress.done.store(true, Ordering::SeqCst);
                log::info!("the index of the user's folders is ready");
                on_done();
            })
            .ok();
        true
    }

    /// Apply one change of the file system to the index.
    ///
    /// The change comes from the watcher, so it is the truth about the file
    /// system: the file is there, or it is gone.
    ///
    /// **The change waits for its turn.** It is put on a list, and the whole
    /// list is applied at once when the time comes. Before, every single
    /// change took the lock, changed the tree and made the version newer — so
    /// a program that wrote a hundred thousand files had the index rewritten
    /// a hundred thousand times, and every query that was running had to
    /// start again.
    ///
    /// Nothing is lost by waiting: a file that was created a moment ago is
    /// hardly the file somebody is looking for **right now**. A file that was
    /// made by hand is remembered by the person who made it, and a file that
    /// a program made is looked for minutes and hours later.
    pub fn apply(&self, path: PathBuf, action: ChangeAction) {
        if self.refreshing.load(Ordering::SeqCst) {
            // The walk is running: the change is remembered and applied to
            // the new tree when it is ready.
            let mut pending = self.pending.write();
            if pending.len() < PENDING_LIMIT {
                pending.push((path, action));
            }
            return;
        }
        let mut waiting = self.waiting.write();
        if waiting.len() < PENDING_LIMIT {
            waiting.push((path, action));
        }
    }

    /// Put the waiting changes into the index right now.
    ///
    /// The changes are applied in one go: one lock, one version, one round
    /// of work. The program does this on its own every minute; this is for
    /// the tests, and for anybody who wants the index to be fresh at once.
    pub fn flush(&self) {
        self.flush_waiting();
    }

    /// Apply the changes that have been waiting.
    ///
    /// They are applied **in one go**: one lock, one version, one round of
    /// work.
    ///
    /// **The folders are read once each.** The names and the kinds of the
    /// files come out of the listing, so a whole folder costs one reading —
    /// and the size and the time of the files are not asked for at all:
    /// the files are on the disk, and there is no sense in spending time on
    /// them just to put them into the index. They are picked up later, when
    /// the folder is read anyway.
    fn flush_waiting(&self) {
        let changes: Vec<(PathBuf, ChangeAction)> = {
            let mut waiting = self.waiting.write();
            std::mem::take(&mut *waiting)
        };
        if changes.is_empty() {
            return;
        }
        // The changes of one folder are put together, so that the folder is
        // read once and not once per file.
        let mut folders: std::collections::HashMap<PathBuf, Vec<(PathBuf, ChangeAction)>> =
            std::collections::HashMap::new();
        for (path, action) in changes {
            let parent = match path.parent() {
                Some(parent) => parent.to_path_buf(),
                None => continue,
            };
            folders.entry(parent).or_default().push((path, action));
        }
        let excluded = self.excluded.read();
        let mut graves = self.tombstones.write();
        let mut tree = self.tree.write();
        let mut changed = false;
        for (folder, list) in folders {
            // **One reading for the whole folder.**
            let meta = read_folder(&folder);
            for (path, action) in list {
                let meta = match action {
                    ChangeAction::Removed => None,
                    _ => {
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_default();
                        meta.get(&name).cloned()
                    }
                };
                if apply_change(&mut tree, &path, action, meta, &excluded, &mut graves) {
                    changed = true;
                }
            }
        }
        drop(tree);
        if changed {
            self.version.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Whether a path lies in one of the folders that are left out.
    fn is_left_out(&self, path: &Path) -> bool {
        let excluded = self.excluded.read();
        if excluded.is_empty() {
            return false;
        }
        path.components().any(|component| {
            let name = component.as_os_str().to_string_lossy();
            excluded
                .iter()
                .any(|excluded| excluded.len() == name.len() && excluded.eq_ignore_ascii_case(&name))
        })
    }

    /// Watch a folder and keep the index up to date with what happens in it.
    ///
    /// The watcher needs no privileges, so this works for a launcher that runs
    /// as the user. When more changes happen than fit into one buffer, the
    /// folder is walked again, so that nothing is lost.
    pub fn watch(self: &Arc<Self>, root: PathBuf) -> Option<Watcher> {
        self.watch_with(root, |_, _| {})
    }

    /// Watch a folder, and tell the caller about every change in it.
    ///
    /// The change is put into the index first; the callback is told about it
    /// afterwards, so that the caller can decide whether it is worth waking
    /// anything up for.
    pub fn watch_with(
        self: &Arc<Self>,
        root: PathBuf,
        on_change: impl Fn(PathBuf, ChangeAction) + Send + 'static,
    ) -> Option<Watcher> {
        let index = self.clone();
        let excluded = self.excluded.clone();
        let watched = root.clone();
        let watcher = Watcher::spawn(
            &root,
            move |path, action| {
                // The folders that are left out are dropped here, so that the
                // thousands of files in a build folder do not cost anything.
                if !index.is_left_out(&path) || action == ChangeAction::Removed {
                    index.apply(path.clone(), action);
                    on_change(path, action);
                }
            },
            {
                let index = self.clone();
                let excluded = excluded.clone();
                let watched = watched.clone();
                move || {
                    log::info!(
                        "the watcher lost changes in {} — walking it again",
                        watched.display()
                    );
                    let options = SearchOptions {
                        roots: vec![watched.clone()],
                        excluded_directories: excluded.read().clone(),
                        ..SearchOptions::default()
                    };
                    index.refresh(options);
                }
            },
        )?;
        log::info!("{} is watched for changes", root.display());
        // **The thread that puts the waiting changes into the index.**
        // It is started here, because here is where the changes come from.
        self.start_updater();
        Some(watcher)
    }
}

/// The index is dropped when the program goes: the thread that applies the
/// changes stops with it.
impl Drop for FileIndex {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

/// Apply one change of the file system to the tree.
///
/// Whether the tree was changed is told at the end: a change that does not
/// touch the tree — a file in a folder that is left out — is not worth a new
/// version of the index.
fn apply_change(
    tree: &mut Tree,
    path: &Path,
    action: ChangeAction,
    meta: Option<Meta>,
    excluded: &[String],
    tombstones: &mut Tombstones,
) -> bool {
    match action {
        ChangeAction::Removed => {
            let Some(parent) = tree.folder_index(path) else {
                return false;
            };
            let Some(name) = path.file_name() else {
                return false;
            };
            let name = name.to_string_lossy();
            let Some(index) = tree.child_index(parent, &name) else {
                return false;
            };
            tree.exclude_subtree(index);
            tree.folders.remove(path);
            tombstones.insert(path.to_path_buf());
            true
        }
        ChangeAction::Added | ChangeAction::Modified => {
            if !excluded.is_empty()
                && path.components().any(|component| {
                    let name = component.as_os_str().to_string_lossy();
                    excluded
                        .iter()
                        .any(|excluded| excluded.len() == name.len() && excluded.eq_ignore_ascii_case(&name))
                })
            {
                return false;
            }
            let Some(name) = path.file_name() else {
                return false;
            };
            let name = name.to_string_lossy().to_string();
            let Some(parent) = tree.ensure_folder(path) else {
                return false;
            };
            match tree.child_index(parent, &name) {
                Some(index) => {
                    // The file is there already: only its size and its time
                    // can have changed.
                    if let Some(meta) = meta {
                        tree.index.set_meta_live(index, meta.size, meta.modified, meta.known);
                    }
                    true
                }
                None => {
                    // The file is new.
                    let Some(meta) = meta else {
                        // The file is gone already, or it cannot be read.
                        return false;
                    };
                    let index = tree.add_child(parent, &name, meta.is_dir, meta.size, meta.modified, meta.known);
                    if meta.is_dir {
                        tree.folders.insert(path.to_path_buf(), index);
                    }
                    true
                }
            }
        }
    }
}

/// How often the changes are put into the index, in seconds.
///
/// The environment variable `IDEAS_UPDATE_INTERVAL` says it. It is there
/// for the tests, and for anybody who wants the index to be fresher (or
/// quieter) than the default.
fn default_update_interval() -> u64 {
    std::env::var("IDEAS_UPDATE_INTERVAL")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(1, 3600)
}

/// What the file system says about a file.
#[derive(Clone)]
struct Meta {
    size: u64,
    modified: u32,
    is_dir: bool,
    /// Whether the size and the time are known.
    ///
    /// A file that was put into the index **without asking the disk about
    /// it** has no size and no time — and that is not the same as being
    /// empty. The mark is what tells the two apart, so that the window can
    /// say "unknown" instead of "0 bytes".
    known: bool,
}

/// Read one file from the file system.
fn read_meta(path: &Path) -> Option<Meta> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);
    Some(Meta {
        size: metadata.len(),
        modified,
        is_dir: metadata.is_dir(),
        known: true,
    })
}

/// Read a whole folder in one go.
///
/// **The names and the kinds of the files come out of the listing itself** —
/// the system hands them over while it walks the folder — so a whole folder
/// costs one reading. The size and the time of the files are **not** asked
/// for: that would be one request per file, and the files are on the disk,
/// and there is no sense in spending time on them just to put them into the
/// index.
///
/// The size and the time of a file are picked up later, when the folder is
/// read anyway — by the walk that builds the index, or by the next time the
/// folder is looked at.
fn read_folder(folder: &Path) -> std::collections::HashMap<String, Meta> {
    let mut out = std::collections::HashMap::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        out.insert(
            name,
            Meta {
                size: 0,
                modified: 0,
                is_dir: kind.is_dir(),
                known: false,
            },
        );
    }
    out
}

/// The root of the volume a path lies on, for example `C:\`.
fn volume_root(path: &Path) -> Option<String> {
    let mut components = path.components();
    let first = components.next()?;
    let prefix = first.as_os_str().to_string_lossy().to_string();
    if !prefix.ends_with(':') {
        return None;
    }
    Some(format!("{prefix}\\"))
}

/// The folders that lead to a root, from the root of the volume down.
///
/// The folders that are already in the tree are used as they are, so that
/// the roots that share a beginning — and they all share the home folder —
/// do not get two copies of it.
fn chain_to(tree: &mut Tree, volume: &str, root: &Path) -> Option<u32> {
    let relative = root.strip_prefix(volume).ok()?;
    let mut index = NO_PARENT;
    let mut path = PathBuf::from(volume);
    for part in relative.components() {
        let name = part.as_os_str().to_string_lossy().to_string();
        path.push(part);
        let child = match tree.child_index(index, &name) {
            Some(child) => child,
            None => {
                let child = tree.add_child(index, &name, true, 0, 0, true);
                tree.folders.insert(path.clone(), child);
                child
            }
        };
        index = child;
    }
    Some(index)
}

/// Where the entries of a walk go.
///
/// A walk that starts with an empty index puts its entries straight into the
/// tree that is on show: the user sees the first results within a second
/// instead of waiting for the whole walk. A walk that replaces an index that
/// is already there collects the entries in a tree of their own and swaps
/// them in at the end, so that the old results stay on show until the new
/// ones are ready.
pub enum Sink<'a> {
    /// Straight into the tree that is on show.
    Direct(&'a RwLock<Tree>),
    /// Into a tree of their own.
    Local(Tree),
}

impl Sink<'_> {
    /// Do something with the tree the entries go into.
    fn with_tree<R>(&mut self, action: impl FnOnce(&mut Tree) -> R) -> R {
        match self {
            Sink::Direct(tree) => {
                let mut guard = tree.write();
                action(&mut guard)
            }
            Sink::Local(tree) => action(tree),
        }
    }

    /// The tree of a walk that collected its entries on its own.
    fn into_local(self) -> Option<Tree> {
        match self {
            Sink::Local(tree) => Some(tree),
            Sink::Direct(_) => None,
        }
    }
}

/// Walk the configured roots and put what is found into the tree.
///
/// Every root has a queue of its own, and the queues take turns: one folder of
/// the desktop, one of the documents, one of the downloads, and so on. Without
/// that, one big folder (a directory of source code, say) would use up the
/// whole budget, and the folders after it would never be looked at.
///
/// The queue of a root is a breadth-first one: the files that lie right next
/// to the root are found before the ones deep down.
pub fn walk_into(
    sink: &mut Sink,
    options: &SearchOptions,
    progress: &ScanProgress,
    version: &AtomicU64,
) -> usize {
    let started = Instant::now();
    let excluded: HashSet<String> = options
        .excluded_directories
        .iter()
        .map(|s| s.to_lowercase())
        .collect();
    let excluded_ext: HashSet<String> = options
        .excluded_extensions
        .iter()
        .map(|s| s.trim_start_matches('.').to_lowercase())
        .collect();

    // Everything lives on the volume of the first root. The roots of the
    // user are all in the home folder, so they are all on one drive.
    let Some(first) = options.roots.first().or(options.shallow_roots.first()) else {
        return 0;
    };
    let Some(volume) = volume_root(first) else {
        return 0;
    };
    sink.with_tree(|tree| tree.index.set_single_volume(&volume));

    // The queues of the roots.
    let mut queues: Vec<Vec<(PathBuf, u32, usize, bool)>> = Vec::new();
    for root in &options.roots {
        let index = sink.with_tree(|tree| chain_to(tree, &volume, root));
        if let Some(index) = index {
            queues.push(vec![(root.clone(), index, 0, true)]);
        }
    }
    for root in &options.shallow_roots {
        let index = sink.with_tree(|tree| chain_to(tree, &volume, root));
        if let Some(index) = index {
            queues.push(vec![(root.clone(), index, 0, false)]);
        }
    }

    // How many entries the walk itself found. The folders that lead to the
    // roots are not counted: they are the way to the root, not a find.
    let mut found = 0usize;
    let mut told = Instant::now();
    // The names of the folder that is being read go into this one buffer,
    // which is used over and over: a million names must not become a
    // million allocations.
    let mut names = String::new();
    loop {
        if started.elapsed() > options.timeout {
            break;
        }
        let mut worked = false;
        for queue in queues.iter_mut() {
            if queue.is_empty() {
                continue;
            }
            worked = true;
            // One folder of this root.
            let current = std::mem::take(queue);
            for (dir, index, depth, descend) in current {
                if started.elapsed() > options.timeout || found >= options.max_results {
                    break;
                }
                // The folder is read without holding the lock: the disk is
                // slow, and a query must not wait for it.
                let mut batch: Vec<(u32, u32, bool, u64, u32, Option<PathBuf>)> = Vec::new();
                let room = options.max_results.saturating_sub(found);
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                names.clear();
                for entry in entries.flatten() {
                    if batch.len() >= room {
                        break;
                    }
                    let name = entry.file_name();
                    let start = names.len();
                    names.push_str(&name.to_string_lossy());
                    let length = names.len() - start;
                    let Ok(metadata) = entry.metadata() else {
                        continue;
                    };
                    let modified = metadata
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as u32)
                        .unwrap_or(0);
                    let is_dir = metadata.is_dir();
                    // The path of a file is never needed: it is put together
                    // from the tree when it is asked for. Only a folder has
                    // to remember where it lies.
                    let path = if is_dir { Some(entry.path()) } else { None };
                    batch.push((
                        start as u32,
                        length as u32,
                        is_dir,
                        metadata.len(),
                        modified,
                        path,
                    ));
                }
                // The entries go into the tree, and the folders among them
                // are queued for the next round.
                let folders = sink.with_tree(|tree| {
                    let mut folders = Vec::new();
                    for (start, length, is_dir, size, modified, path) in batch {
                        let name = &names[start as usize..(start + length) as usize];
                        if is_dir {
                            if is_left_out(&excluded, name) {
                                continue;
                            }
                            // The folder may already be in the tree: the
                            // chain that leads to a root makes the folders
                            // on the way there, and the home folder lists
                            // them as well.
                            let child = match tree.child_index(index, name) {
                                Some(existing) => {
                                    tree.index.set_meta_live(existing, 0, modified, true);
                                    existing
                                }
                                None => {
                                    let child = tree.add_child(index, name, true, 0, modified, true);
                                    if let Some(place) = path.as_ref() {
                                        tree.folders.insert(place.clone(), child);
                                    }
                                    child
                                }
                            };
                            folders.push((child, path));
                        } else {
                            if is_left_out_extension(&excluded_ext, name) {
                                continue;
                            }
                            tree.add_child(index, name, false, size, modified, true);
                        }
                        found += 1;
                    }
                    folders
                });
                for (child, path) in folders {
                    if descend && depth < options.max_depth {
                        if let Some(path) = path {
                            queue.push((path, child, depth + 1, true));
                        }
                    }
                }
                progress.folders.fetch_add(1, Ordering::Relaxed);
            }
        }
        if !worked {
            break;
        }
        progress.found.store(found as u64, Ordering::Relaxed);
        // The results on the screen have to catch up with the index, so the
        // version is told every now and then.
        if told.elapsed() >= Duration::from_millis(800) {
            told = Instant::now();
            version.fetch_add(1, Ordering::Relaxed);
        }
    }
    progress.found.store(found as u64, Ordering::Relaxed);
    found
}

/// Walk the roots into a tree of its own.
pub fn walk(tree: &mut Tree, options: &SearchOptions, progress: &ScanProgress) -> usize {
    let version = AtomicU64::new(0);
    let mut sink = Sink::Local(std::mem::take(tree));
    let found = walk_into(&mut sink, options, progress, &version);
    if let Some(fresh) = sink.into_local() {
        *tree = fresh;
    }
    found
}

/// Whether a folder name is one of the folders that are left out.
fn is_left_out(excluded: &HashSet<String>, name: &str) -> bool {
    if excluded.is_empty() {
        return false;
    }
    if excluded.contains(name) {
        return true;
    }
    // The names in the list are in lower case. A name with a capital letter
    // in it is put in lower case first — and only then.
    name.bytes().any(|byte| byte.is_ascii_uppercase()) && excluded.contains(&name.to_lowercase())
}

/// Whether the extension of a name is one of the extensions that are left out.
fn is_left_out_extension(excluded: &HashSet<String>, name: &str) -> bool {
    if excluded.is_empty() {
        return false;
    }
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    if excluded.contains(extension) {
        return true;
    }
    extension.bytes().any(|byte| byte.is_ascii_uppercase())
        && excluded.contains(&extension.to_lowercase())
}


/// Format a byte count for display.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(roots: Vec<PathBuf>) -> SearchOptions {
        SearchOptions {
            roots,
            shallow_roots: Vec::new(),
            excluded_directories: Vec::new(),
            excluded_extensions: Vec::new(),
            max_depth: 4,
            max_results: 100,
            timeout: Duration::from_secs(5),
        }
    }

    /// The names of the entries of a tree.
    fn names(tree: &Tree) -> Vec<String> {
        (0..tree.index.len() as u32)
            .map(|index| tree.index.name(index).to_owned())
            .collect()
    }

    #[test]
    fn human_size_is_readable() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn walk_finds_files_and_folders() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("sub"));
        std::fs::write(dir.join("alpha.txt"), b"hello").unwrap();
        std::fs::write(dir.join("sub/beta.txt"), b"world").unwrap();

        let mut tree = Tree::default();
        walk(&mut tree, &options(vec![dir.clone()]), &ScanProgress::default());
        let found = names(&tree);
        assert!(found.contains(&"alpha.txt".to_owned()), "{found:?}");
        assert!(found.contains(&"beta.txt".to_owned()), "{found:?}");
        assert!(found.contains(&"sub".to_owned()), "{found:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_paths_are_right() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-path");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("sub"));
        std::fs::write(dir.join("sub/beta.txt"), b"world").unwrap();

        let mut tree = Tree::default();
        walk(&mut tree, &options(vec![dir.clone()]), &ScanProgress::default());
        let paths: Vec<String> = (0..tree.index.len() as u32)
            .map(|index| tree.index.path(index))
            .collect();
        assert!(
            paths.iter().any(|path| path.ends_with("sub\\beta.txt")),
            "{paths:?}"
        );
        assert!(
            paths.iter().any(|path| path.ends_with("sub")),
            "{paths:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_respects_excluded_directories_and_extensions() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-excl");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("node_modules"));
        std::fs::write(dir.join("node_modules/dep.js"), b"x").unwrap();
        std::fs::write(dir.join("keep.js"), b"x").unwrap();
        std::fs::write(dir.join("skip.bin"), b"x").unwrap();

        let mut options = options(vec![dir.clone()]);
        options.excluded_directories = vec!["node_modules".into()];
        options.excluded_extensions = vec!["bin".into()];
        let mut tree = Tree::default();
        walk(&mut tree, &options, &ScanProgress::default());
        let found = names(&tree);
        assert!(found.contains(&"keep.js".to_owned()), "{found:?}");
        assert!(!found.contains(&"dep.js".to_owned()), "{found:?}");
        assert!(!found.contains(&"skip.bin".to_owned()), "{found:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_respects_the_result_limit() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-limit");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        for i in 0..20 {
            std::fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
        }
        let mut options = options(vec![dir.clone()]);
        options.max_depth = 2;
        options.max_results = 5;
        let mut tree = Tree::default();
        let found = walk(&mut tree, &options, &ScanProgress::default());
        assert_eq!(found, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_roots_are_not_empty() {
        assert!(!default_roots().is_empty());
    }

    #[test]
    fn a_new_file_is_in_the_index_at_once() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-live");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let index = FileIndex::new();
        let file = dir.join("brand-new.txt");
        std::fs::write(&file, b"x").unwrap();

        index.apply(file.clone(), ChangeAction::Added);
        index.flush();
        assert!(index.contains(&file));
        assert!(!index.is_deleted(&file));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A change waits for its round.**
    ///
    /// The index is not rewritten for every single file that appears: the
    /// changes wait, and they are applied together. A program that writes a
    /// hundred thousand files therefore costs the index **one** round, and
    /// not a hundred thousand.
    #[test]
    fn a_change_waits_for_its_round() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-round");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let index = FileIndex::new();
        let file = dir.join("waiting.txt");
        std::fs::write(&file, b"x").unwrap();

        index.apply(file.clone(), ChangeAction::Added);
        assert!(
            !index.contains(&file),
            "the change went into the index right away"
        );
        index.flush();
        assert!(index.contains(&file), "the change did not reach the index");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deleted_file_is_remembered() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-gone");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let index = FileIndex::new();
        let file = dir.join("doomed.txt");
        std::fs::write(&file, b"x").unwrap();
        index.apply(file.clone(), ChangeAction::Added);
        index.flush();

        std::fs::remove_file(&file).unwrap();
        index.apply(file.clone(), ChangeAction::Removed);
        index.flush();

        assert!(!index.contains(&file));
        assert!(index.is_deleted(&file));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_change_bumps_the_version() {
        let index = FileIndex::new();
        let before = index.version();
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-version");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("changed.txt");
        std::fs::write(&file, b"x").unwrap();
        index.apply(file, ChangeAction::Added);
        index.flush();
        assert!(index.version() > before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_watcher_keeps_the_index_up_to_date() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-watcher");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let index = Arc::new(FileIndex::new());
        // The changes are applied in rounds, so that a program which writes
        // thousands of files does not keep the index busy. The test wants
        // the next round to come right away.
        index.set_update_interval(1);
        let watcher = index.watch(dir.clone()).expect("the folder can be watched");
        std::thread::sleep(Duration::from_millis(300));

        // A file that appears is in the index.
        let file = dir.join("watched.txt");
        std::fs::write(&file, b"x").unwrap();
        let mut appeared = false;
        for _ in 0..100 {
            if index.contains(&file) {
                appeared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(appeared, "the new file did not reach the index");

        // A file that is renamed away is not in the index any more.
        let moved = dir.join("moved-away.txt");
        std::fs::rename(&file, &moved).unwrap();
        let mut gone = false;
        for _ in 0..100 {
            if !index.contains(&file) {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(gone, "the renamed file is still in the index");
        assert!(index.is_deleted(&file));

        drop(watcher);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_walk_does_not_let_one_folder_take_everything() {
        // The first root holds a long chain of folders. A walk that dives
        // into it would use up the whole budget there and never come back
        // for the second root — and the file that sits right there would
        // never be found.
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-layers");
        let _ = std::fs::remove_dir_all(&dir);
        let deep = dir.join("deep");
        let mut current = deep.clone();
        let _ = std::fs::create_dir_all(&current);
        for level in 0..40 {
            let file = current.join(format!("level{level}.txt"));
            std::fs::write(&file, b"x").unwrap();
            current = current.join("deeper");
            let _ = std::fs::create_dir_all(&current);
        }
        let shallow = dir.join("shallow.txt");
        std::fs::write(&shallow, b"x").unwrap();

        let mut options = options(vec![deep.clone(), dir.clone()]);
        options.max_results = 12;
        let mut tree = Tree::default();
        walk(&mut tree, &options, &ScanProgress::default());

        let found = names(&tree);
        assert!(
            found.contains(&"shallow.txt".to_owned()),
            "the file next to the deep folder was not found"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_lies_in_two_roots_is_only_here_once() {
        // The home folder is walked through one level deep, and the folders
        // in it are walked through as well. The folder that is in both has
        // to be in the tree once — with its files under it.
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-two-roots");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("inner"));
        std::fs::write(dir.join("inner/the-file.txt"), b"x").unwrap();

        let mut options = options(vec![dir.join("inner")]);
        options.shallow_roots = vec![dir.clone()];
        let mut tree = Tree::default();
        walk(&mut tree, &options, &ScanProgress::default());

        let found = names(&tree);
        assert_eq!(
            found.iter().filter(|name| *name == "inner").count(),
            1,
            "the folder is in the tree twice: {found:?}"
        );
        // The file under it is found, and it is found once.
        let paths: Vec<String> = (0..tree.index.len() as u32)
            .map(|index| tree.index.path(index))
            .collect();
        assert_eq!(
            paths
                .iter()
                .filter(|path| path.ends_with("inner\\the-file.txt"))
                .count(),
            1,
            "{paths:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_query_finds_a_file_in_a_folder() {
        let dir = std::env::temp_dir().join("ideas-launcher-fileindex-query");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(dir.join("needle"));
        std::fs::write(dir.join("needle/haystack.txt"), b"x").unwrap();

        let mut tree = Tree::default();
        walk(&mut tree, &options(vec![dir.clone()]), &ScanProgress::default());
        let hits = tree
            .index
            .search_with_cancel("haystack", 10, None, FileSort::Relevance, None);
        assert!(!hits.is_empty(), "the file was not found");

        // The folder the file lies in is found as well.
        let hits = tree
            .index
            .search_with_cancel("needle", 10, None, FileSort::Relevance, None);
        assert!(!hits.is_empty(), "the folder was not found");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
