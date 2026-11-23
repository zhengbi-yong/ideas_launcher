use super::*;

impl LauncherApp {
    /// Open the settings panel.
    ///
    /// This is what the tray menu does, and what the `--settings` command line
    /// switch uses.
    pub fn open_settings(&mut self) {
        self.visible_flag.store(true, Ordering::SeqCst);
        self.foreground_refused = !crate::hotkey::force_foreground(self.hwnd as winapi::HWND);
        self.open_settings_on_show = true;
        self.on_shown();
    }

    /// Open the window on start-up, optionally with a query.
    ///
    /// This is what the `--show` and `--query` command line switches use.
    ///
    /// The window still hides itself when it loses the focus — the switch only
    /// says that the window is open right away. It gets a moment for that:
    /// Windows does not always give the focus to the window at once.
    pub fn open_on_startup(&mut self, query: Option<String>) {
        self.startup_show = true;
        self.startup_query = query;
    }

    /// Keep the window on the screen even when it does not have the focus.
    ///
    /// This is for the tools that take a picture of the launcher: a window
    /// that hides itself the moment the picture is taken is of no use.
    pub fn keep_open(&mut self) {
        self.hide_on_deactivate = false;
    }

    /// Measure the latency of one query after the window has settled.
    ///
    /// The self test runs the query in the steady state — the window is
    /// already open and idle — so the number is not polluted by the start-up
    /// of the window itself.
    pub fn start_selftest(&mut self, query: String) {
        self.selftest = Some(query);
        self.selftest_at = Some(Instant::now() + Duration::from_millis(400));
        // The window is idle by then, so a repaint has to be scheduled for
        // the self test to run at all.
        self.ctx.request_repaint_after(Duration::from_millis(400));
    }

    /// Fit the number of frames to what the screen can actually show.
    ///
    /// The window asks for a new frame every time the pointer moves, and the
    /// pointer moves a hundred times a second — a mouse that reports a
    /// thousand times a second asks for a thousand times that. Whether those
    /// frames can be shown depends on the machine: a healthy one puts a frame
    /// on the screen in three milliseconds, a machine with an overlay and a
    /// remote desktop program running needs forty.
    ///
    /// Asking for more frames than the screen can show is not free: the extra
    /// ones pile up in the compositor and the desktop — and the pointer with
    /// it — falls behind. So the time a frame really takes is measured, and
    /// the mouse is held back until the window asks for no more frames than
    /// the screen can take.
    ///
    /// `egui` draws **two** frames for every mouse message, so the gap
    /// between the frames is half the gap between the messages. The two are
    /// compared: when the frames take longer than the mouse message spacing
    /// asks for, the screen is the one that cannot keep up, and the mouse is
    /// held back further.
    pub(super) fn adapt_frame_rate(&mut self) {
        if self.frame_gaps.len() < 8 {
            return;
        }
        let mut gaps = self.frame_gaps.clone();
        gaps.sort();
        // The slowest tenth of the frames: the stalls are what matters, not
        // the average.
        let slow = gaps[((gaps.len() as f64 - 1.0) * 0.9) as usize];
        let current = crate::mouse::interval_ms();
        let slow_ms = slow.as_millis() as u32;
        // What the mouse message spacing asks for: two frames per message.
        let expected = (current / 2).max(1);
        let mut wanted = current;
        if slow_ms > expected * 3 / 2 {
            // The screen cannot keep up with what is being asked for: ask for
            // less. The gap is what the screen needs, and a little more.
            wanted = (slow_ms + slow_ms / 2).min(100);
        } else if slow_ms * 2 < current {
            // The screen is keeping up easily: ask for what it can take.
            wanted = (current - 4).max(16);
        }
        if wanted != current {
            crate::mouse::set_interval(wanted);
            log::info!(
                "a frame takes {slow_ms} ms, so the mouse is held back to {wanted} ms"
            );
        }
    }

    /// React to the window being shown.
    ///
    /// The OS-level show/hide already happened on the hotkey thread, so this
    /// only refreshes the UI-side state.
    pub(super) fn on_shown(&mut self) {
        self.visible = true;
        // The flag stays set until the window is hidden, so that the startup
        // path — which shows the window a second time — keeps the panel.
        self.view = if self.open_settings_on_show {
            View::Settings
        } else {
            View::Search
        };
        self.shown_at = Some(Instant::now());
        self.ever_focused = false;
        self.focus_lost_at = None;
        self.query_text.clear();
        self.results.clear();
        self.selected = 0;
        // The highlight does not glide in from wherever it was last time.
        self.highlight_row = 0.0;
        self.highlight_vel = 0.0;
        self.tip_row = None;
        self.preview_row = None;
        self.pending_focus = true;
        // The window has just been brought up; it has not had the focus yet.
        self.was_focused = false;
        self.run_query();
        // Keep egui's viewport state in sync with the OS: eframe re-applies
        // `visible` every frame, so a bare `ShowWindow` would be undone.
        self.position_at_cursor();
        self.ctx.request_repaint();
        // The hotkey thread already asked for the foreground; asking again
        // from this thread makes Windows deactivate and reactivate the window,
        // which drops the keyboard focus again.
    }

    /// Open the window because the user asked for it through the tray icon.
    pub(super) fn show_from_tray(&mut self, settings: bool) {
        self.visible_flag.store(true, Ordering::SeqCst);
        self.foreground_refused = !crate::hotkey::force_foreground(self.hwnd);
        self.open_settings_on_show = settings;
        self.on_shown();
        self.ctx.request_repaint();
        crate::hotkey::wake(self.hwnd as usize);
    }

    /// Hide the launcher.
    pub(super) fn hide(&mut self) {
        log::info!("hide() requested");
        self.visible = false;
        // A window nobody sees does not need the wheel.
        crate::mouse::capture_wheel(false);
        crate::mouse::clear_list_hit_area();
        // A video that keeps playing in a window nobody sees is a video
        // that keeps the processor busy.
        if let Some(player) = &self.video {
            player.pause();
        }
        // The next time the window opens, it opens on the search box.
        self.open_settings_on_show = false;
        self.shown_at = None;
        self.visible_flag.store(false, Ordering::SeqCst);
        // Only Win32 is used here. An egui `Visible(false)` command is applied
        // at the end of a frame, so a queued one would hide the window again
        // right after the hotkey had shown it.
        crate::hotkey::set_window_visible(self.hwnd, false);
    }

    /// Place the window in the middle of the monitor that holds the cursor.
    ///
    /// Mirrors Flow Launcher's `SearchWindowScreen = Cursor` /
    /// `SearchWindowAlign = Center` defaults.
    pub(super) fn position_at_cursor(&self) {
        if self.hwnd.is_null() {
            return;
        }
        unsafe {
            let mut cursor = winapi::POINT { x: 0, y: 0 };
            if winapi::GetCursorPos(&mut cursor) == 0 {
                return;
            }
            // MONITOR_DEFAULTTONEAREST
            let monitor = winapi::MonitorFromPoint(cursor, 0x0000_0002);
            let mut info = winapi::MONITORINFO {
                cbSize: std::mem::size_of::<winapi::MONITORINFO>() as u32,
                rcMonitor: winapi::RECT { left: 0, top: 0, right: 0, bottom: 0 },
                rcWork: winapi::RECT { left: 0, top: 0, right: 0, bottom: 0 },
                dwFlags: 0,
            };
            if winapi::GetMonitorInfoW(monitor, &mut info) == 0 {
                return;
            }
            let work_width = info.rcWork.right - info.rcWork.left;
            let work_height = info.rcWork.bottom - info.rcWork.top;
            // The window size has to be read from the window itself: on a
            // scaled display the physical size is larger than the logical
            // size the layout was designed for, and `SetWindowPos` works in
            // physical pixels.
            let mut rect = winapi::RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if winapi::GetWindowRect(self.hwnd, &mut rect) != 0 {
                let width = rect.right - rect.left;
                let height = rect.bottom - rect.top;
                if width > 0 && height > 0 {
                    let x = info.rcWork.left + (work_width - width) / 2;
                    let y = info.rcWork.top + (work_height - height) / 3;
                    // SWP_NOSIZE | SWP_NOZORDER | SWP_SHOWWINDOW
                    winapi::SetWindowPos(
                        self.hwnd,
                        winapi::HWND_TOP,
                        x,
                        y,
                        0,
                        0,
                        0x0001 | 0x0004 | 0x0040,
                    );
                    return;
                }
            }

            let width = metrics::WINDOW_SIZE.x as i32;
            let height = metrics::WINDOW_SIZE.y as i32;
            let x = info.rcWork.left + (work_width - width) / 2;
            let y = info.rcWork.top + (work_height - height) / 3;

            // SWP_NOSIZE | SWP_NOZORDER | SWP_SHOWWINDOW
            winapi::SetWindowPos(
                self.hwnd,
                winapi::HWND_TOP,
                x,
                y,
                0,
                0,
                0x0001 | 0x0004 | 0x0040,
            );
        }
    }
}
