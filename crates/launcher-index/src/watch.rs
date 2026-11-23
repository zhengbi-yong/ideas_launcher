//! Watching folders for changes, without administrative privileges.
//!
//! Everything keeps its index up to date with the NTFS change journal, which
//! needs the "manage volume" privilege. A launcher that runs as the user
//! cannot read it: the volume cannot even be opened, and the log says so
//! every time. This module uses the other way of watching a folder:
//! `ReadDirectoryChangesW`. It needs no privilege at all — only a handle on
//! the folder — and it reports every change of the names in it: files that
//! appear, files that are written to and files that are deleted.
//!
//! The changes are handed over as they come, so the index is up to date within
//! milliseconds instead of after the next restart. That is what makes a file
//! that was created a second ago findable right away.

#![cfg(windows)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

type Handle = *mut c_void;
type Dword = u32;
type Bool = i32;

const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

/// The right to read the names in a folder.
const FILE_LIST_DIRECTORY: Dword = 0x0000_0001;
const FILE_SHARE_READ: Dword = 0x0000_0001;
const FILE_SHARE_WRITE: Dword = 0x0000_0002;
const FILE_SHARE_DELETE: Dword = 0x0000_0004;
const OPEN_EXISTING: Dword = 3;
/// Opening a folder needs this flag.
const FILE_FLAG_BACKUP_SEMANTICS: Dword = 0x0200_0000;
/// The handle has to be opened for overlapped operations.
///
/// Without this flag the call to read the changes blocks in the driver until
/// something happens, and there is no way to stop the thread that waits for
/// it.
const FILE_FLAG_OVERLAPPED: Dword = 0x4000_0000;

const FILE_NOTIFY_CHANGE_FILE_NAME: Dword = 0x0000_0001;
const FILE_NOTIFY_CHANGE_DIR_NAME: Dword = 0x0000_0002;
const FILE_NOTIFY_CHANGE_ATTRIBUTES: Dword = 0x0000_0004;
const FILE_NOTIFY_CHANGE_SIZE: Dword = 0x0000_0008;
const FILE_NOTIFY_CHANGE_LAST_WRITE: Dword = 0x0000_0010;
const FILE_NOTIFY_CHANGE_CREATION: Dword = 0x0000_0040;

const FILE_ACTION_ADDED: Dword = 1;
const FILE_ACTION_REMOVED: Dword = 2;
const FILE_ACTION_MODIFIED: Dword = 3;
const FILE_ACTION_RENAMED_OLD_NAME: Dword = 4;
const FILE_ACTION_RENAMED_NEW_NAME: Dword = 5;

const ERROR_OPERATION_ABORTED: Dword = 995;
const ERROR_IO_PENDING: Dword = 997;
const WAIT_OBJECT_0: Dword = 0x0000;
const WAIT_TIMEOUT: Dword = 0x0102;
const INFINITE: Dword = 0xFFFF_FFFF;

/// How many bytes of changes are collected in one go.
const BUFFER_SIZE: usize = 64 * 1024;

#[repr(C)]
struct SecurityAttributes {
    length: Dword,
    descriptor: *mut c_void,
    inherit_handle: Bool,
}

/// The structure that lets an operation run in the background.
///
/// The layout is the one of `OVERLAPPED` in the Windows headers: two words
/// for the status, the offset of the transfer (which shares its place with a
/// pointer) and the event that is set when the operation is over.
///
/// The order of the fields matters down to the byte: the driver writes the
/// event into the field at offset 24, and a field that sits anywhere else
/// means that the event is never set.
#[repr(C)]
#[derive(Default)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    h_event: Handle,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: Dword,
        share_mode: Dword,
        security_attributes: *mut SecurityAttributes,
        creation_disposition: Dword,
        flags_and_attributes: Dword,
        template_file: Handle,
    ) -> Handle;
    fn ReadDirectoryChangesW(
        directory: Handle,
        buffer: *mut c_void,
        buffer_length: Dword,
        watch_subtree: Bool,
        notify_filter: Dword,
        bytes_returned: *mut Dword,
        overlapped: *mut Overlapped,
        completion_routine: *const c_void,
    ) -> Bool;
    fn CancelIoEx(file: Handle, overlapped: *mut Overlapped) -> Bool;
    fn GetOverlappedResult(
        file: Handle,
        overlapped: *mut Overlapped,
        bytes_returned: *mut Dword,
        wait: Bool,
    ) -> Bool;
    fn CreateEventW(
        attributes: *const SecurityAttributes,
        manual_reset: Bool,
        initial_state: Bool,
        name: *const u16,
    ) -> Handle;
    fn SetEvent(handle: Handle) -> Bool;
    fn ResetEvent(handle: Handle) -> Bool;
    fn WaitForSingleObject(handle: Handle, milliseconds: Dword) -> Dword;
    fn WaitForMultipleObjects(
        count: Dword,
        handles: *const Handle,
        wait_all: Bool,
        milliseconds: Dword,
    ) -> Dword;
    fn CloseHandle(handle: Handle) -> Bool;
    fn GetLastError() -> Dword;
}

/// A Windows handle.
///
/// The pointer itself is not `Send`, but a handle may be used from another
/// thread — that is what the thread of the watcher does with it.
#[derive(Clone, Copy)]
struct OwnedHandle(Handle);

// SAFETY: a handle is a number that the operating system hands out; using it
// from another thread is what the thread of the watcher does with it.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

/// What happened to a name in the watched folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeAction {
    /// The file or folder appeared.
    Added,
    /// The file was written to.
    Modified,
    /// The file or folder is gone.
    Removed,
}

/// A folder that is watched for changes.
///
/// The watcher owns a thread of its own, which waits for the changes and hands
/// them over. Dropping it stops the thread and closes the handle.
pub struct Watcher {
    /// Set when the watcher should stop.
    stop: Arc<AtomicBool>,
    /// The event that tells the thread to stop.
    stop_event: OwnedHandle,
    /// The thread that waits for the changes.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        unsafe {
            SetEvent(self.stop_event.0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        unsafe {
            CloseHandle(self.stop_event.0);
        }
    }
}

impl Watcher {
    /// Watch a folder and everything below it.
    ///
    /// The changes are handed over to `on_change`. When more changes happen
    /// than fit into one buffer, `on_overflow` is called instead: the names
    /// that were lost cannot be recovered, so the caller has to look at the
    /// folder again.
    ///
    /// Returns `None` when the folder cannot be opened — a folder on a drive
    /// that is not connected, for example.
    pub fn spawn(
        root: &Path,
        on_change: impl Fn(PathBuf, ChangeAction) + Send + 'static,
        on_overflow: impl Fn() + Send + 'static,
    ) -> Option<Self> {
        let mut name: Vec<u16> = root.as_os_str().encode_wide().collect();
        name.push(0);
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        let stop_event = unsafe {
            CreateEventW(
                std::ptr::null(),
                1,
                0,
                std::ptr::null(),
            )
        };
        if stop_event.is_null() {
            unsafe {
                CloseHandle(handle);
            }
            return None;
        }

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        // Set once the watch is really armed — that is, once the driver has
        // been asked for the changes of the folder.
        //
        // Without it there is a window in which the folder is watched by
        // nobody: the thread has been started but has not asked the driver
        // yet, and a file that appears in that window is never reported.
        // The launcher arms the watch **before** it walks the folder, so a
        // file that appears during the walk is reported twice (once by the
        // walk, once by the watch) — which is harmless — and never missed.
        let armed = Arc::new(AtomicBool::new(false));
        let thread_armed = armed.clone();
        let root = root.to_path_buf();
        let owned = OwnedHandle(handle);
        let owned_stop = OwnedHandle(stop_event);
        let thread = std::thread::Builder::new()
            .name("fs-watch".into())
            .spawn(move || {
                watch_loop(
                    owned,
                    owned_stop,
                    &root,
                    &thread_stop,
                    &thread_armed,
                    on_change,
                    on_overflow,
                );
                unsafe {
                    CloseHandle(owned.0);
                }
            })
            .ok();
        let Some(thread) = thread else {
            // The thread could not be started. The handles are of no use then,
            // and the folder and the event have to go — otherwise they stay
            // open for the rest of the session.
            unsafe {
                CloseHandle(owned.0);
                CloseHandle(owned_stop.0);
            }
            return None;
        };
        // Wait for the watch to be armed, so that the caller knows that from
        // here on nothing is missed. Two seconds is plenty; the thread only
        // has to open the driver.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !armed.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Some(Self {
            stop,
            stop_event: OwnedHandle(stop_event),
            thread: Some(thread),
        })
    }
}

/// Wait for changes until the watcher is stopped.
fn watch_loop(
    handle: OwnedHandle,
    stop_event: OwnedHandle,
    root: &Path,
    stop: &AtomicBool,
    armed: &AtomicBool,
    on_change: impl Fn(PathBuf, ChangeAction) + Send + 'static,
    on_overflow: impl Fn() + Send + 'static,
) {
    let handle = handle.0;
    let stop_event = stop_event.0;
    let io_event = unsafe {
        CreateEventW(std::ptr::null(), 1, 0, std::ptr::null())
    };
    if io_event.is_null() {
        return;
    }

    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut overlapped = Overlapped {
        h_event: io_event,
        ..Default::default()
    };
    let waits = [io_event, stop_event];

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        unsafe {
            ResetEvent(io_event);
        }
        let ok = unsafe {
            ReadDirectoryChangesW(
                handle,
                buffer.as_mut_ptr().cast(),
                buffer.len() as Dword,
                1,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_ATTRIBUTES
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_CREATION,
                // In this mode the number of bytes comes with the overlapped
                // result, so the call does not get a place for it.
                std::ptr::null_mut(),
                &mut overlapped,
                std::ptr::null(),
            )
        };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_PENDING {
                // The folder is gone, or the handle cannot be used any more.
                break;
            }
        }
        // The driver is now watching the folder: from here on every change
        // is reported. The caller waits for this, so that nothing that
        // happens right after the watcher was made is lost.
        armed.store(true, Ordering::SeqCst);
        // Wait for the changes, or for the watcher to be stopped.
        //
        // The event is set when the operation is over — whether it was over
        // right away or later on.
        let waited = unsafe { WaitForMultipleObjects(2, waits.as_ptr(), 0, INFINITE) };
        if waited == WAIT_OBJECT_0 + 1 {
            // The stop event: the pending operation is cancelled, so that
            // the thread does not stay stuck in the driver.
            unsafe {
                CancelIoEx(handle, &mut overlapped);
                WaitForSingleObject(io_event, INFINITE);
            }
            break;
        }
        if waited != WAIT_OBJECT_0 && waited != WAIT_TIMEOUT {
            break;
        }
        // The number of bytes in the buffer comes from the overlapped result.
        let mut transferred: Dword = 0;
        let got = unsafe { GetOverlappedResult(handle, &mut overlapped, &mut transferred, 1) };
        if got == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_OPERATION_ABORTED {
                break;
            }
            continue;
        }
        if transferred == 0 {
            // More changes happened than fit into the buffer. The names that
            // were lost cannot be recovered, so the folder has to be looked
            // at again.
            on_overflow();
            continue;
        }
        let found = parse_changes(&buffer[..transferred as usize]);
        for (name, action) in found {
            on_change(root.join(name), action);
        }
    }

    unsafe {
        CloseHandle(io_event);
    }
}

/// Read the names out of the buffer that the driver filled.
fn parse_changes(buffer: &[u8]) -> Vec<(String, ChangeAction)> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + 12 <= buffer.len() {
        let next = u32::from_le_bytes(
            buffer[offset..offset + 4]
                .try_into()
                .expect("four bytes for the offset"),
        ) as usize;
        let action = u32::from_le_bytes(
            buffer[offset + 4..offset + 8]
                .try_into()
                .expect("four bytes for the action"),
        );
        let name_len = u32::from_le_bytes(
            buffer[offset + 8..offset + 12]
                .try_into()
                .expect("four bytes for the length"),
        ) as usize;
        let start = offset + 12;
        let end = start + name_len;
        if end > buffer.len() {
            break;
        }
        let name = decode_name(&buffer[start..end]);
        let action = match action {
            FILE_ACTION_ADDED => ChangeAction::Added,
            FILE_ACTION_REMOVED => ChangeAction::Removed,
            FILE_ACTION_RENAMED_OLD_NAME => ChangeAction::Removed,
            FILE_ACTION_RENAMED_NEW_NAME => ChangeAction::Added,
            FILE_ACTION_MODIFIED => ChangeAction::Modified,
            _ => ChangeAction::Modified,
        };
        out.push((name, action));
        if next == 0 {
            break;
        }
        offset += next;
    }
    out
}

/// Turn the UTF-16 name of the driver into a string.
fn decode_name(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_names_of_the_driver() {
        // The layout of one entry of `FILE_NOTIFY_INFORMATION`: the offset of
        // the next entry, the action, the length of the name in bytes and the
        // name itself in UTF-16.
        let mut buffer: Vec<u8> = Vec::new();
        for (action, name) in [(1u32, "a.txt"), (2, "b.txt"), (3, "c.txt")] {
            let start = buffer.len();
            let units: Vec<u16> = name.encode_utf16().collect();
            let name_bytes = units.len() * 2;
            let next: u32 = if name == "c.txt" {
                0
            } else {
                (12 + name_bytes) as u32
            };
            buffer.extend_from_slice(&next.to_le_bytes());
            buffer.extend_from_slice(&action.to_le_bytes());
            buffer.extend_from_slice(&(name_bytes as u32).to_le_bytes());
            for unit in units {
                buffer.extend_from_slice(&unit.to_le_bytes());
            }
            let _ = start;
        }
        let found = parse_changes(&buffer);
        let names: Vec<&str> = found.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
        assert_eq!(found[0].1, ChangeAction::Added);
        assert_eq!(found[1].1, ChangeAction::Removed);
        assert_eq!(found[2].1, ChangeAction::Modified);
    }

    #[test]
    fn reports_a_new_file() {
        let dir = std::env::temp_dir().join("ideas-launcher-watch-test");
        let _ = std::fs::create_dir_all(&dir);
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let watcher = Watcher::spawn(
            &dir,
            move |path, action| {
                sink.lock().unwrap().push((path, action));
            },
            || {},
        )
        .expect("the folder can be watched");

        // The watcher is armed when `spawn` returns, so the file that comes
        // now cannot be missed.
        let file = dir.join("watched.txt");
        std::fs::write(&file, b"x").unwrap();

        let mut found = false;
        for _ in 0..100 {
            if seen.lock().unwrap().iter().any(|(path, _)| *path == file) {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        drop(watcher);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(found, "the new file was not reported");
    }

    #[test]
    fn reports_a_deleted_file() {
        let dir = std::env::temp_dir().join("ideas-launcher-watch-del");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("doomed.txt");
        std::fs::write(&file, b"x").unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let watcher = Watcher::spawn(
            &dir,
            move |path, action| {
                sink.lock().unwrap().push((path, action));
            },
            || {},
        )
        .expect("the folder can be watched");
        std::fs::remove_file(&file).unwrap();

        let mut found = false;
        for _ in 0..100 {
            if seen
                .lock()
                .unwrap()
                .iter()
                .any(|(path, action)| *path == file && *action == ChangeAction::Removed)
            {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        drop(watcher);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(found, "the deleted file was not reported");
    }
}
