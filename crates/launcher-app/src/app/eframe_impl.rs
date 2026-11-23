use super::*;

impl eframe::App for LauncherApp {
    /// The colour the window is cleared with.
    ///
    /// The window is opaque and the card fills it completely, so the colour
    /// only shows for the frame or two before the first paint. It is the
    /// card's own colour, so the window never flashes.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    /// Runs before every UI pass, and also while the window is hidden when a
    /// repaint has been requested. This is where the global hotkey is handled.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.first_frame {
            self.first_frame = false;
            // Visibility is driven purely through Win32 (`ShowWindow`).
            //
            // eframe's `ViewportCommand::Visible(false)` is deliberately not
            // used: it leaves winit believing the window is hidden, and winit
            // then stops delivering keyboard input to it. The window is hidden
            // on the next line instead.
            let hwnd = self.hwnd as usize;
            let visible = self.visible_flag.clone();
            std::thread::spawn(move || {
                // eframe shows the window once it has painted a frame; put it
                // back afterwards.
                for _ in 0..20 {
                    std::thread::sleep(Duration::from_millis(25));
                    if !visible.load(Ordering::SeqCst) {
                        crate::hotkey::set_window_visible(hwnd as winapi::HWND, false);
                    }
                }
            });
        }
        if self.toggle_requested.swap(false, Ordering::SeqCst) {
            // The hotkey thread already showed or hid the window; all that is
            // left is to bring the UI state in line.
            if self.visible_flag.load(Ordering::SeqCst) {
                if !self.visible {
                    // The hotkey always opens the search box.
                    self.open_settings_on_show = false;
                    self.on_shown();
                }
            } else if self.visible {
                self.hide();
            }
        }

        // `eframe` shows this window after the very first frame whatever the
        // builder said (see the builder in `main` for the whole story). The
        // window is born far off the screen, and while it is not meant to be
        // seen it is hidden here again on every pass — if anything puts it
        // back (a frame of `eframe`, a saved position coming back), it is
        // gone before the eye can catch it.
        if !self.visible && !self.hwnd.is_null() {
            crate::hotkey::set_window_visible(self.hwnd as winapi::HWND, false);
        }

        // Pick up whatever the query worker produced while the UI was busy.
        self.drain_query_results();

        // Tab completes the query with the selected result. It has to be
        // handled here, before the UI pass, because otherwise `egui` moves the
        // focus to the next widget.
        if ctx.input(|i| i.key_pressed(egui::Key::Tab)) {
            ctx.input_mut(|i| {
                let modifiers = i.modifiers;
                i.consume_key(modifiers, egui::Key::Tab);
            });
            self.complete_selected();
        }

        // Ctrl+C copies the selected result. The search box has the focus all
        // the time, so without this the key would copy the query instead.
        if ctx.input(|i| i.key_pressed(egui::Key::C) && i.modifiers.ctrl) {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::C));
            self.copy_selected();
        }

        // Ctrl+R runs the query again.
        if ctx.input(|i| i.key_pressed(egui::Key::R) && i.modifiers.ctrl) {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::R));
            self.run_query();
        }

        // The indexes are built in the background. A query that runs before
        // they are there comes back short — so when one of them shows up, the
        // query runs again. Without this the list would stay empty until the
        // user happened to type another character.
        //
        // The check below only runs when a frame is painted, and a frame is
        // painted when something happens. Nothing happens while the disk is
        // being read — so the window asks for one, until the index is there.
        // Without that, the files would stay missing until the user typed
        // another character.
        // The window is hidden: there is nothing to draw, and a repaint that
        // nobody sees is a repaint that costs.
        if self.visible && self.file_plugin.is_building() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        // The moving part of the progress bar needs a new frame every time it
        // is to move. Without this it stands still, and the bar looks like it
        // is stuck.
        if self.visible && self.file_plugin.is_building() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        // **The walk through the folders of the user is a different thing.**
        // It can take three quarters of a minute, and while it runs the
        // results on the screen do not change — the new files are only
        // found by the **next** search, so that the list the user is
        // looking at is not disturbed.
        //
        // So there is nothing to redraw sixty times a second: the only
        // thing that moves is the progress bar, and it moves slowly. Sixty
        // frames a second of the whole window cost three quarters of a core
        // and made the mouse over the list feel heavy — for a bar that
        // creeps along.
        if self.visible && self.file_plugin.is_scanning() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let program_ready = self.program_plugin.index().is_some();
        let uwp_ready = self.uwp_plugin.len() > 0;
        let file_ready = !self.file_index.is_empty();
        // The full-disk index is the one that matters for the file search, and
        // it arrives on its own — from the cache or from a fresh build. When
        // it shows up, the query has to run again, or the files stay missing
        // until the user types another character.
        let full_ready = self.file_plugin.has_full_index();
        if (program_ready && !self.program_index_ready)
            || (uwp_ready && !self.uwp_index_ready)
            || (file_ready && !self.file_index_ready)
            || (full_ready && !self.full_index_ready)
        {
            self.program_index_ready = program_ready;
            self.uwp_index_ready = uwp_ready;
            self.file_index_ready = file_ready;
            self.full_index_ready = full_ready;
            if !self.query_text.is_empty() {
                log::info!("an index became ready — running the query again");
                self.run_query();
            }
        }

        // The folders of the user are watched, so the index of those folders
        // is never out of date. When a file appears or disappears, the query
        // runs again — otherwise the new file would only be found after the
        // next keystroke.
        //
        // **Not any more.** A query costs the service a tenth of a second,
        // and the machine writes files all the time: a training job, a
        // build, the index of the service itself. Every one of those would
        // have the window ask again, and again, and again — while the user
        // is looking at the results.
        //
        // Nothing is lost by waiting: the watcher keeps the index of the
        // folders up to date (it is **incremental** — one change at a time,
        // never a walk through the folders), and the service follows the
        // change journal of the volumes, which is incremental too. So the
        // **next** search finds everything that is new, and the search that
        // is on the screen is not disturbed in the meantime.
        let live_version = self.file_plugin.relevant_version();
        if live_version != self.live_version_seen {
            self.live_version_seen = live_version;
        }

        // A plugin may ask for a different query.
        let requested = self.host_state.pending_query.lock().take();
        if let Some(query) = requested {
            log::info!("a plugin changed the query to {query:?}");
            self.query_text = query;
            self.run_query();
        }

        // A plugin may ask for a message to be shown.
        let messages: Vec<(String, String)> = {
            let mut guard = self.host_state.messages.lock();
            guard.drain(..).collect()
        };
        for (title, subtitle) in messages {
            log::info!("plugin message: {title} — {subtitle}");
        }

        // The tray icon: show the window, open the settings or quit.
        if let Some(tray) = &self.tray {
            // The icon follows the setting: in the open notification area, or
            // folded away into the overflow panel so that it takes up no room
            // on the task bar.
            tray.set_promoted(self.settings.always_show_tray_icon);
            match tray.take_command() {
                crate::tray::TrayCommand::None => {}
                crate::tray::TrayCommand::Show => self.show_from_tray(false),
                crate::tray::TrayCommand::Settings => self.show_from_tray(true),
                crate::tray::TrayCommand::RunAsAdministrator => {
                    log::info!("starting an elevated copy through the tray icon");
                    if crate::elevate::relaunch_as_admin() {
                        std::process::exit(0);
                    }
                }
                crate::tray::TrayCommand::RebuildFileIndex => {
                    log::info!("the full-disk index was asked to be rebuilt");
                    self.file_plugin.rebuild();
                }
                crate::tray::TrayCommand::Quit => {
                    log::info!("quitting through the tray icon");
                    std::process::exit(0);
                }
            }
        }

        // The settings pane cannot reach the index itself, so it leaves the
        // request there.
        if self.settings_view.take_rebuild_request() {
            log::info!("the full-disk index was asked to be rebuilt from the settings");
            self.file_plugin.rebuild();
        }

        // The index service has to be registered with Windows, and that needs
        // administrative privileges — so an elevated copy does it.
        if self.settings_view.take_install_service_request() {
            log::info!("the index service was asked to be installed from the settings");
            if crate::elevate::relaunch_as_admin_with(&["--install-service"]) {
                std::process::exit(0);
            }
        }

        // The self test runs once the window has settled, so that the number
        // describes the steady state and not the start-up of the window.
        if let Some(at) = self.selftest_at {
            if Instant::now() >= at {
                self.selftest_at = None;
                if let Some(query) = self.selftest.clone() {
                    self.query_text = query;
                    self.selftest_running = true;
                    self.run_query();
                }
            } else {
                // The scheduled repaint may have come a hair too early.
                let remaining = at.saturating_duration_since(Instant::now());
                self.ctx.request_repaint_after(remaining);
            }
        }

        // Open the window on start-up when the command line asked for it.
        if self.startup_show {
            self.startup_show = false;
            let hwnd = self.hwnd;
            // The visibility flag has to agree with the window state, or the
            // first-frame housekeeping hides the window again.
            self.visible_flag.store(true, Ordering::SeqCst);
            crate::hotkey::force_foreground(hwnd);
            self.on_shown();
            if let Some(query) = self.startup_query.take() {
                self.query_text = query;
                self.run_query();
            }
            self.ctx.request_repaint();
            crate::hotkey::wake(self.hwnd as usize);
        }

        // Hide when the window loses the focus — the same behaviour as
        // Flow Launcher's `HideWhenDeactivated`.
        //
        // The window is hidden whenever it does not have the focus. It used
        // to wait for the focus to have been there once, but then a window
        // that never got the focus — Windows does not always give it — would
        // stay on the screen for ever.
        //
        // Whether the window is in front is asked of Windows itself, and not
        // of `egui`: `egui` hears about the focus through the events of the
        // window, and it does not hear of every change. Holding the Alt key
        // is enough to make it believe the window still has the focus, while
        // Windows has already given it away — and then the window stays on
        // the screen for ever.
        let focused = unsafe { winapi::GetForegroundWindow() } == self.hwnd as winapi::HWND;
        // Whose window is in front? The candidate window of the input method
        // often belongs to **this** process, and the window of the system
        // dialog that was just opened does as well. Neither of them means
        // that the user went somewhere else.
        let foreground_pid = unsafe {
            let mut pid = 0u32;
            winapi::GetWindowThreadProcessId(winapi::GetForegroundWindow(), &mut pid);
            pid
        };
        let ours = foreground_pid == std::process::id();
        if focused != self.was_focused {
            log::info!(
                "focus transition: {} -> {} (visible={})",
                self.was_focused,
                focused,
                self.visible
            );
        }
        if focused {
            self.ever_focused = true;
            self.focus_lost_at = None;
            self.foreground_refused = false;
        } else if self.focus_lost_at.is_none() {
            self.focus_lost_at = Some(Instant::now());
        }
        // A window that has just been opened may not be in front yet — that
        // is normal, and it is not a reason to take it down again. Once it has
        // been in front, losing the front means the user went somewhere else,
        // and then it goes away at once.
        let grace = !self.ever_focused
            && self
                .shown_at
                .map(|t| t.elapsed() < Duration::from_secs(3))
                .unwrap_or(false);
        // A dialog of the system — *open with*, *properties*, the question
        // before a file goes into the bin — takes the focus away from the
        // window. The window has to stay a while, or the user would be
        // answering the question to an empty screen.
        //
        // The grace is a few seconds long: long enough for the dialog to be
        // on the screen, short enough that the launcher is not left lying
        // around behind it. The question before a file goes into the bin
        // blocks the thread that asks it, so the window cannot go away while
        // that one is open anyway.
        let dialog_up = self
            .system_dialog_until
            .map(|t| Instant::now() < t)
            .unwrap_or(false);
        // A focus that is gone for a moment and comes straight back is not a
        // departure. The candidate window of the input method takes the focus
        // while the text is committed, and the window must not go away then:
        // the user is in the middle of typing.
        //
        // The wait is two seconds: picking a character out of a list of
        // candidates takes about that long, and the window has to survive it.
        //
        // And a window of **this** process in front is never a departure:
        // the candidate window of the input method and the dialog of the
        // system both belong to us.
        let focus_grace = self
            .focus_lost_at
            .map(|t| t.elapsed() < Duration::from_secs(2))
            .unwrap_or(false);
        if self.visible
            && !focused
            && !ours
            && !grace
            && !dialog_up
            && !focus_grace
            && !self.foreground_refused
            && self.hide_on_deactivate
        {
            log::info!("hide(): the window lost the focus");
            self.hide();
        }
        self.was_focused = focused;

        // A video that has been looked at for a moment gets its player.
        // The window is asked for a frame while one is waiting, so that the
        // player starts on time.
        if self.pending_video.is_some() {
            self.start_pending_video();
            ctx.request_repaint_after(Duration::from_millis(60));
        }

        // The check above only happens when a frame is painted, and a frame
        // is painted when something happens. A thread of its own watches the
        // foreground window and wakes the interface up when it changes, so the
        // window goes away at once and not a quarter of a second later.
        if self.visible {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        if !self.visible {
            ctx.set_cursor_icon(egui::CursorIcon::Default);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if self.view == View::Settings {
            // The settings pages keep the wheel of `egui` — their scroll
            // areas are its, and the smear of a notch costs nothing there.
            crate::mouse::capture_wheel(false);
            crate::mouse::clear_list_hit_area();
            self.settings_view.set_index_status(index_status(&self.file_plugin));
            self.settings_view.show(ui, &mut self.settings, &mut self.palette);
            if self.settings_view.should_close() {
                self.view = View::Search;
                self.settings_view.request_close();
                self.save_settings();
            }
            if let Some(theme) = self.settings_view.take_theme_change() {
                self.apply_theme(&theme);
                self.save_settings();
            }
            // The index of the whole disk needs administrative privileges.
            if self.settings_view.take_elevate_request() {
                log::info!("starting an elevated copy because the index needs it");
                if crate::elevate::relaunch_as_admin() {
                    std::process::exit(0);
                }
            }
            self.ctx.request_repaint();
            return;
        }

        let palette = self.palette.clone();

        // The frame rate. It is written down only when the measurement is
        // asked for — the number is a debugging aid, and a line of log every
        // second is not something a program should write on its own.
        self.frames += 1;
        // How long the last frame took to arrive. It is the measure of how
        // fast the window really gets its frames onto the screen.
        let gap = self.last_frame.elapsed();
        self.last_frame = Instant::now();
        if gap < Duration::from_millis(100) {
            // Only the gaps of a window that is actually drawing are of
            // interest. A window that waits for the pointer to come back
            // draws four times a second, and those gaps say nothing about how
            // fast the screen is.
            self.frame_gaps.push(gap);
            if self.frame_gaps.len() > 240 {
                self.frame_gaps.remove(0);
            }
        }
        let since = self.frames_at.elapsed();
        if since >= Duration::from_secs(1) {
            let fps = self.frames as f64 / since.as_secs_f64();
            if launcher_core::profile::enabled() {
                let (seen, passed) = crate::mouse::counts();
                let mut gaps = self.frame_gaps.clone();
                gaps.sort();
                let median = gaps
                    .get(gaps.len() / 2)
                    .map(|d| d.as_secs_f64() * 1000.0)
                    .unwrap_or(0.0);
                let p90 = gaps
                    .get(((gaps.len() as f64 - 1.0) * 0.9) as usize)
                    .map(|d| d.as_secs_f64() * 1000.0)
                    .unwrap_or(0.0);
                log::info!(
                    "the window drew {fps:.1} frames a second (gaps: median {median:.1} ms, \
                     p90 {p90:.1} ms; mouse messages {seen}, let through {passed}, \
                     interval {} ms; tip {:?}, hover {:?}, selected {}, settled {})",
                    crate::mouse::interval_ms(),
                    self.tip_row,
                    self.tip_hovered,
                    self.selected,
                    self.selected_at.elapsed() >= Duration::from_millis(300)
                );
            }
            // The number of frames is only worth fitting while the pointer is
            // moving: a window that waits for the pointer draws four frames a
            // second, and that says nothing about the screen.
            let (_, passed) = crate::mouse::counts();
            if passed.saturating_sub(self.mouse_passed_before) > 20 {
                self.adapt_frame_rate();
            }
            self.mouse_passed_before = passed;
            self.frames = 0;
            self.frames_at = Instant::now();
        }

        if self.visible && self.trace_frames {
            // Every frame of an interactive session has events, and writing
            // them out costs a format and a write to the disk — sixty times
            // a second. It is a debugging aid, so it is only done when it is
            // asked for.
            let events: Vec<String> = ui
                .ctx()
                .input(|i| i.events.iter().map(|e| format!("{e:?}")).collect());
            if !events.is_empty() {
                log::info!(
                    "frame: focused={} query={:?} events={:?}",
                    ui.ctx().input(|i| i.focused),
                    self.query_text,
                    events,
                );
            }
        }

        // Fade + slide in.
        let (opacity, offset) = match self.shown_at {
            Some(t) => {
                let elapsed = t.elapsed().as_secs_f32();
                let k = (elapsed / 0.14).clamp(0.0, 1.0);
                let eased = 1.0 - (1.0 - k) * (1.0 - k);
                (eased, (1.0 - eased) * 10.0)
            }
            None => (0.0, 10.0),
        };

        // The root `Ui` has no allocated widgets yet, so `min_rect` is empty.
        // `viewport_rect` is in *screen* coordinates, so only its size is used;
        // the painter works in window-local coordinates.
        let size = ui.ctx().viewport_rect().size();
        let rect = Rect::from_min_size(egui::pos2(0.0, offset), size);

        if !self.logged_layout {
            self.logged_layout = true;
            log::info!(
                "layout: viewport_rect={:?} size={size:?} focused={} events={}",
                ui.ctx().viewport_rect(),
                ui.ctx().input(|i| i.focused),
                ui.ctx().input(|i| i.events.len()),
            );
        }

        let mut painter = ui.painter().clone();
        painter.set_clip_rect(rect);

        // The card fills the whole window and is painted with square corners.
        //
        // Windows rounds the corners of every window with a radius of its own
        // — 8 points on Windows 11 — and it cuts the client area along that
        // curve. Painting a card with a radius of its own only puts two
        // different curves next to each other, and the sliver in between is
        // what everybody sees as a dark smudge.
        //
        // So the card is square and fills the window, and the window is told
        // to keep its corners square as well. One shape, one outline, and
        // nothing of the desktop shows at the corners.
        painter.rect_filled(
            rect,
            egui::CornerRadius::ZERO,
            palette.surface.gamma_multiply(opacity),
        );
        painter.rect_stroke(
            rect,
            egui::CornerRadius::ZERO,
            Stroke::new(1.0, palette.border.gamma_multiply(opacity)),
            egui::StrokeKind::Inside,
        );

        let inner = rect.shrink(metrics::PAD);

        // ---- search row -------------------------------------------------
        let search_height = metrics::SEARCH_HEIGHT;
        let search_rect = Rect::from_min_size(inner.min, Vec2::new(inner.width(), search_height));

        // The magnifying glass has to look like it belongs to the text next
        // to it: the letters are about eighteen pixels tall, and an icon that
        // is taller than the text looks out of place. The size follows the
        // text, so that the two stay together when the font is changed.
        let glyph_size = (self.settings.query_box_font_size as f32 * 0.85)
            .min(metrics::SEARCH_GLYPH_SIZE);
        let glyph_pos = egui::pos2(
            search_rect.left() + 2.0,
            search_rect.center().y - glyph_size * 0.5,
        );
        painter.text(
            glyph_pos,
            Align2::LEFT_CENTER,
            "\u{e721}",
            crate::fonts::icon_font_id(glyph_size),
            palette.glyph.gamma_multiply(opacity),
        );

        let text_edit_rect = Rect::from_min_size(
            egui::pos2(search_rect.left() + glyph_size + 14.0, search_rect.top()),
            Vec2::new(search_rect.width() - glyph_size - 14.0, search_height),
        );
        let text_edit = ui.put(
            text_edit_rect,
            TextEdit::singleline(&mut self.query_text)
                .hint_text("搜索程序、文件、网页…")
                .font(FontId::proportional(self.settings.query_box_font_size as f32))
                .text_color(palette.text)
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY)
                .align(Align2::LEFT_CENTER),
        );

        // Remember the input id so the box can take focus the moment the
        // window appears — without it every keystroke would be swallowed by
        // whatever else has focus.
        self.search_id = Some(text_edit.id);
        if self.visible {
            let id = self.search_id.expect("just set");
            // Only ask for the focus when it is not already there.
            //
            // `egui::Memory::request_focus` interrupts the input method, so
            // calling it on every frame would tear down and rebuild the IME
            // sixty times a second — and Chinese, Japanese or Korean input
            // would never get past the first syllable.
            if !ui.memory(|m| m.has_focus(id)) {
                ui.memory_mut(|m| m.request_focus(id));
            }
        }

        if text_edit.changed() {
            self.run_query();
        }

        // The input method is only enabled while a text field has the focus:
        // `egui` asks for it in its platform output, and the windowing layer
        // then associates the IME with the window. Without it, a Chinese,
        // Japanese or Korean keyboard composes nothing.
        let ime_wanted = self.ctx.output(|o| o.ime.is_some());
        if self.ime_reported != Some(ime_wanted) {
            self.ime_reported = Some(ime_wanted);
            log::info!("the search box asks for the input method: {ime_wanted}");
        }

        // ---- results ----------------------------------------------------
        // A hairline under the search box separates what is typed from what
        // is found, so the two never blur into one block.
        painter.line_segment(
            [
                egui::pos2(inner.left(), search_rect.bottom()),
                egui::pos2(inner.right(), search_rect.bottom()),
            ],
            Stroke::new(1.0, palette.border.gamma_multiply(opacity * 0.7)),
        );

        let list_top = search_rect.bottom() + metrics::GAP;
        let footer_height = metrics::FOOTER_HEIGHT;
        let list_height = (inner.bottom() - footer_height - 6.0 - list_top).max(0.0);
        // The scroll has to know how tall the list is, or it scrolls a list
        // that is much taller than the one it thinks it has.
        self.list_height = list_height;

        // The rows that are about to be drawn. They are taken out of the
        // state first, because drawing them needs the icon cache as well.
        let results = std::mem::take(&mut self.results);
        let selected = self.selected;
        // How long the pointer has been resting on this row.
        //
        // The details of a file are read from the disk, and that is only
        // worth doing for a row the user is actually looking at — not for
        // every row the pointer happens to cross on its way somewhere else.
        if self.selected != self.selected_seen {
            self.selected_seen = self.selected;
            self.selected_at = Instant::now();
        }
        let settled = self.selected_at.elapsed() >= Duration::from_millis(300);
        // The preview follows the row the pointer **rests** on. While the
        // pointer sweeps, the pane keeps showing what it was showing: a
        // swept-past row is not being looked at, and re-laying the pane out
        // for every row of a sweep — new text, new glyphs — is exactly the
        // kind of burst that makes the sweep itself stumble.
        if settled {
            self.preview_row = Some(selected);
        }

        // The preview pane takes the right-hand side — but only when the
        // selected row actually has a preview. Reserving the space for an
        // empty pane would squeeze the list into the left half of the window
        // and leave a hole on the right.
        //
        // **Once the pane is there, it stays.** The pointer chooses the row
        // that is looked at, and the rows are of different kinds: a program
        // has no preview and a file has one. If the pane followed the row
        // under the pointer, the width of the list would change with every
        // row the pointer crossed — the paths would be cut off and brought
        // back, the whole list would jump sideways, and the text would be
        // laid out again every time. It is decided again with the query.
        let preview_content = self
            .preview_row
            .filter(|row| *row < results.len())
            .and_then(|row| results.get(row))
            .and_then(preview::derive);
        let room_for_preview =
            self.settings.always_preview || inner.width() >= preview::PREVIEW_MIN_WINDOW_WIDTH;
        if preview_content.is_some() && room_for_preview {
            self.preview_pane = true;
        }
        let show_preview = self.preview_pane && room_for_preview;
        let preview_width = if show_preview { preview::PREVIEW_WIDTH } else { 0.0 };
        let gap = if preview_width > 0.0 { metrics::GAP } else { 0.0 };
        // The scroll bar of the list gets a place of its own, so that it can
        // be seen and dragged.
        let content_height = list_content_height(results.len());
        let needs_scroll = content_height > list_height;
        let scrollbar_width = if needs_scroll { metrics::SCROLLBAR_WIDTH } else { 0.0 };
        let list_width = (inner.width() - preview_width - gap - scrollbar_width).max(160.0);
        let list_rect = Rect::from_min_size(
            egui::pos2(inner.left(), list_top),
            Vec2::new(list_width, list_height),
        );
        let preview_rect = if preview_width > 0.0 {
            Some(Rect::from_min_size(
                egui::pos2(inner.left() + list_width + gap, list_top),
                Vec2::new(preview_width, list_height),
            ))
        } else {
            None
        };

        // Tell the mouse throttle where the rows are, in the physical pixels
        // of the client area: a move that stays on the row of the last one
        // delivered changes nothing on the screen, and the throttle drops it
        // before `egui` can turn it into frames (`mouse::set_list_hit_area`
        // says why that matters on a machine whose frames are expensive).
        //
        // While a popup is open — a context menu, typically — the dropping
        // is switched off: the menu lies **over** the list, and a pointer
        // that walks its entries may well stay on "the same list row"
        // underneath. Dropping the move would leave the menu highlighting
        // the entry it was on instead of the one under the pointer.
        crate::mouse::set_row_drop(!ui.ctx().any_popup_open());
        // The row the interface has settled on: moves on it may be dropped,
        // moves on any other row reach the interface until it decides.
        crate::mouse::set_selected_row(selected);
        {
            let ppp = ui.ctx().pixels_per_point();
            crate::mouse::set_list_hit_area(
                (list_rect.left() * ppp) as i32,
                (list_rect.top() * ppp) as i32,
                (list_rect.right() * ppp) as i32,
                (list_rect.bottom() * ppp) as i32,
                row_pitch() * ppp,
            );
        }

        // The list can only be scrolled as far as it is long. Without this the
        // list keeps the place of an earlier, longer list and starts in the
        // middle.
        let content = list_content_height(results.len());
        let max_offset = (content - list_height).max(0.0);
        self.scroll_offset = self.scroll_offset.clamp(0.0, max_offset);

        // ---- the wheel of the mouse ----------------------------------------
        //
        // The wheel messages of the window are answered by the throttle on
        // the window procedure (`mouse::capture_wheel` — see there for why
        // `egui` must not hear of them), and the notches are picked up here.
        // A notch moves the list by whole rows, **at once**: no smear, and
        // one frame per pump however many notches arrived — Windows coalesces
        // `WM_PAINT` on its own.
        crate::mouse::capture_wheel(true);
        let wheel = crate::mouse::take_wheel();
        if wheel != 0 {
            // A notch away from the user (positive) scrolls up, towards the
            // top — the way every list of Windows works. A notch is **three
            // rows**, the measure every list of Windows uses: one row a
            // notch made people turn the wheel fast just to get anywhere,
            // and a fast wheel is many frames a second — the very thing
            // this window is careful with.
            let notches = wheel as f32 / crate::mouse::WHEEL_DELTA as f32;
            let wanted =
                (self.scroll_offset - notches * 3.0 * row_pitch()).clamp(0.0, max_offset);
            if (wanted - self.scroll_offset).abs() > 0.01 {
                self.scroll_offset = wanted;
                self.pending_scroll = Some(wanted);

                // The wheel is a user input about **where**, like a move of
                // the pointer: the list has just moved under the pointer,
                // and the row under the pointer is the natural selection —
                // it is visible by definition, so the highlight can never
                // scroll out of sight. Without this the selection kept its
                // row while the list ran away from under it, and a fast
                // wheel carried the highlight clean out of the list — it
                // was "lost".
                //
                // The follow happens **in the same frame**, from the
                // arithmetic of the new offset: the highlight and the
                // content move together, and on the screen the highlight
                // simply stays where the pointer is — which is exactly what
                // the eye expects. (Deferring it by a frame made the
                // highlight ride up with the old content and dive back to
                // the pointer afterwards — a visible round trip.)
                //
                // When the pointer is not on the list, the selection is
                // clamped into the visible range instead: the highlight
                // stays on the screen whatever the wheel does.
                //
                // This does not reopen the fight with the keyboard: the
                // keyboard moves the selection without a wheel event and
                // without the pointer moving, and a resting pointer holds
                // its peace.
                let (base, base_top, pitch, scroll_then) = self.row_geometry;
                let pointer = ui.input(|i| i.pointer.latest_pos());
                let under = pointer.and_then(|pos| {
                    let column = pos.x >= list_rect.left() && pos.x < list_rect.right();
                    if !column || pitch <= 0.0 {
                        return None;
                    }
                    let y = pos.y - base_top + (wanted - scroll_then);
                    let row = base as i64 + (y / pitch).floor() as i64;
                    if row >= 0 && (row as usize) < results.len() {
                        Some(row as usize)
                    } else {
                        None
                    }
                });
                let rows_visible = ((list_rect.bottom() - base_top + (wanted - scroll_then))
                    / pitch)
                    .floor() as usize;
                let visible_to = (base + rows_visible).min(results.len().saturating_sub(1));
                let target = under
                    .or(Some(self.selected))
                    .map(|row| row.clamp(base, visible_to))
                    .unwrap_or(self.selected);
                if target != self.selected {
                    let gap = self.row_changed_at.elapsed();
                    self.sweeping = gap < Duration::from_millis(150) || self.sweeping;
                    self.row_changed_at = Instant::now();
                    self.selected = target;
                    // The wheel moved the content in one step; the highlight
                    // rides *with* it, at once — a glide here would leave it
                    // dangling between two places while the list has
                    // already jumped.
                    self.highlight_row = target as f32;
                    self.highlight_vel = 0.0;
                }
            }
        }

        // ---- the highlight of the selected row ------------------------------
        //
        // The highlight is one rectangle of its own, at a **fractional** row
        // position: when the selection moves to another row, the rectangle
        // glides there — a spring with mass and momentum, like the focus of
        // a game console — instead of teleporting. The velocity is kept
        // across rows, so a sweep down the list builds up speed and the
        // highlight follows like something that was pushed.
        //
        // The frames of the glide are **paced** (`request_repaint_after`,
        // never the bare `request_repaint` that asks for as many frames as
        // the machine can make): sixteen milliseconds where the machine can
        // show them, and no more than thirty a second where it cannot —
        // the same budget the mouse itself is held to.
        let target = selected as f32;
        if (self.highlight_row - target).abs() > 0.001 || self.highlight_vel.abs() > 0.05 {
            // A spring with a hint of give — and **adaptive stiffness** on
            // two axes. ζ = 0.85 throughout.
            //
            // *Distance:* ω grows with how far behind the highlight is; a
            // row away it is 25 s⁻¹ (the silky glide with its barely-there
            // overshoot), further behind it stiffens fast.
            //
            // *Momentum:* `sweeping` is set when changes of the row come
            // **close together** — a sweep of the pointer, a run of wheel
            // notches. At sweep speed a soft spring trails behind
            // continuously, which reads as the highlight sitting on the
            // wrong row; so while the momentum lasts, the spring is at
            // least 150 s⁻¹. A **solitary** change — one crossing, one
            // key press — is not a sweep: it glides at the soft rate,
            // because that is the move the eye wants to see travel.
            //
            // A jump of more than six rows — a page key, a big coalesced
            // sweep — lands at once: over that distance a glide is a lag.
            let distance = (target - self.highlight_row).abs();
            if distance > 6.0 {
                self.highlight_row = target;
                self.highlight_vel = 0.0;
            }
            let mut omega = 25.0 + 55.0 * (distance - 1.0).clamp(0.0, 3.0);
            if self.sweeping {
                omega = omega.max(150.0);
            }
            let stiffness = omega * omega;
            let damping = 2.0 * 0.85 * omega;
            // The step shrinks as the spring stiffens. A stiff spring
            // integrated in steps that are too long does not oscillate —
            // it **diverges**: the damping term flips the velocity faster
            // than the step can resolve, each step throws the position
            // further out, and the highlight leaves the list for good
            // (it is clipped away, which looks like it vanished). With
            // `h · ω ≤ 0.5` every stiffness this spring can take is
            // comfortably inside the stable range of the integrator.
            let max_step = (0.5 / omega).min(0.008);
            let mut left = ui.ctx().input(|i| i.stable_dt).min(0.1);
            while left > 0.0 {
                let step = left.min(max_step);
                left -= step;
                let pull = stiffness * (target - self.highlight_row);
                self.highlight_vel += (pull - damping * self.highlight_vel) * step;
                self.highlight_row += self.highlight_vel * step;
            }
            // Insurance against arithmetic surprises: the highlight never
            // leaves the neighbourhood of its target. If it ever tried,
            // the distance itself would stiffen the spring, and a stiff
            // spring gone wrong is the divergence above — this makes the
            // worst case a quick glide back instead of a disappearance.
            let leash = 15.0;
            if (self.highlight_row - target).abs() > leash {
                self.highlight_row =
                    target + (self.highlight_row - target).signum() * leash;
                self.highlight_vel = 0.0;
                log::warn!("the highlight was pulled back to its target");
            }
            if self.highlight_vel.abs() > 200.0 {
                self.highlight_vel = 0.0;
            }
            if (self.highlight_row - target).abs() < 0.01 && self.highlight_vel.abs() < 2.0 {
                self.highlight_row = target;
                self.highlight_vel = 0.0;
            } else {
                let pace = u64::from(crate::mouse::interval_ms().clamp(16, 33));
                self.ctx.request_repaint_after(Duration::from_millis(pace));
            }
        }

        // The glide is drawn under the rows: the rectangle is painted first,
        // and the text of the rows (painted after, in the scroll area)
        // lands on top of it. Its position comes from the geometry of the
        // rows as they were **actually laid out** (the remembered
        // `row_geometry`), adjusted for how far the list has scrolled since
        // — arithmetic about the layout is only a guess, and a highlight
        // half a row off reads as "the highlight is wrong".
        if !results.is_empty() {
            let mut highlight_painter = painter.clone();
            highlight_painter.set_clip_rect(painter.clip_rect().intersect(list_rect));
            let (base, base_top, pitch, scroll_then) = self.row_geometry;
            let top =
                base_top + (self.highlight_row - base as f32) * pitch + (self.scroll_offset
                    - scroll_then);
            let slot = Rect::from_min_size(
                egui::pos2(list_rect.left(), top),
                Vec2::new(list_rect.width(), metrics::ROW_HEIGHT),
            );
            let row_rect = slot.shrink2(Vec2::new(0.0, metrics::ROW_GAP * 0.5));
            highlight_painter.rect_filled(
                row_rect,
                CornerRadius::same(metrics::ROW_RADIUS as u8),
                palette.accent_soft.gamma_multiply(opacity),
            );
            // Accent bar on the left.
            let bar = Rect::from_min_size(
                egui::pos2(row_rect.left() + 3.0, row_rect.center().y - 9.0),
                Vec2::new(2.5, 18.0),
            );
            highlight_painter.rect_filled(
                bar,
                CornerRadius::same(2),
                palette.accent.gamma_multiply(opacity),
            );
        }

        let mut list_ui = ui.new_child(
            egui::UiBuilder::new().max_rect(list_rect).layout(egui::Layout::top_down(egui::Align::Min)),
        );
        // The gap between two rows is set by hand. `egui` would use its own
        // default, and then the window would be a little shorter than the
        // rows it has to show — the last row would be cut in half.
        list_ui.spacing_mut().item_spacing.y = metrics::ROW_GAP;
        list_ui.shrink_clip_rect(list_rect);

        let mut clicked: Option<usize> = None;
        // The row the pointer is on, if it is on one. It is decided after
        // the list has been drawn — see "the row under the pointer".
        // The first and the last row that were laid out, with their real
        // tops: the hit area of the mouse throttle is published from them,
        // from the layout as it actually is — not from arithmetic about it.
        let mut first_row: Option<(usize, f32)> = None;
        let mut last_row: Option<(usize, f32)> = None;
        // What the menu of a row was asked to do.
        let mut menu_action: Option<(usize, RowAction)> = None;
        // The fonts of the rows, and the height they take. They are asked of
        // the font collection **once for the whole list**: the collection is
        // written to whenever a new glyph shows up, and the write lock is
        // the one thing the drawing must not wait for.
        let title_font = FontId::proportional(self.settings.result_item_font_size as f32);
        let subtitle_font = FontId::proportional(self.settings.result_sub_item_font_size as f32);
        let title_height = self.ctx.fonts_mut(|f| f.row_height(&title_font));
        let subtitle_height = self.ctx.fonts_mut(|f| f.row_height(&subtitle_font));
        // The frame of the menu of a row is the same for every row, so it is
        // made once for the whole list and not once per row and frame.
        let menu_frame = menu_frame(&self.palette, &self.ctx);
        if results.is_empty() {
            list_ui.add_space(28.0);
            list_ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(if self.query_text.is_empty() {
                        "输入关键字以搜索"
                    } else {
                        "没有找到结果"
                    })
                    .color(palette.text_faint)
                    .size(metrics::TEXT_BODY),
                );
            });
        } else {
            list_ui.vertical(|ui| {
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("result-list")
                    .auto_shrink([false, false])
                    .max_height(list_rect.height())
                    // The scroll bar of the launcher is drawn by the launcher
                    // itself — see below.
                    .scroll_bar_visibility(egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden);
                // The offset is only written when the app asks for a jump.
                // Writing it every frame would throw away what the user did
                // with the wheel or with the scroll bar — the list would
                // refuse to move.
                if let Some(target) = self.pending_scroll.take() {
                    // A jump is not animated: the list has to be **there**
                    // now and not drift towards it. The animation of `egui`
                    // would be started over again with every frame of a drag,
                    // and the list would not move at all. The wheel still
                    // glides.
                    area = area.animated(false).vertical_scroll_offset(target);
                }
                let out = area.show_rows(
                        ui,
                        metrics::ROW_HEIGHT,
                        results.len(),
                        |ui, range: std::ops::Range<usize>| {
                        for index in range {
                            let result = &results[index];
                            let _is_selected = index == selected;

                            // `allocate_exact_size` advances the layout cursor,
                            // which is what keeps the rows from stacking on top
                            // of each other. The response is of no use here:
                            // the row gets its own, stable id further down —
                            // the automatic one changes every frame, and a
                            // menu that changes its id every frame never
                            // stays open.
                            let (row_rect, _) = ui.allocate_exact_size(
                                Vec2::new(list_rect.width(), metrics::ROW_HEIGHT),
                                Sense::hover(),
                            );
                            if first_row.is_none() {
                                first_row = Some((index, row_rect.top()));
                            }
                            last_row = Some((index, row_rect.top()));

                            // The hit rectangle of the row: the whole slot —
                            // the row plus half the gap on either side, so
                            // that no strip of the list belongs to nobody.
                            // It is **clamped to the list**: the first and
                            // the last row are cut by its edge, and a hit
                            // rectangle reaching past it would let a pointer
                            // on the footer hover the row while the
                            // pointer-in-list guard forbids the selection
                            // from following — hovered but never selected,
                            // the highlight stuck on an old row. That was
                            // the mismatch the log caught at the bottom edge.
                            let mut hit =
                                row_rect.expand2(Vec2::new(0.0, metrics::ROW_GAP * 0.5));
                            if hit.min.y < list_rect.min.y {
                                hit.min.y = list_rect.min.y;
                            }
                            if hit.max.y > list_rect.max.y {
                                hit.max.y = list_rect.max.y;
                            }
                            let id = ui.make_persistent_id(("result-row", index));
                            let response = ui.interact(hit, id, Sense::click());

                            // The hover of the row is NOT decided here — it
                            // is decided once for the whole list, after the
                            // rows, from the geometry of the rows as they
                            // lie: event-driven, with hysteresis, and
                            // without scrolling. See "the row under the
                            // pointer" below the list.

                            let response = draw_row(
                                ui,
                                row_rect,
                                result,
                                &palette,
                                &mut self.icons,
                                &self.byte_icons,
                                &self.ctx,
                                opacity,
                                self.settings.result_item_font_size as f32,
                                self.settings.result_sub_item_font_size as f32,
                                title_height,
                                subtitle_height,
                                index,
                                response,
                            );

                            // A click with the left button opens the row. The
                            // right button opens the menu, and it must not
                            // open the row as well — `egui` counts a click of
                            // the second button as a click too.
                            let second_button = ui.ctx().input(|i| i.pointer.secondary_clicked());
                            if response.clicked() && !second_button {
                                log::info!("the row {index} was clicked: {:?}", result.title);
                                clicked = Some(index);
                            }

                            // ---- the menu of the row ----------------------------
                            //
                            // The right mouse button opens the menu of the row.
                            // A row that is a file on the disk gets the menu
                            // of the Windows shell — the very same one the
                            // file manager shows, with *open with*, *rename*
                            // and *delete* and whatever else is installed on
                            // the machine. Everything else gets the menu of
                            // the launcher.
                            //
                            // The path of the row is only needed **inside** the
                            // menu, and the menu is closed almost always. So
                            // the path is not put together here — that was a
                            // copy of a whole path for every row, every
                            // frame.
                            let mut pick: Option<RowAction> = None;
                            egui::Popup::context_menu(&response)
                                .id(ui.make_persistent_id(("result-menu", index)))
                                .frame(menu_frame.clone())
                                .show(|ui| {
                                    let path = result_path(result);
                                    if menu_item(ui, &self.palette, "打开").clicked() {
                                        pick = Some(RowAction::Open);
                                        ui.close();
                                    }
                                    if path.is_some() {
                                        if menu_item(ui, &self.palette, "打开方式…").clicked() {
                                            pick = Some(RowAction::OpenWith);
                                            ui.close();
                                        }
                                        menu_separator(ui, &self.palette);
                                        if menu_item(ui, &self.palette, "在文件夹中显示").clicked() {
                                            pick = Some(RowAction::OpenFolder);
                                            ui.close();
                                        }
                                        if menu_item(ui, &self.palette, "复制路径").clicked() {
                                            pick = Some(RowAction::CopyPath);
                                            ui.close();
                                        }
                                        if menu_item(ui, &self.palette, "复制名称").clicked() {
                                            pick = Some(RowAction::CopyName);
                                            ui.close();
                                        }
                                        menu_separator(ui, &self.palette);
                                        if menu_item(ui, &self.palette, "重命名…").clicked() {
                                            pick = Some(RowAction::Rename);
                                            ui.close();
                                        }
                                        if menu_item(ui, &self.palette, "删除到回收站").clicked() {
                                            pick = Some(RowAction::Delete);
                                            ui.close();
                                        }
                                        if menu_item(ui, &self.palette, "属性").clicked() {
                                            pick = Some(RowAction::Properties);
                                            ui.close();
                                        }
                                    }
                                });
                            if let Some(action) = pick {
                                menu_action = Some((index, action));
                            }
                        }
                        },
                    );
                // What the user did with the wheel or with the scroll bar is
                // read back here, so the app knows where the list stands.
                self.scroll_offset = out.state.clamped_offset().y;
                // The row the pointer is on was taken from the responses of
                // the rows themselves (in the loop above) — the hit test is
                // the authority, and it now covers the gaps between the rows
                // too. No arithmetic about the layout any more: what the hit
                // test says and what the user sees are the same thing.
                // The hit area of the throttle, published from the rows as
                // they were **actually laid out**: the arithmetic that runs
                // before the list is a good guess, this one is the truth —
                // and a wrong row in the throttle is a move dropped that
                // should have been delivered, which shows up as the
                // highlight sitting on a row the pointer is not on.
                if let (Some((first, first_top)), Some((last, last_top))) = (first_row, last_row)
                {
                    if last > first {
                        let ppp = ui.ctx().pixels_per_point();
                        let pitch = (last_top - first_top) / (last - first) as f32;
                        crate::mouse::set_list_hit_area(
                            (list_rect.left() * ppp) as i32,
                            (first_top * ppp) as i32,
                            (list_rect.right() * ppp) as i32,
                            ((first_top + (last + 1 - first) as f32 * pitch) * ppp) as i32,
                            pitch * ppp,
                        );
                        // Remembered for the highlight and the tip of the
                        // next frame: they are drawn before the rows, from
                        // the geometry of the rows as they last were.
                        self.row_geometry =
                            (first, first_top, pitch, self.scroll_offset);
                    }
                }
            });
        }

        // ---- the row under the pointer -----------------------------------
        //
        // One determination for the whole list, on three principles.
        //
        // **Event-driven.** The hover is re-evaluated only when the pointer
        // has *moved* — a resting pointer holds its peace. A hover that is
        // re-decided on every frame fights everything else that moves the
        // list: the keyboard moves the selection, the selection scrolls the
        // list, the list moves the row under the resting pointer, and the
        // hover drags the selection back — a loop that shows up as the
        // highlight bouncing up and down while the mouse lies still.
        //
        // **Hysteresis.** A high-resolution mouse at rest still reports a
        // count of movement now and then; a pointer resting exactly on the
        // line between two rows flips between them with every count, and a
        // stiff spring turns that into a visible flicker. Leaving a row for
        // its neighbour therefore asks the pointer to be *clearly inside*
        // the neighbour — a few points past the shared edge. Staying on the
        // row it is on asks nothing.
        //
        // **No scrolling.** The row under the pointer is visible by
        // definition — the pointer is on it. Following the pointer never
        // scrolls the list; `sync_scroll` belongs to the keyboard, which
        // has to bring its row into view.
        let pointer = ui.input(|i| i.pointer.latest_pos());
        let hovered_row: Option<usize>;
        let pointer_moved = pointer != self.pointer_seen;
        if pointer_moved {
            self.pointer_moved_at = Instant::now();
            self.pointer_seen = pointer;
        }
        let (base, base_top, pitch, scroll_then) = self.row_geometry;

        // (The wheel carries out its follow in its own frame — see the
        // wheel handling above.)

        let pointer_row = pointer.and_then(|pos| {
            let column = pos.x >= list_rect.left() && pos.x < list_rect.right();
            if !column || pitch <= 0.0 {
                return None;
            }
            // The geometry is the one of the last frame; the scroll of this
            // frame shifts it by exactly the difference.
            let y = pos.y - base_top + (self.scroll_offset - scroll_then);
            let row = base as i64 + (y / pitch).floor() as i64;
            if row >= 0 && (row as usize) < results.len() {
                Some((row as usize, y - (row as f32 - base as f32) * pitch))
            } else {
                None
            }
        });
        hovered_row = match pointer_row {
            // Staying on the selected row: no margin asked.
            Some((row, _)) if row == self.selected => Some(row),
            // Leaving for a neighbour: the pointer must be clearly inside —
            // **when it has come to rest.** The margin is there for the
            // jitter of an idle high-resolution mouse; a pointer that is
            // *moving* is not jittering, and asking it for four points of
            // depth holds the highlight half a row behind the pointer —
            // which reads as "the highlight does not quite follow". While
            // the pointer is on its way, every crossing counts at once.
            Some((row, within)) => {
                let settled = self.pointer_moved_at.elapsed() >= Duration::from_millis(80);
                if !settled || (within >= 4.0 && within <= pitch - 4.0) {
                    Some(row)
                } else {
                    Some(self.selected)
                }
            }
            // Outside the list: no row under the pointer.
            None => None,
        };
        if pointer_moved
            && let Some(row) = hovered_row
            && row != self.selected
            && !ui.input(|i| i.pointer.secondary_down())
            && !ui.ctx().any_popup_open()
        {
            // A change that follows another one closely carries momentum
            // (the pointer is sweeping); a change that stands alone is a
            // single crossing, and the highlight glides to it softly.
            let gap = self.row_changed_at.elapsed();
            self.sweeping = gap < Duration::from_millis(150);
            self.row_changed_at = Instant::now();
            self.selected = row;
        }

        self.tip_hovered = hovered_row;
        // A mismatch between the row under the pointer and the selected row
        // is the glide doing its job — for a moment. One that **stays** is
        // a bug, and it says so: with what the pointer rests on, what is
        // selected, and how long it has been, there is enough in the log to
        // find whoever caused it.
        //
        // The check runs only while the pointer has moved recently: with a
        // resting pointer the keyboard and the wheel may carry the selection
        // away from under it — that is what they are for, not a bug.
        if let Some(row) = hovered_row {
            if row != selected
                && row < results.len()
                && self.pointer_moved_at.elapsed() <= Duration::from_millis(600)
            {
                match self.mismatch_since {
                    Some((seen, _at, true)) if seen == row => {}
                    Some((seen, at, false)) if seen == row => {
                        if at.elapsed() >= Duration::from_millis(120) {
                            let (pos, in_list, menu) = self.ctx.input(|i| {
                                (
                                    i.pointer.latest_pos(),
                                    i.pointer
                                        .latest_pos()
                                        .is_some_and(|p| list_rect.contains(p)),
                                    i.pointer.secondary_down(),
                                )
                            });
                            log::warn!(
                                "highlight mismatch: pointer rests on row {row}, \
                                 selected is {selected} for {} ms \
                                 (pos {pos:?}, in_list {in_list}, menu {menu}, \
                                  scroll {:.1}, geom {:?})",
                                at.elapsed().as_millis(),
                                self.scroll_offset,
                                self.row_geometry,
                            );
                            // said once per episode, not once per frame
                            self.mismatch_since = Some((row, at, true));
                        }
                    }
                    _ => {
                        self.mismatch_since = Some((row, Instant::now(), false));
                    }
                }
            } else {
                self.mismatch_since = None;
            }
        } else {
            self.mismatch_since = None;
        }

        // ---- the scroll bar of the list ---------------------------------
        //
        // The bar of `egui` is not drawn in this layout — the list is drawn
        // by hand, and the bar of the system ends up under the preview. So
        // the launcher draws its own: it can be seen, and it can be dragged.
        if needs_scroll {
            let bar_rect = Rect::from_min_size(
                egui::pos2(
                    list_rect.right() + (metrics::SCROLLBAR_WIDTH - 6.0) * 0.5,
                    list_rect.top(),
                ),
                egui::vec2(6.0, list_rect.height()),
            );
            // The handle is as long as the part of the list that is on screen.
            let visible = (list_rect.height() / content_height).clamp(0.08, 1.0);
            let handle_height = (list_rect.height() * visible).max(28.0);
            let max_offset = (content_height - list_height).max(0.0);
            let ratio = if max_offset > 0.0 {
                (self.scroll_offset / max_offset).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let handle_top = list_rect.top() + ratio * (list_rect.height() - handle_height);
            let handle_rect = Rect::from_min_size(
                egui::pos2(bar_rect.left(), handle_top),
                egui::vec2(bar_rect.width(), handle_height),
            );

            painter.rect_filled(
                bar_rect,
                CornerRadius::same(3),
                palette.border.gamma_multiply(opacity * 0.35),
            );
            painter.rect_filled(
                handle_rect,
                CornerRadius::same(3),
                palette.text_faint.gamma_multiply(opacity * 0.65),
            );

            // Dragging the handle moves the list.
            //
            // The bar that is drawn is six pixels wide, and a six-pixel
            // target is hard to catch — so the whole strip is the target, and
            // the handle follows the pointer instead of the list moving by
            // the same number of pixels (which would make the handle run
            // away from the pointer as soon as the list is longer than the
            // window).
            let grab = Rect::from_min_size(
                egui::pos2(list_rect.right(), list_rect.top()),
                egui::vec2(metrics::SCROLLBAR_WIDTH, list_rect.height()),
            );
            let response = ui.interact(
                grab,
                ui.make_persistent_id("result-scrollbar"),
                Sense::click_and_drag(),
            );
            // How far the handle can travel.
            let travel = (list_rect.height() - handle_height).max(1.0);
            if response.dragged() {
                let delta = response.drag_delta().y / travel * max_offset;
                let wanted = (self.scroll_offset + delta).clamp(0.0, max_offset);
                self.scroll_offset = wanted;
                self.pending_scroll = Some(wanted);
            } else if response.clicked() {
                // A click on the strip puts the handle where the pointer is.
                if let Some(position) = response.hover_pos() {
                    let ratio = ((position.y - handle_height * 0.5) - list_rect.top()) / travel;
                    let wanted = (ratio * max_offset).clamp(0.0, max_offset);
                    self.scroll_offset = wanted;
                    self.pending_scroll = Some(wanted);
                }
            }
        }

        // The results have to go back into the app before anything is opened:
        // `open_selected` looks the row up in `self.results`, and the list is
        // empty while it is being drawn.
        self.results = results;

        if let Some(index) = clicked {
            self.selected = index;
            self.open_selected();
        }

        if let Some((index, action)) = menu_action {
            self.selected = index;
            let result = self.results.get(index).cloned();
            let hwnd = self.hwnd as winapi::HWND;
            match (action, result) {
                (RowAction::Open, Some(result)) => self.open_result(&result),
                (RowAction::OpenFolder, Some(result)) => {
                    if let Some(path) = result_path(&result) {
                        open_folder_of(&path);
                    }
                }
                (RowAction::OpenWith, Some(result)) => {
                    if let Some(path) = result_path(&result) {
                        self.system_dialog_opened();
                        crate::shellmenu::open_with(hwnd, &path);
                        // The dialog of the system is a window of its own, and
                        // the launcher is always on top — it would lie over the
                        // dialog. So the launcher goes away and lets the user
                        // do their work.
                        self.hide();
                    }
                }
                (RowAction::CopyPath, Some(result)) => {
                    if let Some(path) = result_path(&result) {
                        self.ctx.copy_text(path);
                    }
                }
                (RowAction::CopyName, Some(result)) => {
                    self.ctx.copy_text(result.title.clone());
                }
                (RowAction::Rename, Some(result)) => {
                    if let Some(path) = result_path(&result) {
                        // The name that is typed starts with the name the file
                        // has now, without the folder.
                        let name = std::path::Path::new(&path)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| result.title.clone());
                        self.rename_text = name;
                        self.rename_path = Some(path);
                    }
                }
                (RowAction::Delete, Some(result)) => {
                    if let Some(path) = result_path(&result) {
                        self.system_dialog_opened();
                        crate::shellmenu::delete_to_bin(hwnd, &path);
                    }
                }
                (RowAction::Properties, Some(result)) => {
                    if let Some(path) = result_path(&result) {
                        self.system_dialog_opened();
                        crate::shellmenu::show_properties(hwnd, &path);
                        // The property page is a window of this process, but it
                        // is not a child of the launcher and not above it: the
                        // launcher is always on top and would cover the page.
                        // So the launcher goes away and lets the user do their
                        // work.
                        self.hide();
                    }
                }
                (_, _) => {}
            }
        }

        // The last row of a scrolling list is cut in half by the edge of the
        // list. A soft veil over the bottom lets it fade out instead, which
        // reads as "there is more below" rather than as a broken layout.
        draw_bottom_veil(&painter, list_rect, &palette, opacity);

        // ---- preview ----------------------------------------------------
        if let Some(preview_rect) = preview_rect {
            if let Some(content) = preview_content {
                // A video gets a player of its own; the old one is let go
                // when another file is looked at.
                //
                // The player reads the video from the disk, so it only
                // starts when the user asked for that.
                if self.settings.preview_media {
                    self.prepare_video(&content);
                }
                let mut preview_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(preview_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                preview_ui.set_clip_rect(preview_rect);
                preview::show(
                    &mut preview_ui,
                    &content,
                    &palette,
                    opacity,
                    &self.thumbnails,
                    &self.details,
                    self.video.as_mut(),
                    &self.ctx,
                    settled,
                    self.settings.preview_media,
                );
            } else {
                self.video = None;
                // The pane is there but the row under the pointer has nothing
                // to show. The card is drawn anyway — an empty card looks
                // like part of the window, a hole looks like a mistake.
                painter.rect_filled(
                    preview_rect,
                    egui::CornerRadius::same(8),
                    palette.surface_alt.gamma_multiply(opacity * 0.7),
                );
            }
        }
        // ---- the tip of the resting row ----------------------------------
        //
        // The whole title and path of the row under a pointer that rests
        // there. It is drawn by the launcher itself, on top of the rows,
        // with a fade that is **paced** — the tip of `egui` rides on an
        // `Area`, and an `Area` fades in with a bare `request_repaint` for
        // every frame of the fade, which on a machine whose frames are
        // expensive is a stutter each time a tip appears. This one fades
        // just as quickly, but at a frame rate the screen can actually
        // show, and it is nothing but a rounded box and two lines of text.
        if settled && hovered_row == Some(selected) && !self.results.is_empty() {
            if self.tip_row != Some(selected) {
                self.tip_row = Some(selected);
                self.tip_since = Instant::now();
            }
            let k = (self.tip_since.elapsed().as_secs_f32() / 0.15).min(1.0);
            let fade = k * k;
            if k < 1.0 {
                let pace = u64::from(crate::mouse::interval_ms().clamp(16, 33));
                self.ctx.request_repaint_after(Duration::from_millis(pace));
            }
            // The list has gone back into the app by now; this borrows it
            // from there.
            let results = &self.results;
            let result = &results[selected];
            let tip_font = FontId::proportional(self.settings.result_sub_item_font_size as f32);
            let max_width = (inner.width() - 24.0).max(120.0);
            let title = self.ctx.fonts_mut(|f| {
                f.layout(
                    result.title.clone(),
                    tip_font.clone(),
                    palette.text.gamma_multiply(opacity * fade),
                    max_width,
                )
            });
            let subtitle = if result.subtitle.is_empty() {
                None
            } else {
                Some(self.ctx.fonts_mut(|f| {
                    f.layout(
                        result.subtitle.clone(),
                        tip_font,
                        palette.text_dim.gamma_multiply(opacity * fade),
                        max_width,
                    )
                }))
            };
            let pad = 9.0;
            let tip_size = egui::vec2(
                title.size().x.max(subtitle.as_ref().map_or(0.0, |g| g.size().x)) + pad * 2.0,
                title.size().y
                    + subtitle.as_ref().map_or(0.0, |g| g.size().y + 3.0)
                    + pad * 2.0,
            );
            // Under the row when there is room, over it when there is not.
            let row_bottom = {
                let (base, base_top, pitch, scroll_then) = self.row_geometry;
                base_top + (selected as f32 + 1.0 - base as f32) * pitch
                    + (self.scroll_offset - scroll_then)
            };
            let mut tip_pos = egui::pos2(
                (list_rect.left() + 6.0)
                    .min(inner.right() - tip_size.x - 8.0)
                    .max(inner.left() + 8.0),
                row_bottom + 4.0,
            );
            if tip_pos.y + tip_size.y > inner.bottom() - 6.0 {
                tip_pos.y = (row_bottom - row_pitch() - 4.0 - tip_size.y).max(inner.top() + 6.0);
            }
            let tip_rect = Rect::from_min_size(tip_pos, tip_size);
            // The tip is painted on the **foreground** layer, above every
            // widget — the preview pane is painted after the list, and a tip
            // drawn in the same layer as the rows would end up underneath
            // the pane wherever the two overlap. On this layer it is over
            // everything, which is where a tip belongs.
            let tip = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                // `Id::new` is deprecated in favour of salt-based ids, but a
                // custom layer wants one **stable** id of its own — a fresh
                // one every frame would pile up a new layer per frame.
                {
                    #[allow(deprecated)]
                    let id = egui::Id::new("row-tip");
                    id
                },
            ));
            tip.rect_filled(
                tip_rect,
                CornerRadius::same(metrics::ROW_RADIUS as u8),
                palette.surface.gamma_multiply(opacity * fade),
            );
            tip.rect_stroke(
                tip_rect,
                CornerRadius::same(metrics::ROW_RADIUS as u8),
                Stroke::new(1.0, palette.border.gamma_multiply(opacity * fade)),
                egui::StrokeKind::Outside,
            );
            let text_left = tip_rect.left() + pad;
            let mut text_top = tip_rect.top() + pad;
            let title_h = title.size().y;
            tip.galley(
                egui::pos2(text_left, text_top),
                title,
                Color32::WHITE,
            );
            if let Some(subtitle) = subtitle {
                text_top += title_h + 3.0;
                tip.galley(egui::pos2(text_left, text_top), subtitle, Color32::WHITE);
            }
        } else if self.tip_row.is_some() {
            // The pointer moved on — the tip goes at once. A fade-out would
            // ask for frames to hide something nobody is looking at.
            self.tip_row = None;
        }

        // ---- footer -----------------------------------------------------
        let footer_rect = Rect::from_min_size(
            egui::pos2(inner.left(), inner.bottom() - footer_height),
            Vec2::new(inner.width(), footer_height),
        );
        let index_state = if self.file_plugin.is_building() {
            // The building comes first: a rebuild that runs while the old
            // table is still in use has to show that it is running, or the
            // button in the settings looks like it did nothing at all.
            let progress = self.file_plugin.progress();
            FileIndexState::Building {
                entries: progress.entries,
                fraction: progress.fraction,
                phase: progress.phase,
            }
        } else if self.file_plugin.is_scanning() {
            // The folders of the user are being walked through. This is the
            // index that works without administrative privileges.
            FileIndexState::Scanning(self.file_plugin.scanned())
        } else {
            FileIndexState::Ready
        };
        // More results than fit on the screen? Then the list has to be paged,
        // and the footer says so.
        let rows_that_fit = ((list_height / row_pitch()).floor().max(1.0)) as usize;
        let paging = self.results.len() > rows_that_fit;
        draw_footer(
            ui,
            footer_rect,
            &palette,
            opacity,
            index_state,
            paging,
            animation_phase(self.frame_clock),
        );
        // ---- the player of a video ---------------------------------------
        //
        // The player belongs to one file: when another file is looked at,
        // it is let go — a decoder that keeps running is a decoder that
        // keeps the disk busy.

        // ---- the name of a file is being changed ---------------------------
        //
        // A small window of its own, so that the list stays where it is.
        if let Some(path) = self.rename_path.clone() {
            let mut done = false;
            let mut cancel = false;
            egui::Window::new("重命名")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::LEFT_TOP, egui::vec2(24.0, 96.0))
                .show(&self.ctx, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.rename_text)
                            .desired_width(280.0)
                            .hint_text("新的名字"),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("确定").clicked() {
                            done = true;
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        done = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancel = true;
                    }
                });
            if done {
                self.rename_path = None;
                rename_file(&path, self.rename_text.trim());
                self.run_query();
            } else if cancel {
                self.rename_path = None;
            }
        }

        // ---- the window follows the results -------------------------------
        //
        // A window that keeps its height whatever it holds leaves a hole
        // under a short list and pushes the long one out of sight. So the
        // height follows the number of rows, between the smallest window and
        // the tallest one that still fits on the screen.
        //
        // It is done **once per query**: the results change by themselves
        // every now and then (the index is watched), and a window that was
        // resized every time would make the list flicker.
        if self.fit_window {
            self.fit_window = false;
            self.fit_window_to_results(ui);
        }

        // ---- keyboard ----------------------------------------------------
        if self.visible {
            handle_keyboard(ui, self);
        }

        // Keep the animation running and refresh the clock.
        if let Some(t) = self.shown_at {
            if t.elapsed() < Duration::from_millis(260) {
                // The fade lasts 140 ms, and thirty frames a second show a
                // fade perfectly well — a fade is the most forgiving thing
                // there is to animate. The frame must not be asked for "as
                // soon as possible" (the default of `request_repaint`):
                // the window can make three hundred frames a second, and a
                // burst of them at every summon is exactly what the pointer
                // stumbles over on a machine whose frames are expensive to
                // put on the screen. A summon is the most frequent thing
                // this window does.
                self.ctx
                    .request_repaint_after(Duration::from_millis(33));
            }
        }
    }
}
