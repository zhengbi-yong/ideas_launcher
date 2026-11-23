//! The system tray icon.
//!
//! Flow Launcher lives in the notification area: the icon is the only way to
//! reach the settings and to quit when the launcher has no window on screen.
//!
//! The icon is owned by a hidden window of its own, because the shell sends
//! its callbacks to that window. The window lives on the thread that runs the
//! user interface, so the messages are dispatched by the normal message loop.

use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

use crate::winapi::*;

/// What the user asked for through the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Nothing.
    None,
    /// Show the search window.
    Show,
    /// Show the window with the settings.
    Settings,
    /// Start the launcher again with administrative privileges.
    ///
    /// Reading the master file table needs them, and so does the full-disk
    /// index in general.
    RunAsAdministrator,
    /// Read the master file table again.
    RebuildFileIndex,
    /// Quit the application.
    Quit,
}

/// The message the shell sends when the tray icon is clicked.
const WM_TRAY_CALLBACK: u32 = WM_APP_WAKE + 1;

/// The identifier of the tray icon.
const TRAY_ICON_ID: u32 = 1;

/// The commands of the tray menu.
const MENU_SHOW: usize = 1001;
const MENU_SETTINGS: usize = 1002;
const MENU_QUIT: usize = 1003;
const MENU_ADMIN: usize = 1004;
const MENU_REBUILD: usize = 1005;

/// The window class of the hidden tray window.
const TRAY_CLASS: &str = "IdeasLauncherTrayWindow";

/// The state the tray window shares with the application.
struct Shared {
    command: Mutex<TrayCommand>,
    /// The message the shell sends when the task bar is created again.
    taskbar_created: u32,
    /// The icon that is in the notification area.
    ///
    /// The task bar is sometimes rebuilt, and then the icon has to be added
    /// again. The old one has to be freed then — otherwise every rebuild
    /// leaves one more icon behind, and they are GDI objects that nobody
    /// ever counts.
    icon: AtomicIsize,
    /// The hotkey that really works, shown in the menu.
    hotkey: String,
    /// The context of the user interface.
    ///
    /// It is asked for another frame whenever the user picks something. The
    /// launcher window is hidden most of the time, and then nothing else
    /// would draw a frame — the command would sit in the queue until the
    /// next hotkey press.
    ctx: egui::Context,
}

/// The tray icon.
pub struct Tray {
    shared: Arc<Shared>,
    /// The icon handle; it is destroyed when the tray is dropped.
    icon: HICON,
    /// The hidden window that receives the shell callbacks.
    hwnd: HWND,
    added: bool,
    /// Whether the icon is currently asked to stay in the open notification
    /// area.
    ///
    /// `0` means "not known yet" — the shell may have left the icon in the
    /// open from an earlier run, so the first request has to be carried out
    /// even when it asks for the same thing.
    promoted: AtomicU8,
}

impl Tray {
    /// Put the launcher into the notification area.
    ///
    /// `hotkey` is the combination that really works right now — it is shown
    /// in the menu, because the configured one may have been taken by another
    /// program and the launcher fell back to something else.
    ///
    /// `ctx` is the context of the user interface. It is asked for another
    /// frame when the user picks something, because the window is usually
    /// hidden and nothing else would wake it up.
    ///
    /// Returns `None` when the shell refuses the icon; the launcher then
    /// simply has no tray icon, which is not fatal.
    pub fn new(tip: &str, hotkey: &str, ctx: egui::Context) -> Option<Self> {
        unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let class_name: Vec<u16> = TRAY_CLASS.encode_utf16().chain(std::iter::once(0)).collect();

            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(tray_wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
            };
            RegisterClassW(&wc);

            let taskbar_created = {
                let name: Vec<u16> = "TaskbarCreated".encode_utf16().chain(std::iter::once(0)).collect();
                RegisterWindowMessageW(name.as_ptr())
            };

            let shared = Arc::new(Shared {
                command: Mutex::new(TrayCommand::None),
                taskbar_created,
                icon: AtomicIsize::new(0),
                hotkey: hotkey.to_owned(),
                ctx,
            });

            // A hidden top-level window (not a message-only one): it has to be
            // able to own the context menu and to come to the foreground.
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                log::warn!("could not create the tray window");
                return None;
            }

            // The window procedure needs the shared state; it is stored with
            // the window itself.
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                Arc::as_ptr(&shared) as isize,
            );

            // Use the icon that is embedded in the executable.
            let icon = load_application_icon();
            shared.icon.store(icon as isize, Ordering::Relaxed);

            let mut data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_ICON_ID,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                uCallbackMessage: WM_TRAY_CALLBACK,
                hIcon: icon,
                ..Default::default()
            };
            copy_wide(tip, &mut data.szTip);

            let added = Shell_NotifyIconW(NIM_ADD, &mut data) != 0;
            if !added {
                log::warn!("the shell refused the tray icon");
            }
            negotiate_version(&mut data);

            Some(Self {
                shared,
                icon,
                hwnd,
                added,
                promoted: AtomicU8::new(0),
            })
        }
    }

    /// Keep the icon in the open notification area, or let it hide in the
    /// overflow panel.
    ///
    /// Windows decides where an icon goes: a new one usually lands in the
    /// hidden part of the tray, where it takes up no room on the task bar but
    /// is also never found. The launcher can ask the shell to keep it in the
    /// open instead — and to fold it away again when the user changes their
    /// mind.
    pub fn set_promoted(&self, promoted: bool) {
        let wanted = if promoted { 1 } else { 2 };
        if self.promoted.swap(wanted, Ordering::SeqCst) == wanted {
            return;
        }
        let exe = std::env::current_exe()
            .ok()
            .map(|path| path.to_string_lossy().to_lowercase());
        std::thread::spawn(move || set_tray_icon_promoted(exe.as_deref(), promoted));
    }

    /// Take the command the user chose, if any.
    pub fn take_command(&self) -> TrayCommand {
        let mut command = self.shared.command.lock();
        let taken = *command;
        *command = TrayCommand::None;
        taken
    }

    /// Whether the icon really is in the notification area.
    pub fn is_added(&self) -> bool {
        self.added
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            if self.added {
                let mut data = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: self.hwnd,
                    uID: TRAY_ICON_ID,
                    ..Default::default()
                };
                Shell_NotifyIconW(NIM_DELETE, &mut data);
            }
            if !self.icon.is_null() {
                DestroyIcon(self.icon);
            }
            if !self.hwnd.is_null() {
                DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Copy a string into a fixed size wide buffer.
fn copy_wide(text: &str, buffer: &mut [u16]) {
    for (slot, ch) in buffer.iter_mut().zip(text.encode_utf16()) {
        *slot = ch;
    }
}

/// The window procedure of the hidden tray window.
unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if pointer == 0 {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let shared = &*(pointer as *const Shared);

        // The shell re-creates the task bar now and then; the icon has to be
        // added again afterwards.
        if msg == shared.taskbar_created {
            add_icon_again(hwnd, shared);
            return 0;
        }

        if msg == WM_TRAY_CALLBACK {
            log::info!(
                "the tray callback: wparam=0x{:X} lparam=0x{:X}",
                wparam, lparam
            );
            match notification_code(lparam) {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    set_command(shared, TrayCommand::Show);
                    return 0;
                }
                WM_RBUTTONUP => {
                    show_context_menu(hwnd, shared);
                    return 0;
                }
                _ => {}
            }
        }

        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

/// Store the command the user chose.
fn set_command(shared: &Shared, command: TrayCommand) {
    *shared.command.lock() = command;
    // The launcher window is hidden most of the time, and a hidden window
    // does not draw a frame on its own. Without this the command would wait
    // in the queue until the next hotkey press.
    shared.ctx.request_repaint();
}

/// The notification code of a tray callback.
///
/// The shell puts the mouse notification in the **low word of `lParam`**; the
/// high word carries a notification event of its own. Older versions put the
/// whole of `lParam` there, so the low word is read either way.
///
/// `wParam` is *not* the notification code: with the fourth version of the
/// notification data it carries the position of the mouse instead, and with
/// the older ones it carries the id of the icon. Reading the code from
/// there means that neither the left nor the right button ever matches, and
/// the icon does nothing at all.
fn notification_code(lparam: LPARAM) -> u32 {
    (lparam as u32) & 0xFFFF
}

/// Put the icon into the visible part of the notification area, or fold it
/// away into the overflow panel.
///
/// Windows keeps that choice per program in the registry, under a key that
/// holds the path of the executable. The key can therefore be looked up
/// instead of guessed.
fn set_tray_icon_promoted(exe: Option<&str>, promoted: bool) {
    let Some(exe) = exe else {
        return;
    };
    // The shell writes the key a moment after the icon appears, so this is
    // retried for a while before giving up.
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let Some(key) = find_tray_key(exe) else {
            continue;
        };
        write_tray_key_promoted(&key, promoted);
        return;
    }
    log::info!("the shell has not registered the tray icon yet");
}

/// The registry key of the shell that holds the tray icon of this program.
const TRAY_SETTINGS_KEY: &str = "Control Panel\\NotifyIconSettings";

/// Find the key the shell keeps for this program.
fn find_tray_key(exe: &str) -> Option<String> {
    let parent: Vec<u16> = TRAY_SETTINGS_KEY.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey: HKEY = std::ptr::null_mut();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, parent.as_ptr(), 0, KEY_READ, &mut hkey) } != 0 {
        return None;
    }

    let mut found = None;
    let mut index = 0u32;
    loop {
        let mut name = vec![0u16; 256];
        let mut len = name.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                hkey,
                index,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            break;
        }
        index += 1;
        let key = String::from_utf16_lossy(&name[..len as usize]);
        if let Some(path) = tray_key_executable(&key) {
            if path.to_lowercase() == exe {
                found = Some(key);
                break;
            }
        }
    }
    unsafe { RegCloseKey(hkey) };
    found
}

/// The executable the shell has registered under a tray key.
fn tray_key_executable(key: &str) -> Option<String> {
    let sub: Vec<u16> = format!("{TRAY_SETTINGS_KEY}\\{key}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let value: Vec<u16> = "ExecutablePath".encode_utf16().chain(std::iter::once(0)).collect();

    let mut data = vec![0u16; 1024];
    let mut size = (data.len() * 2) as u32;
    let mut kind = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            &mut kind,
            data.as_mut_ptr() as *mut std::ffi::c_void,
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    // The size counts the terminating zero as well.
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&data[..len]))
}

/// Tell the shell whether to show the icon of a key in the open notification
/// area.
fn write_tray_key_promoted(key: &str, promoted: bool) {
    let sub: Vec<u16> = format!("{TRAY_SETTINGS_KEY}\\{key}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut hkey: HKEY = std::ptr::null_mut();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) } != 0 {
        return;
    }
    let value: Vec<u16> = "IsPromoted".encode_utf16().chain(std::iter::once(0)).collect();
    let data: u32 = u32::from(promoted);
    let status = unsafe {
        RegSetValueExW(
            hkey,
            value.as_ptr(),
            0,
            REG_DWORD,
            &data as *const u32 as *const u8,
            std::mem::size_of::<u32>() as u32,
        )
    };
    unsafe { RegCloseKey(hkey) };
    if status == 0 {
        log::info!(
            "the tray icon was {} the notification area",
            if promoted { "put into" } else { "folded into" }
        );
    }
}

/// Add the icon to the tray again.
///
/// The shell re-creates the task bar now and then, and the icon has to be
/// added again afterwards.
///
/// The version has to be negotiated again as well. Without it the shell
/// goes back to the old behaviour, in which it only sends a message for the
/// double click — and the right button is completely silent.
unsafe fn add_icon_again(hwnd: HWND, shared: &Shared) {
    unsafe {
        let icon = load_application_icon();
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY_CALLBACK,
            hIcon: icon,
            ..Default::default()
        };
        copy_wide("IDEAS Launcher", &mut data.szTip);
        if Shell_NotifyIconW(NIM_ADD, &mut data) == 0 {
            log::warn!("the shell refused the tray icon again");
            // The icon is of no use now, and nobody is going to free it.
            if !icon.is_null() {
                DestroyIcon(icon);
            }
            return;
        }
        // The old icon is of no use any more.
        let old = shared.icon.swap(icon as isize, Ordering::Relaxed);
        if old != 0 {
            DestroyIcon(old as HICON);
        }
        negotiate_version(&mut data);
    }
}

/// Ask the shell for the fourth version of the notification data.
///
/// Only that version tells the left, the right and the middle button apart.
/// In the older ones the shell sends a message for the double click only, and
/// the right button never says anything at all.
fn negotiate_version(data: &mut NOTIFYICONDATAW) {
    data.uVersion = NOTIFYICON_VERSION_4;
    let status = unsafe { Shell_NotifyIconW(NIM_SETVERSION, data) };
    log::info!("the tray icon negotiated the fourth version: {status}");
    if status == 0 {
        log::warn!(
            "the shell refused the fourth version of the tray icon; \
             the right button of the icon will not work"
        );
    }
}

/// Show the context menu of the tray icon.
unsafe fn show_context_menu(hwnd: HWND, shared: &Shared) {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        // The menu shows the hotkey that really works — the configured one may
        // have been taken by another program, and the launcher then fell back
        // to a different combination.
        let hint = if shared.hotkey.is_empty() {
            String::new()
        } else {
            format!("\t{}", shared.hotkey)
        };
        let show: Vec<u16> = format!("显示搜索窗口{hint}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let settings: Vec<u16> = "设置…".encode_utf16().chain(std::iter::once(0)).collect();
        let admin: Vec<u16> = "以管理员身份重新启动…".encode_utf16().chain(std::iter::once(0)).collect();
        let rebuild: Vec<u16> = "重建全盘索引".encode_utf16().chain(std::iter::once(0)).collect();
        let quit: Vec<u16> = "退出".encode_utf16().chain(std::iter::once(0)).collect();
        AppendMenuW(menu, MF_STRING, MENU_SHOW, show.as_ptr());
        AppendMenuW(menu, MF_STRING, MENU_SETTINGS, settings.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, MENU_ADMIN, admin.as_ptr());
        AppendMenuW(menu, MF_STRING, MENU_REBUILD, rebuild.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, MENU_QUIT, quit.as_ptr());

        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        // The window has to be in the foreground, otherwise the menu does not
        // close when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            cursor.x,
            cursor.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);
        PostMessageW(hwnd, WM_NULL, 0, 0);

        match command {
            MENU_SHOW => set_command(shared, TrayCommand::Show),
            MENU_SETTINGS => set_command(shared, TrayCommand::Settings),
            MENU_ADMIN => set_command(shared, TrayCommand::RunAsAdministrator),
            MENU_REBUILD => set_command(shared, TrayCommand::RebuildFileIndex),
            MENU_QUIT => set_command(shared, TrayCommand::Quit),
            _ => {}
        }
    }
}

/// Load the icon that is embedded in the executable.
unsafe fn load_application_icon() -> HICON {
    unsafe {
        let Ok(exe) = std::env::current_exe() else {
            return std::ptr::null_mut();
        };
        let wide: Vec<u16> = exe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut large: HICON = std::ptr::null_mut();
        let mut small: HICON = std::ptr::null_mut();
        let count = ExtractIconExW(wide.as_ptr(), 0, &mut large, &mut small, 1);
        if count == 0 || large.is_null() {
            return std::ptr::null_mut();
        }
        if !small.is_null() {
            DestroyIcon(small);
        }
        large
    }
}

// The window procedure needs a few constants that are not in `winapi` yet.
#[allow(dead_code)]
const WM_LBUTTONUP: u32 = 0x0202;
#[allow(dead_code)]
const WM_LBUTTONDBLCLK: u32 = 0x0203;
#[allow(dead_code)]
const WM_RBUTTONDOWN: u32 = 0x0204;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_NULL: u32 = 0x0000;
const GWLP_USERDATA: i32 = -21;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tray_can_be_created() {
        // Creating the tray icon must not panic, and the command starts out
        // empty.
        let ctx = egui::Context::default();
        if let Some(tray) = Tray::new("IDEAS Launcher", "Ctrl + Space", ctx) {
            assert_eq!(tray.take_command(), TrayCommand::None);
        }
    }

    #[test]
    fn the_notification_code_is_read_from_the_right_place() {
        // The shell puts the mouse notification in the low word of `lParam`;
        // the high word carries a notification event of its own. Comparing
        // the whole word with the notification code means that the right
        // button never matches, and the icon does nothing at all.
        //
        // These are the values a real click on the icon produces.
        assert_eq!(notification_code(0x0000_0205), WM_RBUTTONUP);
        assert_eq!(notification_code(0x0001_0205), WM_RBUTTONUP);
        assert_eq!(notification_code(0x0001_0204), WM_RBUTTONDOWN);
        assert_eq!(notification_code(0x0001_0202), WM_LBUTTONUP);
        // The older versions put the whole code there.
        assert_eq!(notification_code(0x0000_0203), WM_LBUTTONDBLCLK);
    }
}
