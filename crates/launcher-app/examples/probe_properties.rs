//! A probe that checks whether the properties dialog of the system opens.
//!
//! It is a throwaway tool: it calls the very function the menu of a row calls
//! and looks at the windows that are there afterwards.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use ideas_launcher::winapi;

fn titles() -> &'static Mutex<Vec<String>> {
    static TITLES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    TITLES.get_or_init(|| Mutex::new(Vec::new()))
}

unsafe extern "system" fn collect(hwnd: winapi::HWND, _: winapi::LPARAM) -> winapi::BOOL {
    let mut buffer = [0u16; 512];
    let length = unsafe { winapi::GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    if length > 0 && unsafe { winapi::IsWindowVisible(hwnd) } != 0 {
        let title = String::from_utf16_lossy(&buffer[..length as usize]);
        if !title.is_empty() {
            titles().lock().unwrap().push(title);
        }
    }
    1
}

/// The titles of the windows that are on the screen.
fn visible_titles() -> Vec<String> {
    titles().lock().unwrap().clear();
    unsafe {
        winapi::EnumWindows(Some(collect), 0);
    }
    titles().lock().unwrap().clone()
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("the path of the file to look at");
    let before = visible_titles();
    ideas_launcher::shellmenu::show_properties(std::ptr::null_mut(), &path);
    std::thread::sleep(Duration::from_millis(2500));
    let after = visible_titles();
    let fresh: Vec<String> = after
        .iter()
        .filter(|t| !before.contains(t))
        .cloned()
        .collect();
    println!("new windows: {fresh:?}");
    assert!(!fresh.is_empty(), "the properties dialog did not open");
    println!("OK");
}
