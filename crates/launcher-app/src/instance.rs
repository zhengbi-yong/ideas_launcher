//! Single-instance support.
//!
//! The launcher is a single-instance program: two instances would fight over
//! the global hotkey. But "single instance" must not mean "starting it again
//! does nothing" — the second instance asks the first one to show its
//! window, which is what the user expects when they click the icon again.
//!
//! The names of the mutex and of the event are derived from the path of the
//! executable, so a development build and an installed one do not get in each
//! other's way.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::hotkey;
use crate::winapi::*;

/// A name that is unique to this installation.
pub fn instance_id() -> String {
    let key = std::env::current_exe()
        .map(|path| path.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    format!("{:016x}", fnv1a(&key))
}

/// A stable hash of a string.
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// The name of the mutex that keeps a single instance.
fn mutex_name(id: &str) -> Vec<u16> {
    format!("Local\\IdeasLauncherSingleton-{id}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

/// The name of the event another instance uses to ask for the window.
fn show_event_name(id: &str) -> Vec<u16> {
    format!("Local\\IdeasLauncherShow-{id}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

/// Claim the single-instance mutex.
///
/// Returns `false` when another instance already owns it.
pub fn claim(id: &str) -> bool {
    let name = mutex_name(id);
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        if handle.is_null() {
            // The mutex could not be created; carry on rather than refusing
            // to start.
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // The handle is closed again. The mutex belongs to the other
            // process, and holding on to the handle would keep the object
            // alive after that process is gone — so the copy that waits for
            // it could never take it over. (The elevated copy waits for the
            // ordinary one to let go, and that is exactly this case.)
            CloseHandle(handle);
            return false;
        }
        // The handle is intentionally leaked: it has to stay open for the
        // lifetime of the process.
        let _keep = handle;
        true
    }
}

/// Claim the single-instance mutex, waiting for it to be released.
///
/// An elevated copy is started while the ordinary one is still running, so
/// it has to wait for the older process to let go of the mutex.
pub fn claim_with_wait(id: &str, timeout_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if claim(id) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Ask the running instance to show its window.
///
/// Returns `false` when there is no instance to ask — which should not
/// happen, because the mutex said so.
pub fn request_show(id: &str) -> bool {
    let name = show_event_name(id);
    unsafe {
        let handle = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if handle.is_null() {
            return false;
        }
        let signalled = SetEvent(handle) != 0;
        CloseHandle(handle);
        signalled
    }
}

/// Watch for another instance asking for the window.
///
/// The thread waits on the event and shows the window when it is signalled.
pub fn watch_for_show_request(
    id: &str,
    hwnd: usize,
    visible_flag: Arc<AtomicBool>,
    toggle_requested: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    let name = show_event_name(id);
    unsafe {
        // An auto-reset event: one signal, one show.
        let handle = CreateEventW(std::ptr::null(), 0, 0, name.as_ptr());
        if handle.is_null() {
            return;
        }
        // The handle is a plain kernel handle; it is carried as an integer
        // because a raw pointer cannot be sent to another thread.
        let handle = handle as usize;
        std::thread::Builder::new()
            .name("show-request".into())
            .spawn(move || {
                let handle = handle as HANDLE;
                loop {
                    let result = WaitForSingleObject(handle, 1000);
                    if result != WAIT_OBJECT_0 {
                        continue;
                    }
                    // The window is shown, not toggled: asking for it twice
                    // has to end with it open.
                    toggle_requested.store(true, Ordering::SeqCst);
                    visible_flag.store(true, Ordering::SeqCst);
                    hotkey::force_foreground(hwnd as HWND);
                    ctx.request_repaint();
                    hotkey::wake(hwnd);
                }
            })
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_instance_id_is_stable() {
        assert_eq!(instance_id(), instance_id());
        assert_eq!(instance_id().len(), 16);
    }

    #[test]
    fn the_mutex_and_the_event_have_different_names() {
        let id = instance_id();
        assert_ne!(mutex_name(&id), show_event_name(&id));
    }
}
