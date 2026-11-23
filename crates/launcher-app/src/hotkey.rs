//! Global hotkey handling.
//!
//! Uses `RegisterHotKey` on a message-only window owned by a dedicated
//! thread. Compared with a low-level keyboard hook — which is what Flow
//! Launcher uses (`SetWindowsHookEx`/`UnhookWindowsHookEx` appear in its
//! P/Invoke table) — this costs nothing while the user is typing, because
//! the OS dispatches the hotkey without calling into the process at all.

use std::sync::Arc;
use std::thread;

use crate::winapi::*;

/// A registered global hotkey.
///
/// Dropping the handle unregisters the hotkey.
pub struct GlobalHotkey {
    hwnd: HWND,
    id: i32,
    thread: Option<thread::JoinHandle<()>>,
}

impl GlobalHotkey {
    /// Register a global hotkey.
    ///
    /// `on_trigger` is called on the hotkey thread every time the combination
    /// is pressed.
    pub fn register(
        description: &str,
        on_trigger: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Result<Self, String> {
        Self::register_any(&[description.to_string()], on_trigger).map(|h| h.0)
    }

    /// Register the first available hotkey from `descriptions`.
    ///
    /// Returns the hotkey together with the description that was actually
    /// taken. A combination may already be owned by another application —
    /// `Alt + Space` and `Win + Space` are commonly taken — so the launcher
    /// needs somewhere to fall back to.
    pub fn register_any(
        descriptions: &[String],
        on_trigger: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Result<(Self, String), String> {
        let mut last_error = String::from("no hotkey candidates");

        for description in descriptions {
            let Some((modifiers, key)) = parse_hotkey(description) else {
                last_error = format!("cannot parse hotkey '{description}'");
                continue;
            };

            match spawn_hotkey_thread(modifiers, key, on_trigger.clone()) {
                Ok((hwnd, id)) => {
                    return Ok((
                        Self {
                            hwnd,
                            id,
                            thread: None,
                        },
                        description.clone(),
                    ));
                }
                Err(e) => last_error = e,
            }
        }

        Err(last_error)
    }

    /// Unregister the hotkey and stop the thread.
    pub fn unregister(&mut self) {
        unsafe {
            UnregisterHotKey(self.hwnd, self.id);
            PostMessageW(self.hwnd, WM_QUIT, 0, 0);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for GlobalHotkey {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// The window procedure for the message-only hotkey window.
unsafe extern "system" fn hotkey_wnd_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Spawn the hotkey thread and return the message window handle.
fn spawn_hotkey_thread(
    modifiers: u32,
    key: u32,
    on_trigger: Arc<dyn Fn() + Send + Sync + 'static>,
) -> Result<(HWND, i32), String> {
    // The window handle is a raw pointer, which is not `Send`; pass it as an integer.
    let (sender, receiver) = std::sync::mpsc::channel::<(usize, i32)>();

    let thread = thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            unsafe {
                let hinstance = GetModuleHandleW(std::ptr::null());
                let class_name: Vec<u16> = "IdeasLauncherHotkey\0".encode_utf16().collect();

                let wnd_proc: unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT =
                    hotkey_wnd_proc;

                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(wnd_proc),
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

                let hwnd = CreateWindowExW(
                    0,
                    class_name.as_ptr(),
                    std::ptr::null(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    hinstance,
                    std::ptr::null_mut(),
                );
                if hwnd.is_null() {
                    let _ = sender.send((0, 0));
                    return;
                }

                const HOTKEY_ID: i32 = 0xBEEF;
                if RegisterHotKey(hwnd, HOTKEY_ID, modifiers | MOD_NOREPEAT, key) == 0 {
                    // The combination is taken by somebody else. The window
                    // that was made for it is of no use then, and it has to
                    // go — otherwise every combination that is tried and
                    // refused leaves a window behind.
                    DestroyWindow(hwnd);
                    let _ = sender.send((0, 0));
                    return;
                }

                let _ = sender.send((hwnd as usize, HOTKEY_ID));

                let mut msg = MSG {
                    hwnd: std::ptr::null_mut(),
                    message: 0,
                    wParam: 0,
                    lParam: 0,
                    time: 0,
                    pt: POINT { x: 0, y: 0 },
                };
                loop {
                    let got = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                    if got <= 0 {
                        log::error!("hotkey thread: GetMessage returned {got}, giving up");
                        break;
                    }
                    if msg.message == WM_HOTKEY {
                        log::info!("hotkey thread: WM_HOTKEY received");
                        on_trigger();
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                UnregisterHotKey(hwnd, HOTKEY_ID);
                DestroyWindow(hwnd);
            }
        })
        .map_err(|e| e.to_string())?;

    match receiver.recv() {
        Ok((hwnd, id)) if hwnd != 0 => Ok((hwnd as HWND, id)),
        _ => {
            let _ = thread.join();
            Err("failed to register the global hotkey".to_string())
        }
    }
}

/// Wake the Win32 message loop of the launcher window.
///
/// `egui::Context::request_repaint` only sets a flag; it does **not** wake
/// eframe's event loop, and eframe only repaints a window once the repaint
/// time it scheduled has come. Posting a plain message is not enough either,
/// because a message that winit does not translate into an event does not
/// produce a frame.
///
/// `InvalidateRect` does the right thing: it asks Windows to send `WM_PAINT`,
/// winit turns that into `WindowEvent::RedrawRequested`, and eframe runs a
/// frame — which is where the pending query results are picked up.
pub fn wake(hwnd: usize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        // The window is **not** erased: it is drawn from top to bottom in
        // every frame anyway, so erasing the whole client area first is
        // work for nothing — and this happens on every result that comes
        // in, that is, on every keystroke.
        InvalidateRect(hwnd as HWND, std::ptr::null(), 0);
    }
}
/// Show or hide the launcher window.
///
/// Called from the hotkey thread so that the window appears without waiting
/// for the UI thread to wake up.
pub fn set_window_visible(hwnd: HWND, visible: bool) {
    if hwnd.is_null() {
        return;
    }
    unsafe {
        if visible {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        } else {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Bring the window to the front and give it keyboard focus.
///
/// `SetActiveWindow` is deliberately not used: it only works for windows owned
/// by the calling thread, and calling it from the hotkey thread would leave
/// the window without keyboard focus (egui then drops every key event).
///
/// Returns whether the window really got the focus. Windows refuses the
/// request now and then — while a window of the system is asking the user
/// something, for example — and then the launcher has to stay on the screen
/// anyway, or the user presses the hotkey and sees nothing at all.
pub fn force_foreground(hwnd: HWND) -> bool {
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        let ok = SetForegroundWindow(hwnd);
        if ok != 0 {
            log::info!("force_foreground: SetForegroundWindow -> {ok}");
            return true;
        }
        // The request was refused. The way around it is to borrow the input
        // queue of the thread that is in front for a moment.
        let foreground = GetForegroundWindow();
        let mut foreground_pid = 0u32;
        let foreground_thread = GetWindowThreadProcessId(foreground, &mut foreground_pid);
        let own_thread = GetCurrentThreadId();
        if foreground_thread != 0 && foreground_thread != own_thread {
            AttachThreadInput(own_thread, foreground_thread, TRUE);
            BringWindowToTop(hwnd);
            SetForegroundWindow(hwnd);
            AttachThreadInput(own_thread, foreground_thread, FALSE);
        }
        let again = SetForegroundWindow(hwnd);
        log::info!("force_foreground: SetForegroundWindow -> {ok}, retry -> {again}");
        again != 0
    }
}

/// Show the launcher window and give it keyboard focus.
pub fn focus_window(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_keys_are_readable() {
        // The function must be callable without a window.
        let _ = vk::SHIFT;
    }
}
