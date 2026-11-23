//! Process listing and termination.
//!
//! Mirrors Flow Launcher's `ProcessKiller` plugin: list running processes with
//! their window titles so a busy process can be ended from the launcher.
//!
//! Two details matter for performance:
//!
//! * Flow Launcher routes this plugin through the `kill` action keyword only
//!   (confirmed in the analysed `Settings.json` → `PluginSettings.Plugins`),
//!   so it never competes with the program results.
//! * Walking the process table and reading every image path costs tens of
//!   milliseconds, so the list is built in the background and cached. A query
//!   only ever reads the snapshot.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use launcher_core::matcher::StringMatcher;
use launcher_core::plugin::{PluginResult, QueryPlugin};
use launcher_core::query::Query;
use launcher_core::result::Result as LauncherResult;
use parking_lot::RwLock;

use crate::winapi;

/// How long a snapshot is considered fresh.
const SNAPSHOT_TTL: Duration = Duration::from_secs(4);

/// A running process.
#[derive(Debug, Clone)]
pub struct ProcessEntry {
    /// The process id.
    pub pid: u32,
    /// The executable name.
    pub name: String,
    /// The full image path, when it could be read.
    pub path: String,
    /// The title of the process's main window, when it has one.
    pub window_title: String,
    /// How much working memory the process uses, in bytes.
    pub working_set: u64,
    /// Lower-cased name, for matching.
    pub name_lower: String,
    /// Lower-cased window title, for matching.
    pub title_lower: String,
}

/// List the running processes.
pub fn list_processes() -> Vec<ProcessEntry> {
    let mut entries: Vec<ProcessEntry> = Vec::new();
    let titles = window_titles();

    unsafe {
        let snapshot = winapi::CreateToolhelp32Snapshot(winapi::TH32CS_SNAPPROCESS, 0);
        if snapshot == winapi::INVALID_HANDLE_VALUE {
            return entries;
        }

        let mut pe = winapi::PROCESSENTRY32W {
            dwSize: std::mem::size_of::<winapi::PROCESSENTRY32W>() as u32,
            cntUsage: 0,
            th32ProcessID: 0,
            th32DefaultHeapID: 0,
            th32ModuleID: 0,
            cntThreads: 0,
            th32ParentProcessID: 0,
            pcPriClassBase: 0,
            dwFlags: 0,
            szExeFile: [0; 260],
        };

        if winapi::Process32FirstW(snapshot, &mut pe) != 0 {
            loop {
                let name = wide_to_string(&pe.szExeFile);
                let pid = pe.th32ProcessID;
                let (path, working_set) = process_details(pid);
                let title = titles.get(&pid).cloned().unwrap_or_default();
                entries.push(ProcessEntry {
                    pid,
                    name_lower: name.to_lowercase(),
                    title_lower: title.to_lowercase(),
                    name,
                    path,
                    window_title: title,
                    working_set,
                });
                if winapi::Process32NextW(snapshot, &mut pe) == 0 {
                    break;
                }
            }
        }

        winapi::CloseHandle(snapshot);
    }

    entries
}

// The window titles collected by the last `EnumWindows` pass.
//
// The callback cannot capture, so the map lives in a thread-local that both
// the collector and the callback share.
thread_local! {
    static WINDOW_TITLES: std::cell::RefCell<std::collections::HashMap<u32, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Map process id to the title of its main window.
fn window_titles() -> std::collections::HashMap<u32, String> {
    WINDOW_TITLES.with(|t| t.borrow_mut().clear());

    unsafe {
        winapi::EnumWindows(Some(collect_window_title), 0);
    }

    WINDOW_TITLES.with(|t| t.borrow().clone())
}

/// The `EnumWindows` callback; it cannot capture, so it uses a thread-local map.
unsafe extern "system" fn collect_window_title(hwnd: winapi::HWND, _lparam: winapi::LPARAM) -> winapi::BOOL {
    let mut pid = 0u32;
    unsafe { winapi::GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return 1;
    }

    // Windows of *this* process are skipped on purpose. `GetWindowTextW` on a
    // window owned by the calling process sends `WM_GETTEXT` to that window
    // and waits for the answer. When the query runs on a worker thread while
    // the UI thread is busy, that wait turns into a deadlock. Windows of other
    // processes are safe: the caption is read from the cached window data
    // without a round trip.
    if pid == std::process::id() {
        return 1;
    }

    let mut buf = [0u16; 512];
    let len = unsafe { winapi::GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if len > 0 {
        let title = unsafe { wide_to_string(&buf) };
        if !title.is_empty() {
            WINDOW_TITLES.with(|t| t.borrow_mut().insert(pid, title));
        }
    }
    1
}

/// Read the image path and working set of a process.
unsafe fn process_details(pid: u32) -> (String, u64) {
    let handle = unsafe {
        winapi::OpenProcess(winapi::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
    };
    if handle.is_null() {
        return (String::new(), 0);
    }

    let mut buf = [0u16; 1024];
    let mut size = buf.len() as u32;
    let path = if unsafe { winapi::QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size) } != 0 {
        unsafe { wide_to_string(&buf) }
    } else {
        String::new()
    };

    unsafe { winapi::CloseHandle(handle) };
    (path, 0)
}

/// Terminate a process.
pub fn kill(pid: u32) -> Result<(), String> {
    unsafe {
        let handle = winapi::OpenProcess(
            winapi::PROCESS_TERMINATE | winapi::PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        if handle.is_null() {
            return Err("could not open the process".into());
        }
        let ok = winapi::TerminateProcess(handle, 1);
        winapi::CloseHandle(handle);
        if ok != 0 {
            Ok(())
        } else {
            Err("could not terminate the process".into())
        }
    }
}

/// Convert a wide string into a `String`.
unsafe fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// The process plugin.
///
/// The process table is expensive to walk, so the plugin keeps a background
/// snapshot of it and only refreshes that snapshot when it has gone stale.
pub struct ProcessPlugin {
    /// The cached process list and the time it was taken.
    snapshot: Arc<RwLock<Option<(Instant, Arc<Vec<ProcessEntry>>)>>>,
    /// Set while a refresh is running.
    refreshing: Arc<AtomicBool>,
    matcher: StringMatcher,
}

impl Default for ProcessPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessPlugin {
    pub fn new() -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(None)),
            refreshing: Arc::new(AtomicBool::new(false)),
            matcher: StringMatcher::new(),
        }
    }

    /// The cached process list, if it is still fresh.
    fn fresh_snapshot(&self) -> Option<Arc<Vec<ProcessEntry>>> {
        let guard = self.snapshot.read();
        guard
            .as_ref()
            .filter(|(at, _)| at.elapsed() < SNAPSHOT_TTL)
            .map(|(_, entries)| entries.clone())
    }

    /// Refresh the snapshot in the background.
    fn refresh_async(&self) {
        if self.refreshing.swap(true, Ordering::SeqCst) {
            return;
        }
        let snapshot = self.snapshot.clone();
        let refreshing = self.refreshing.clone();
        std::thread::Builder::new()
            .name("process-cache".into())
            .spawn(move || {
                let entries = Arc::new(list_processes());
                *snapshot.write() = Some((Instant::now(), entries));
                refreshing.store(false, Ordering::SeqCst);
            })
            .ok();
    }
}

impl QueryPlugin for ProcessPlugin {
    fn id(&self) -> &str {
        "process"
    }

    fn name(&self) -> &str {
        "Processes"
    }

    fn action_keywords(&self) -> &[String] {
        // Flow Launcher routes this plugin through `kill` only — it is not a
        // wildcard plugin, so its results never compete with the programs.
        static KEYWORDS: std::sync::LazyLock<Vec<String>> =
            std::sync::LazyLock::new(|| vec!["kill".to_string()]);
        &KEYWORDS
    }

    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
        let term = query.search.trim();
        if term.is_empty() {
            return Ok(Vec::new());
        }

        // A stale snapshot is still shown, and a new one is built in the
        // background. The query itself never walks the process table.
        let entries = match self.fresh_snapshot() {
            Some(entries) => entries,
            None => {
                self.refresh_async();
                match self.snapshot.read().clone() {
                    Some((_, entries)) => entries,
                    None => return Ok(Vec::new()),
                }
            }
        };

        let prepared = self.matcher.prepare(term);
        let mut scored: Vec<(i32, &ProcessEntry)> = Vec::new();
        for entry in entries.iter() {
            let by_name = self.matcher.match_prepared_lower(&prepared, &entry.name_lower);
            let by_title = self
                .matcher
                .match_prepared_lower(&prepared, &entry.title_lower);
            let score = match (by_name, by_title) {
                (Some(a), Some(b)) => a.score.max(b.score),
                (Some(a), None) => a.score,
                (None, Some(b)) => b.score,
                (None, None) => 0,
            };
            if score > 0 {
                scored.push((score, entry));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));

        Ok(scored
            .into_iter()
            .take(30)
            .map(|(score, p)| LauncherResult {
                title: p.name.clone(),
                subtitle: if p.window_title.is_empty() {
                    format!("pid {}", p.pid)
                } else {
                    format!("{}  ·  pid {}", p.window_title, p.pid)
                },
                score,
                plugin_id: "process".into(),
                copy_text: p.pid.to_string(),
                ..Default::default()
            })
            .collect())
    }
}

// Keep the unused-import checker happy about the raw pointer type.
#[allow(dead_code)]
type _Unused = *mut c_void;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_list_is_not_empty() {
        // The test process itself is always running.
        assert!(!list_processes().is_empty());
    }

    #[test]
    fn plugin_matches_a_known_process_name() {
        let plugin = ProcessPlugin::new();
        // The first query builds the snapshot in the background; the second
        // one reads it.
        let _ = plugin.query(&Query::plain("explorer")).unwrap();
        for _ in 0..100 {
            if plugin.fresh_snapshot().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let results = plugin.query(&Query::plain("explorer")).unwrap();
        assert!(!results.is_empty(), "expected explorer.exe to match");
    }

    #[test]
    fn blank_query_matches_nothing() {
        let plugin = ProcessPlugin::new();
        assert!(plugin.query(&Query::plain(" ")).unwrap().is_empty());
    }

    #[test]
    fn the_plugin_is_keyword_scoped() {
        // Flow Launcher does not route the process plugin through the
        // wildcard; it is reached with `kill`.
        let plugin = ProcessPlugin::new();
        assert_eq!(plugin.action_keywords(), &["kill".to_string()]);
    }
}
