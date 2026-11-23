//! Relaunch the launcher with administrative privileges.
//!
//! Reading the NTFS master file table needs them. Everything asks for the
//! same in its own words:
//!
//! > "Everything" requires administrative privileges to index NTFS volumes.
//! > Run as administrator or install the Everything service to index NTFS
//! > volumes.
//!
//! The launcher offers the same choice: `--admin` starts a second copy with
//! the elevation prompt, and that copy then builds the full-disk index.

use std::os::windows::ffi::OsStrExt;

use crate::winapi;

/// The operation that asks the shell for elevation.
const RUN_AS_ADMIN: &str = "runas";

/// Start another copy of this program with administrative privileges.
///
/// Returns true when the shell accepted the request. The current process
/// keeps running; the caller decides whether to close it.
pub fn relaunch_as_admin() -> bool {
    relaunch_as_admin_with(&[])
}

/// Start another copy of this program with administrative privileges, with
/// some arguments of its own.
///
/// The arguments of the current process are handed on, so that
/// `--show --query …` still works after the elevation.
pub fn relaunch_as_admin_with(extra: &[&str]) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let exe = wide(&exe.to_string_lossy());

    // The arguments of the current process are handed on, so that
    // `--show --query …` still works after the elevation.
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let mut arguments = arguments
        .iter()
        .filter(|a| *a != "--admin")
        .cloned()
        .collect::<Vec<_>>();
    // The elevated copy is told to wait for the older process to let go of
    // the single-instance mutex.
    arguments.push("--elevated".to_string());
    for argument in extra {
        arguments.push(argument.to_string());
    }
    // The arguments are quoted: a query with a space in it
    // (`--query "hello world"`) would be cut in two otherwise, and the
    // elevated copy would look for the wrong thing.
    let arguments = arguments
        .iter()
        .map(|argument| {
            if argument.contains(' ') || argument.contains('"') {
                format!("\"{}\"", argument.replace('"', "\\\""))
            } else {
                argument.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let arguments = wide(&arguments);

    let operation = wide(RUN_AS_ADMIN);
    // `ShellExecuteW` returns a value greater than 32 when it worked.
    let result = unsafe {
        winapi::ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            exe.as_ptr(),
            if arguments.is_empty() {
                std::ptr::null()
            } else {
                arguments.as_ptr()
            },
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    result as isize > 32
}

/// Turn a string into a NUL terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_terminates_the_string() {
        let buffer = wide("ab");
        assert_eq!(buffer, vec![b'a' as u16, b'b' as u16, 0]);
    }
}
