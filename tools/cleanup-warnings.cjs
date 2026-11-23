// Clean up the warnings introduced while adding the new plugins.
const fs = require('fs');
const path = require('path');

const app = 'crates/launcher-app/src';
const core = 'crates/launcher-core/src';

function patch(file, pairs) {
  const p = path.join(process.cwd(), file);
  let t = fs.readFileSync(p, 'utf8');
  let changed = 0;
  for (const [from, to] of pairs) {
    if (t.includes(from)) { t = t.split(from).join(to); changed++; }
    else console.error('NOT FOUND in ' + file + ':\n  ' + from.slice(0, 90).replace(/\n/g, '\\n'));
  }
  fs.writeFileSync(p, t, 'utf8');
  console.log(`${file}: applied ${changed}/${pairs.length}`);
}

// --- unused imports -------------------------------------------------------
patch(`${app}/app.rs`, [
  ['use launcher_core::query::Query;\n', ''],
  ['use crate::settings_view::{SettingsView, SettingsSection};', 'use crate::settings_view::SettingsView;'],
  ['    /// The plugin id.\n', '    /// The plugin id.\n'],
]);

patch(`${app}/plugins/sys.rs`, [
  ['use launcher_core::plugin::{PluginContext, PluginResult, QueryPlugin};',
   'use launcher_core::plugin::{PluginResult, QueryPlugin};'],
]);

patch(`${app}/settings_view.rs`, [
  ['use egui::{Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, Vec2};',
   'use egui::{Align2, Color32, FontId, Layout, Rect, RichText, Stroke, Vec2};'],
]);

// --- unsafe blocks inside unsafe fns (Rust 2024) --------------------------
patch(`${app}/plugins/processes.rs`, [
  [`    let mut pid = 0u32;
    winapi::GetWindowThreadProcessId(hwnd, &mut pid);
    if pid != 0 {
        let mut buf = [0u16; 512];
        let len = winapi::GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if len > 0 {
            let title = wide_to_string(&buf);`,
   `    let mut pid = 0u32;
    unsafe { winapi::GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid != 0 {
        let mut buf = [0u16; 512];
        let len = unsafe { winapi::GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        if len > 0 {
            let title = unsafe { wide_to_string(&buf) };`],
  [`    let handle = winapi::OpenProcess(
        winapi::PROCESS_QUERY_LIMITED_INFORMATION,
        0,
        pid,
    );`,
   `    let handle = unsafe {
        winapi::OpenProcess(winapi::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
    };`],
  [`    let path = if winapi::QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size) != 0 {
        wide_to_string(&buf)`,
   `    let path = if unsafe { winapi::QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size) } != 0 {
        wide_to_string(&buf)`],
  [`    let handle = winapi::OpenProcess(
        winapi::PROCESS_TERMINATE | winapi::PROCESS_QUERY_LIMITED_INFORMATION,
        0,
        pid,
    );`,
   `    let handle = unsafe {
        winapi::OpenProcess(
            winapi::PROCESS_TERMINATE | winapi::PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };`],
]);

patch(`${app}/plugins/sys.rs`, [
  [`    let mut token: winapi::HANDLE = std::ptr::null_mut();
    if winapi::OpenProcessToken(
        winapi::GetCurrentProcess(),
        winapi::TOKEN_ADJUST_PRIVILEGES | winapi::TOKEN_QUERY,
        &mut token,
    ) == 0`,
   `    let mut token: winapi::HANDLE = std::ptr::null_mut();
    let opened = unsafe {
        winapi::OpenProcessToken(
            winapi::GetCurrentProcess(),
            winapi::TOKEN_ADJUST_PRIVILEGES | winapi::TOKEN_QUERY,
            &mut token,
        )
    };
    if opened == 0`],
  [`    if winapi::LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) == 0 {
        winapi::CloseHandle(token);`,
   `    if unsafe { winapi::LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) } == 0 {
        unsafe { winapi::CloseHandle(token) };`],
  [`    let ok = winapi::AdjustTokenPrivileges(`,
   `    let ok = unsafe { winapi::AdjustTokenPrivileges(`],
  [`        previous,
        std::ptr::null_mut(),
    );
    winapi::CloseHandle(token);`,
   `        previous,
        std::ptr::null_mut(),
    ) };
    unsafe { winapi::CloseHandle(token) };`],
]);

// --- history helpers are only used by the tests ----------------------------
patch(`${core}/history.rs`, [
  ['/// Days since the Unix epoch for a civil date (Howard Hinnant\'s algorithm).\nfn days_from_civil',
   '/// Days since the Unix epoch for a civil date (Howard Hinnant\'s algorithm).\n#[cfg(test)]\nfn days_from_civil'],
  ['/// Unix timestamp for a civil date and time.\nfn civil_to_unix',
   '/// Unix timestamp for a civil date and time.\n#[cfg(test)]\nfn civil_to_unix'],
]);
