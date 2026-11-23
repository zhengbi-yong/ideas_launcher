//! What the right mouse button can do to a file.
//!
//! The menu itself is drawn by the launcher, but every entry of it calls the
//! system: *open with* opens the dialog of the system, *delete* puts the file
//! into the bin of the system, *properties* opens the page of the system.
//! So the behaviour is the same as in the file manager, only the look of the
//! menu is the one of the launcher.
//!
//! (The menu of the shell itself is asked for through COM — `SHParseDisplayName`
//! and `SHCreateDefaultContextMenu` — and it would be the very menu the file
//! manager shows. But with the classic right mouse menu of Windows 10 the
//! shell has to load *every* extension of the machine before it can put one
//! together, and the extensions wait for messages of a window that does not
//! exist on the thread that asks. On the drawing thread that is a deadlock, on
//! a thread of its own it takes the process down. A launcher that freezes or
//! dies on the right mouse button is worse than one with a menu of its own
//! whose entries call the system anyway.)
//!
//! The entries reach the system through the verbs of the shell. A verb is not
//! always found: `ShellExecute` looks for it in the registry of the file type
//! only, and the verb *properties* is not registered there on Windows 10 and
//! newer — the file manager draws that entry itself. The plain call answers
//! `SE_ERR_NOASSOC` (31) and nothing happens. `ShellExecuteEx` with
//! `SEE_MASK_INVOKEIDLIST` lets the shell look in the namespace of the shell
//! instead, and the entry works.

use std::ffi::c_void;

use crate::winapi::{self, HANDLE, HWND};

/// `SHFILEOPSTRUCTW`.
#[repr(C)]
struct FileOperation {
    hwnd: HWND,
    w_func: u16,
    p_from: *const u16,
    p_to: *const u16,
    f_flags: u16,
    f_any_operations_aborted: i32,
    h_name_mapping: HANDLE,
    l_progress: *mut c_void,
}

/// `FO_DELETE` — the operation throws the files away.
const FO_DELETE: u16 = 3;
/// `FOF_ALLOWUNDO` — the files go into the bin, not into nowhere.
const FOF_ALLOWUNDO: u16 = 0x0040;
/// `FOF_NOCONFIRMMKDIR` — empty folders go without a question.
const FOF_NOCONFIRMMKDIR: u16 = 0x0200;
/// `FOF_NOERRORUI` — no separate error box.
const FOF_NOERRORUI: u16 = 0x0400;

#[link(name = "shell32")]
unsafe extern "system" {
    /// Throws files away, or moves them.
    fn SHFileOperationW(operation: *mut FileOperation) -> i32;
}

/// Let the user pick the program a file is opened with.
///
/// The dialog is the one of the system, the same one the file manager shows.
pub fn open_with(hwnd: HWND, path: &str) {
    if run_verb(hwnd, path, "openas") {
        return;
    }
    // The verb of the shell is not there on every system — on Windows 11 it
    // answers "no program is associated". The program of the system for the
    // dialog is asked directly then.
    let program = to_wide(r"C:\Windows\System32\OpenWith.exe");
    let file = to_wide(path);
    unsafe {
        winapi::ShellExecuteW(
            hwnd,
            std::ptr::null(),
            program.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            winapi::SW_SHOW,
        );
    }
}

/// Show the properties of a file in the dialog of the system.
///
/// The plain [`winapi::ShellExecuteW`] cannot do it on Windows 10 and newer:
/// the verb *properties* is not registered for the file types any more — the
/// file manager draws that entry itself — so the shell answers
/// `SE_ERR_NOASSOC` (31) and nothing happens at all.
///
/// [`winapi::ShellExecuteExW`] with `SEE_MASK_INVOKEIDLIST` lets the shell
/// look the verb up in the namespace of the shell instead of in the registry
/// of the file type, and then the dialog of the system opens. The dialog is
/// a window of *this* process: the handler of the verb is loaded into the
/// program that asks for it.
pub fn show_properties(hwnd: HWND, path: &str) {
    if run_verb_ex(hwnd, path, "properties") {
        return;
    }
    // The older systems know the verb the simple way.
    run_verb(hwnd, path, "properties");
}

/// Run a file with one of the verbs of the shell, the way that works on the
/// newer systems.
///
/// Returns `false` when the shell did not take the job.
fn run_verb_ex(hwnd: HWND, path: &str, verb: &str) -> bool {
    let path_wide = to_wide(path);
    let verb_wide = to_wide(verb);
    unsafe {
        let mut info = winapi::ShellExecuteInfo {
            cb_size: std::mem::size_of::<winapi::ShellExecuteInfo>() as u32,
            f_mask: winapi::SEE_MASK_INVOKEIDLIST | winapi::SEE_MASK_FLAG_NO_UI,
            hwnd,
            lp_verb: verb_wide.as_ptr(),
            lp_file: path_wide.as_ptr(),
            lp_parameters: std::ptr::null(),
            lp_directory: std::ptr::null(),
            n_show: winapi::SW_SHOW,
            h_inst_app: std::ptr::null_mut(),
            lp_id_list: std::ptr::null_mut(),
            lp_class: std::ptr::null(),
            hkey_class: std::ptr::null_mut(),
            dw_hot_key: 0,
            h_icon: std::ptr::null_mut(),
            h_process: std::ptr::null_mut(),
        };
        let ok = winapi::ShellExecuteExW(&mut info);
        if ok == 0 {
            log::warn!(
                "the shell did not take {verb:?} for {path:?} ({:?})",
                std::io::Error::last_os_error()
            );
            return false;
        }
    }
    true
}

/// Run a file with one of the verbs of the shell.
///
/// Returns `false` when the shell did not take the job.
fn run_verb(hwnd: HWND, path: &str, verb: &str) -> bool {
    let path_wide = to_wide(path);
    let verb_wide = to_wide(verb);
    unsafe {
        let ok = winapi::ShellExecuteW(
            hwnd,
            verb_wide.as_ptr(),
            path_wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            winapi::SW_SHOW,
        );
        // Anything above 32 means the shell took the job.
        if ok as isize <= 32 {
            log::warn!("the shell did not take {verb:?} for {path:?} ({:?})", ok);
            return false;
        }
    }
    true
}

/// Throw a file away — into the bin, where the file manager keeps it.
///
/// The system asks whether it really should, the same way it asks in the file
/// manager.
pub fn delete_to_bin(hwnd: HWND, path: &str) {
    // The list of files has two nulls at the end.
    let mut list: Vec<u16> = to_wide(path).into_iter().chain(std::iter::once(0)).collect();
    list.push(0);
    unsafe {
        let mut operation = FileOperation {
            hwnd,
            w_func: FO_DELETE,
            p_from: list.as_ptr(),
            p_to: std::ptr::null(),
            f_flags: FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR | FOF_NOERRORUI,
            f_any_operations_aborted: 0,
            h_name_mapping: std::ptr::null_mut(),
            l_progress: std::ptr::null_mut(),
        };
        let hr = SHFileOperationW(&mut operation);
        if hr != 0 {
            log::warn!("the file could not be thrown away: {hr:#x}");
        } else if operation.f_any_operations_aborted != 0 {
            log::info!("the throwing away of the file was called off");
        } else {
            log::info!("the file went into the bin: {path}");
        }
    }
}

/// A string of the system, with the null at the end.
fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
