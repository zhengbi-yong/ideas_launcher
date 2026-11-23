use super::*;

impl LauncherApp {
    /// Hand the current query to the worker thread.
    ///
    /// The answer comes back through `apply_results`; the UI thread never
    /// blocks on a plugin.
    pub(super) fn run_query(&mut self) {
        // A new query starts: the one that is still running is of no use any
        // more, and it is told to stop.
        self.query_generation = self.cancel.bump();

        // **The prefix cache.** A query that grows at the end — the user
        // typed another letter — can only *shrink* the answer: everything
        // that matches `rust` also matched `rus`. So the list that is on
        // the screen is narrowed down to the new text **right here, in the
        // frame of the keystroke**, and the window reacts to the key with
        // no wait at all. The real query walks the whole index in the
        // background (~100 ms for nine million files) and replaces the
        // list when it answers; until then the narrowed list is what the
        // user would have seen a moment later anyway.
        //
        // The match is asked of the same matcher the plugins use, against
        // the title (the name of a file) and, failing that, the subtitle
        // (its path) — the two things the plugins match against too. A
        // result that slips through (pinyin, exotic precision settings)
        // comes back with the real answer a moment later; this list is a
        // preview, not the verdict.
        if !self.last_query_text.is_empty()
            && self.query_text.starts_with(&self.last_query_text)
            && !self.results.is_empty()
        {
            let matcher = launcher_core::matcher::StringMatcher::new();
            let prepared = matcher.prepare(&self.query_text);
            let original = std::mem::take(&mut self.results);
            let mut kept: Vec<LauncherResult> = Vec::with_capacity(original.len());
            let mut rest: Vec<LauncherResult> = Vec::new();
            for result in original {
                let hit = matcher
                    .match_prepared_lower(&prepared, &result.title)
                    .is_some()
                    || (!result.subtitle.is_empty()
                        && matcher
                            .match_prepared_lower(&prepared, &result.subtitle)
                            .is_some());
                if hit {
                    kept.push(result);
                } else {
                    rest.push(result);
                }
            }
            // An empty narrowing is **not** shown. The matcher here is a
            // preview, not the verdict: pinyin and the precision settings
            // can disagree with it, and a list that empties itself for a
            // hundredth of a second — only to fill up again when the real
            // answer lands — is exactly the flicker this window does not
            // want. The old list stays; the real answer is a moment away.
            if !kept.is_empty() {
                self.results = kept;
                // The list got shorter; the selected row keeps its place
                // when it still exists, and the window follows the length.
                self.selected = self.selected.min(self.results.len().saturating_sub(1));
                self.fit_window = true;
                log::debug!(
                    "the prefix cache narrowed the list to {} results at once",
                    self.results.len()
                );
            } else {
                self.results = rest;
                log::debug!("the prefix cache kept the list: the narrowing was empty");
            }
        }
        self.last_query_text = self.query_text.clone();

        let request = QueryRequest {
            generation: self.query_generation,
            text: self.query_text.clone(),
            sent_at: Instant::now(),
        };
        if self.query_tx.send(request).is_err() {
            log::error!("the query worker is gone");
        }
    }

    /// Take the answers the worker produced and show them.
    pub(super) fn drain_query_results(&mut self) {
        while let Ok(response) = self.query_rx.try_recv() {
            // A late answer for an older query must not overwrite a newer one.
            if response.generation < self.query_generation {
                continue;
            }
            self.apply_results(response);
        }
    }

    /// Show the results of one query.
    pub(super) fn apply_results(&mut self, response: QueryResponse) {
        let results = response.results;

        // A partial answer is the list as it stands while the slower plugins
        // are still working. It is shown as it is: moving the remembered row
        // to the top and resetting the selection would fight the user.
        if response.partial {
            // **A refresh of the same query does not show the partial
            // answer.** The index of the user's folders is watched, so a
            // query runs again every now and then — and the partial answer
            // of a file query has only a handful of results in it (the
            // programs and the settings come first). The list would shrink
            // to those few rows and grow back again a moment later, and
            // that is what made the list flicker.
            if response.text == self.query_text && !self.results.is_empty() {
                return;
            }
            if self.selected >= results.len() {
                self.selected = results.len().saturating_sub(1);
            }
            // A new query starts at the top; the same query keeps its place,
            // because the list only grows while the plugins work.
            if response.text != self.query_text {
                self.scroll_offset = 0.0;
                self.pending_scroll = Some(0.0);
                // A new query decides the preview pane again.
                self.preview_pane = false;
            }
            self.results = results;
            self.ctx.request_repaint();
            return;
        }

        // The rows the user keeps picking are already weighted by the engine,
        // so nothing is moved around here: a single pick must not shuffle the
        // list, and the selection must not jump away under the cursor.

        // A refresh of the *same* query keeps its place. The index of the
        // user's folders is watched, so a query runs again every now and then
        // — and if that moved the selection, the panel on the right would
        // flash its contents again and again.
        let same_query = response.text == self.query_text;
        if same_query {
            if self.selected >= results.len() {
                self.selected = results.len().saturating_sub(1);
            }
        } else {
            self.selected = 0;
            self.scroll_offset = 0.0;
            self.pending_scroll = Some(0.0);
            // A new query decides the preview pane again.
            self.preview_pane = false;
        }
        // The window is made taller (or shorter) for a **new** query only.
        // When the results change by themselves the window keeps its size,
        // or it would jump under the pointer every time a file is written
        // somewhere on the disk.
        //
        // The **generation** is what tells a new query from a refresh of the
        // same one: the text in the box is the same in both cases.
        if response.generation != self.fitted_generation {
            self.fitted_generation = response.generation;
            self.fit_window = true;
        }
        self.results = results;
        // A new list is a new list: the highlight snaps to the selected row
        // of **this** list. Gliding in from the row it happened to rest on
        // in the old one would be movement between two different worlds.
        self.highlight_row = self.selected as f32;
        self.highlight_vel = 0.0;
        self.last_query_time = response.elapsed;
        self.query_count += 1;
        log::info!(
            "query {:?} -> {} results in {:?} (queue {} ms, worker {} ms, transfer {} ms)",
            response.text,
            self.results.len(),
            self.last_query_time,
            (response.picked_at - response.sent_at).as_micros() as f64 / 1000.0,
            (response.finished_at - response.picked_at).as_micros() as f64 / 1000.0,
            (Instant::now() - response.finished_at).as_micros() as f64 / 1000.0,
        );
        if self.selftest_running {
            self.selftest_running = false;
            log::info!(
                "STEADY STATE: keystroke -> {} results in {:.2} ms (query {:.2} ms, handover {:.2} ms)",
                self.results.len(),
                (Instant::now() - response.sent_at).as_micros() as f64 / 1000.0,
                (response.finished_at - response.picked_at).as_micros() as f64 / 1000.0,
                (Instant::now() - response.finished_at).as_micros() as f64 / 1000.0,
            );
        }
        self.ctx.request_repaint();
    }

    /// Open the selected result.
    /// Make the window as tall as the results need it to be.
    ///
    /// The width is left alone: the user may have dragged the window to a
    /// size they like, and only the height is decided by the content.
    ///
    /// The window grows downwards from the place it has, because a window
    /// that jumps around while the user types is worse than a window that
    /// is a little too tall. The height is capped so that it never reaches
    /// past the bottom of the screen.
    ///
    /// **The height follows the list and nothing else.** It used to be
    /// pushed up by the preview as well, and that made the window jump
    /// back and forth by half a screen every time the pointer moved over
    /// a row — which is the same as saying that the list flickered.
    /// The preview copes with a shorter pane: it scales the picture down
    /// and scrolls the details.
    pub(super) fn fit_window_to_results(&mut self, ui: &Ui) {
        let (screen, top) = self.screen_limits();
        // The space the list may take: the window without the search row, the
        // footer and the gaps around them.
        let chrome = metrics::PAD * 2.0
            + metrics::SEARCH_HEIGHT
            + metrics::GAP
            + metrics::FOOTER_HEIGHT
            + 6.0;
        let pitch = row_pitch();
        let room = (screen - top - chrome).max(pitch);
        // How many rows fit between the search row and the footer.
        let fits = ((room / pitch).floor().max(1.0)) as usize;
        let rows = self.results.len().min(fits);
        // The height of the list is the height of its rows: the last row has
        // no gap under it.
        let wanted = (chrome + list_content_height(rows))
            .clamp(metrics::MIN_WINDOW_SIZE.y, (screen - top).max(metrics::MIN_WINDOW_SIZE.y));

        let size = ui.ctx().viewport_rect().size();
        if (wanted - size.y).abs() > 1.0 {
            log::info!("the window follows the results: {size:?} -> {wanted:?}");
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(size.x, wanted)));
        }
    }

    /// The height of the working area of the screen, and the place the
    /// window sits in it, both in points.
    ///
    /// The window sits where the cursor is, so the screen of the cursor is
    /// the screen the window is on. The working area is measured in physical
    /// pixels while the layout is in points, so the scale of the window turns
    /// one into the other.
    pub(super) fn screen_limits(&self) -> (f32, f32) {
        let mut cursor = winapi::POINT {
            x: 0,
            y: 0,
        };
        let mut info = winapi::MONITORINFO {
            cbSize: std::mem::size_of::<winapi::MONITORINFO>() as u32,
            rcMonitor: winapi::RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            rcWork: winapi::RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            dwFlags: 0,
        };
        let mut rect = winapi::RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        unsafe {
            if winapi::GetCursorPos(&mut cursor) == 0 {
                return (800.0, 0.0);
            }
            let monitor = winapi::MonitorFromPoint(cursor, 0x0000_0002);
            if winapi::GetMonitorInfoW(monitor, &mut info) == 0 {
                return (800.0, 0.0);
            }
            let physical = (info.rcWork.bottom - info.rcWork.top) as f32;
            // Where the window sits in the working area.
            let physical_top = if winapi::GetWindowRect(self.hwnd, &mut rect) != 0 {
                (rect.top - info.rcWork.top) as f32
            } else {
                0.0
            };

            // The scale comes from the dots per inch of the window. The size
            // of the window would do as well, but it changes while the window
            // is resized, and the screen would then be measured with the
            // wrong scale for a frame.
            let scale = winapi::GetDpiForWindow(self.hwnd) as f32 / 96.0;
            if scale <= 0.05 {
                return (physical, physical_top);
            }
            (physical / scale, (physical_top / scale).max(0.0))
        }
    }
}
