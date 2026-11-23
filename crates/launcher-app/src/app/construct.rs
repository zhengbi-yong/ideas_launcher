use super::*;

impl LauncherApp {
    /// Build the application.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::paths::ensure_directories();

        // --- enable the input method ---------------------------------------
        //
        // `winit` creates every window with the IME switched off
        // (`WindowState::ime_allowed` starts out `false` and the window is
        // created with `IACE_CHILDREN`), so a Chinese, Japanese or Korean
        // keyboard would never compose anything. Turning it on here is what
        // makes it possible to type 中文 into the search box.
        #[cfg(target_os = "windows")]
        {
            if let Some(window) = cc.winit_window() {
                window.set_ime_allowed(true);
            }
        }

        let settings = Settings::load(&crate::paths::settings_file());
        let palette = palette_for(&settings);

        // The token that lets an overtaken query stop early. Every keystroke
        // bumps it, so the scan of the previous keystroke gives up instead of
        // working for an answer nobody will see.
        let cancel = launcher_core::cancel::Cancel::new();

        let ctx = cc.egui_ctx.clone();
        crate::theme::apply_visuals(&ctx, &palette);
        {
            let _timer = launcher_core::profile::phase("startup.fonts");
            crate::fonts::install(&ctx);
        }

        let program_plugin = Arc::new(ProgramPlugin::new());
        program_plugin.start_indexing();

        let file_index = Arc::new(FileIndex::new());
        // **How often the changes go into the index.** The changes are put
        // in by rounds, so that a program which writes thousands of files
        // does not keep the index busy — a file that was just made is
        // hardly the file somebody is looking for in the next half minute.
        file_index.set_update_interval(settings.index_update_interval);
        // The table of the whole file system is kept between runs: reading
        // the master file table takes minutes, and the table only has to be
        // read again when the set of volumes changes.
        let file_plugin = Arc::new(
            FilePlugin::new(file_index.clone())
                .with_cache(crate::paths::cache_directory().join("fs-index.bin"))
                .with_cancel(cancel.clone()),
        );
        // The settings steer which volumes are indexed, what is left out and
        // how the results are ordered.
        file_plugin.configure(&settings);
        // Build the file index in the background; it is never on the hot path.
        file_plugin.refresh(SearchOptions::default());

        let bookmark_plugin = Arc::new(BookmarkPlugin::new());

        // The Store applications are indexed in the background; the shell walk
        // is far too slow for the start-up path.
        let uwp_plugin = Arc::new(UwpPlugin::new());
        uwp_plugin.start_indexing();

        let engine = Engine::new(plugins::plugin_context());
        // The window shows a handful of rows, but the list is scrolled
        // through — so the results are not cut off at the height of the
        // window. A file that is further down has to be findable.
        engine.set_max_results(settings.max_results_to_show.max(100));
        engine.set_precision(settings.query_search_precision);
        engine.set_result_order(settings.result_order);
        engine.register(program_plugin.clone());
        engine.register(file_plugin.clone());
        engine.register(uwp_plugin.clone());
        engine.register(Arc::new(SysPlugin::new()));
        engine.register(bookmark_plugin.clone());
        engine.register(Arc::new(ProcessPlugin::new()));
        engine.register(Arc::new(
            plugins::windows_settings::WindowsSettingsPlugin::new(),
        ));
        for plugin in plugins::default_plugins()
            .into_iter()
            .filter(|p| p.id() != "program")
        {
            engine.register(plugin);
        }
        // The web-search providers each own a short keyword (`g`, `b`, `dd`…).
        for keyword in plugins::default_action_keywords() {
            engine.register_keyword("websearch", &keyword);
        }

        let history = {
            let _timer = launcher_core::profile::phase("startup.history");
            History::load(crate::paths::history_file())
        };
        let selection = Arc::new(SelectionMemory::load(crate::paths::selection_file()));
        // The engine ranks the rows with what the user picked before.
        selection.set_threshold(if settings.enable_smart_ranking {
            settings.pick_count_threshold
        } else {
            0
        });
        engine.set_pick_memory(selection.clone());

        let hwnd = native_window_handle(&cc);

    // The shape of the window belongs to Windows.
    //
    // Windows 11 rounds the corners of every top level window with a radius
    // of its own — 8 points — and it cuts the client area along that curve
    // whether the application wants it or not. The launcher used to draw a
    // rounded card of its own on top, which put two different curves next to
    // each other: the sliver in between showed up as a dark smudge in the
    // corners, and the outline did not follow the curve the system cut.
    //
    // So the card is painted with the very same radius, and the system does
    // the cutting. The window's own border is switched off, because DWM draws
    // that one along the *rectangle* — it would fill the rounded corners
    // and make the window look square.
    if !hwnd.is_null() {
        unsafe {
            // Square corners.
            //
            // Windows rounds the corners of every window with a radius of its
            // own, and it cuts the client area along that curve — which
            // would leave a crescent of the desktop in every corner of the
            // card. The launcher wants a plain rectangle, and now that the
            // window is opaque the system is willing to leave the corners
            // alone.
            let corner = winapi::DWMWCP_DONOTROUND;
            winapi::DwmSetWindowAttribute(
                hwnd,
                winapi::DWMWA_WINDOW_CORNER_PREFERENCE,
                &corner as *const i32 as *const std::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );

            // The outline is painted by the launcher, along the same curve.
            let border = winapi::DWMWA_COLOR_NONE;
            winapi::DwmSetWindowAttribute(
                hwnd,
                winapi::DWMWA_BORDER_COLOR,
                &border as *const i32 as *const std::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );

            // No acrylic sheet behind the window.
            //
            // Windows 11 puts an acrylic backdrop behind every window that
            // looks like an ordinary application window. The launcher paints
            // its own card, so that sheet showed up as a blurred rectangle
            // around it — the window looked like two layers instead of one.
            let backdrop = winapi::DWMSBT_NONE;
            winapi::DwmSetWindowAttribute(
                hwnd,
                winapi::DWMWA_SYSTEMBACKDROP_TYPE,
                &backdrop as *const i32 as *const std::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );
            let mica = 0;
            winapi::DwmSetWindowAttribute(
                hwnd,
                winapi::DWMWA_MICA_EFFECT,
                &mica as *const i32 as *const std::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );
        }

        // **The mouse is not allowed to ask for more frames than the screen
        // can show.**
        //
        // `egui` draws a new frame the moment the pointer moves, and there is
        // no way to tell it to wait — the two fields that decide it are
        // private to `egui`. A mouse that reports a thousand times a second
        // therefore asks for a thousand frames a second.
        //
        // Every one of those frames goes through the desktop compositor and
        // through everything that watches the screen: the overlay of the
        // graphics driver, the remote desktop program, the virtual display
        // driver. On a machine with all of those, one frame can take forty
        // milliseconds — and a hundred frames a second then asks for four
        // seconds of work per second. The window itself is drawn in three
        // microseconds and does not get slow; **the desktop does**, and the
        // pointer with it.
        //
        // So the window procedure is replaced by one that holds the mouse
        // messages back. The pointer is never in the wrong place — only a
        // little behind, and only for a moment.
        crate::mouse::install(hwnd);
    }

        // The query runs on its own thread, exactly like Flow Launcher's
        // `ChangeQueryTextAsync` → `QueryAsync` chain: the UI thread only
        // sends the text and picks the answer up when it comes back. Doing
        // the work on the UI thread would freeze the window for as long as
        // the slowest plugin needs.
        let (query_tx, request_rx) = channel::<QueryRequest>();
        let (response_tx, query_rx) = channel::<QueryResponse>();
        {
            let engine = engine.clone();
            let worker_ctx = ctx.clone();
            let worker_hwnd = hwnd as usize;
            std::thread::Builder::new()
                .name("query".into())
                .spawn(move || {
                    while let Ok(mut request) = request_rx.recv() {
                        // Coalesce: while the previous query was running the
                        // user may have typed more. Only the newest request is
                        // worth answering; the older ones are already stale.
                        while let Ok(next) = request_rx.try_recv() {
                            request = next;
                        }
                        let picked_at = Instant::now();
                        let generation = request.generation;
                        let text = request.text.clone();
                        let sent_at = request.sent_at;

                        // The sink is called with the merged list every time a
                        // plugin is done, so the window can fill up while a
                        // slow plugin — an out-of-process one, typically — is
                        // still working.
                        let sink: Arc<dyn Fn(Vec<LauncherResult>) + Send + Sync> = {
                            let response_tx = response_tx.clone();
                            let worker_ctx = worker_ctx.clone();
                            let text = text.clone();
                            // A partial answer costs a frame, and the plugins
                            // answer in a burst — eight of them, one after
                            // the other, is eight frames for one keystroke.
                            // The lists are cumulative: a dropped partial
                            // changes nothing, the next one carries
                            // everything. So at most one partial is sent per
                            // 50 ms; the final answer is always sent.
                            let started = Instant::now();
                            let last_partial = std::sync::atomic::AtomicU64::new(0);
                            Arc::new(move |results| {
                                let now = started.elapsed().as_millis() as u64;
                                let last = last_partial.load(Ordering::Relaxed);
                                if now.saturating_sub(last) < 50 {
                                    return;
                                }
                                last_partial.store(now, Ordering::Relaxed);
                                let response = QueryResponse {
                                    generation,
                                    text: text.clone(),
                                    results,
                                    elapsed: Duration::ZERO,
                                    sent_at,
                                    picked_at,
                                    finished_at: Instant::now(),
                                    partial: true,
                                };
                                let _ = response_tx.send(response);
                                worker_ctx.request_repaint();
                                crate::hotkey::wake(worker_hwnd);
                            })
                        };

                        let outcome =
                            engine.query_with_sink_at(&request.text, sink, request.generation);
                        let finished_at = Instant::now();
                        log::debug!(
                            "query {:?} answered in {:?} with {} results (queue {} ms)",
                            request.text,
                            outcome.elapsed,
                            outcome.results.len(),
                            (picked_at - request.sent_at).as_millis(),
                        );
                        let response = QueryResponse {
                            generation: request.generation,
                            text: request.text,
                            results: outcome.results,
                            elapsed: outcome.elapsed,
                            sent_at: request.sent_at,
                            picked_at,
                            finished_at,
                            partial: false,
                        };
                        if response_tx.send(response).is_err() {
                            break;
                        }
                        // Wake the UI thread so it can pick the answer up.
                        worker_ctx.request_repaint();
                        crate::hotkey::wake(worker_hwnd);
                    }
                })
                .ok();
        }

        let toggle_requested = Arc::new(AtomicBool::new(false));
        let visible_flag = Arc::new(AtomicBool::new(false));
        let on_trigger: Arc<dyn Fn() + Send + Sync + 'static> = {
            let flag = toggle_requested.clone();
            // eframe only wakes up — and only then calls `logic` — when a
            // repaint is requested. Without this the hotkey would be swallowed
            // while the launcher sits idle.
            let ctx = ctx.clone();
            let visible = visible_flag.clone();
            // The handle crosses a thread boundary, so it travels as an
            // integer — a raw pointer is not `Send`.
            let hwnd = hwnd as usize;
            Arc::new(move || {
                flag.store(true, Ordering::SeqCst);
                // Show or hide the window right here, on the hotkey thread.
                // Waiting for the UI thread to wake up would add the event
                // loop's wake-up latency to every invocation.
                let now_visible = !visible.load(Ordering::SeqCst);
                visible.store(now_visible, Ordering::SeqCst);
                if now_visible {
                    // Steal the focus from the hotkey thread: that thread is
                    // the one Windows grants foreground rights to.
                    crate::hotkey::force_foreground(hwnd as winapi::HWND);
                } else {
                    crate::hotkey::set_window_visible(hwnd as winapi::HWND, false);
                }
                ctx.request_repaint();
                // `request_repaint` only sets a flag inside egui; it does not
                // wake the Win32 message loop, which is blocked in
                // `MsgWaitForMultipleObjects`. Posting a message to the window
                // is what actually gets the event loop going again.
                crate::hotkey::wake(hwnd);
            })
        };
        let hotkey = match GlobalHotkey::register_any(
            &hotkey_candidates(&settings.hotkey),
            on_trigger,
        ) {
            Ok((h, description)) => {
                log::info!("global hotkey active: {description}");
                Some((h, description))
            }
            Err(e) => {
                log::error!("could not register any global hotkey: {e}");
                None
            }
        };
        let hotkey_description = hotkey
            .as_ref()
            .map(|(_, description)| description.clone())
            .unwrap_or_default();

        // The user's own plugins run in their own processes and talk to the
        // launcher over JSON-RPC. They are registered with the engine, which
        // starts them on a background thread.
        let host_state = Arc::new(HostState::default());
        // A thread of its own watches the foreground window, so that the
        // launcher goes away as soon as it is no longer in front.
        Self::watch_foreground(ctx.clone(), hwnd as usize, visible_flag.clone());
        let host_api = Arc::new(LauncherHostApi {
            state: host_state.clone(),
            visible_flag: visible_flag.clone(),
            hwnd: hwnd as usize,
            ctx: ctx.clone(),
            matcher: Mutex::new(launcher_core::matcher::StringMatcher::new()),
        });
        let mut json_rpc_plugins: Vec<Arc<JsonRpcPlugin>> = Vec::new();
        for metadata in plugins::jsonrpc::discover_plugins(&crate::paths::plugins_directory()) {
            let plugin = Arc::new(JsonRpcPlugin::new(metadata, host_api.clone()));
            let id = plugin.id().to_string();
            engine.register(plugin.clone());
            json_rpc_plugins.push(plugin);
            log::info!("external plugin {id} registered");
        }

        let mut settings_view = SettingsView::new();
        settings_view.set_theme_list(Theme::builtin_names());
        settings_view.set_effective_hotkey(&hotkey_description);

        // Another instance of the launcher asks for the window through this
        // event, so that starting it twice brings it up instead of doing
        // nothing.
        crate::instance::watch_for_show_request(
            &crate::instance::instance_id(),
            hwnd as usize,
            visible_flag.clone(),
            toggle_requested.clone(),
            ctx.clone(),
        );

        let tray = crate::tray::Tray::new("IDEAS Launcher", &hotkey_description, cc.egui_ctx.clone());
        // The icon stays folded away unless the user asks for it to be
        // visible on the task bar.
        if let Some(tray) = &tray {
            tray.set_promoted(settings.always_show_tray_icon);
        }

        Self {
            settings,
            palette,
            engine,
            program_plugin,
            file_plugin,
            file_index,
            bookmark_plugin,
            uwp_plugin,
            icons: IconCache::new(256, ctx.clone()),
            byte_icons: icons::ByteIconCache::new(ctx.clone()),
            thumbnails: preview::ThumbnailCache::new(ctx.clone()),
            details: details::DetailsCache::new(ctx.clone()),
            video: None,
            pending_video: None,
            json_rpc_plugins,
            host_state,
            history,
            selection,
            query_text: String::new(),
            results: Vec::new(),
            selected: 0,
            selected_at: Instant::now(),
            selected_seen: 0,
            preview_pane: false,
            scroll_offset: 0.0,
            pending_scroll: None,
            fit_window: false,
            fitted_generation: 0,
            visible: false,
            view: View::Search,
            settings_view,
            toggle_requested,
            visible_flag,
            hotkey,
            query_tx,
            query_rx,
            query_generation: 0,
            last_query_text: String::new(),
            highlight_row: 0.0,
            highlight_vel: 0.0,
            tip_row: None,
            tip_since: Instant::now(),
            row_geometry: (0, 0.0, row_pitch(), 0.0),
            mismatch_since: None,
            row_changed_at: Instant::now(),
            pointer_seen: None,
            sweeping: false,
            pointer_moved_at: Instant::now(),
            tip_hovered: None,
            preview_row: None,
            cancel,
            shown_at: None,
            last_query_time: Duration::ZERO,
            query_count: 0,
            ctx,
            hwnd,
            logged_layout: false,
            frames: 0,
            frames_at: Instant::now(),
            last_frame: Instant::now(),
            frame_gaps: Vec::new(),
            mouse_passed_before: 0,
            trace_frames: std::env::var_os("IDEAS_TRACE_FRAMES").is_some(),
            foreground_refused: false,
        rename_path: None,
        rename_text: String::new(),
        system_dialog_until: None,
            list_height: 0.0,
            first_frame: true,
            search_id: None,
            pending_focus: false,
            was_focused: false,
            startup_show: false,
            startup_query: None,
            selftest: None,
            selftest_at: None,
            selftest_running: false,
            hide_on_deactivate: true,
            ever_focused: false,
            focus_lost_at: None,
            tray,            ime_reported: None,
            program_index_ready: false,
            uwp_index_ready: false,
            file_index_ready: false,
            full_index_ready: false,
            live_version_seen: 0,
            frame_clock: Instant::now(),
            open_settings_on_show: false,
        }
    }

    /// A thread that watches the foreground window.
    ///
    /// The window goes away when it is no longer in front. The check needs a
    /// frame, and a frame is only painted when something happens — and this
    /// window is told about the change of the foreground window by nobody.
    /// So a thread of its own keeps an eye on it and wakes the interface up
    /// the moment it is somebody else's turn. Without it the window would
    /// linger for a quarter of a second.
    pub(super) fn watch_foreground(ctx: egui::Context, hwnd: usize, visible: Arc<AtomicBool>) {
        let spawned = std::thread::Builder::new()
            .name("foreground-watch".into())
            .spawn(move || {
                let hwnd = hwnd as winapi::HWND;
                let mut last = std::ptr::null_mut();
                loop {
                    // The window has to go *at once* when the user clicks
                    // somewhere else, so the front is looked at often.
                    std::thread::sleep(Duration::from_millis(10));
                    if !visible.load(Ordering::Relaxed) {
                        last = std::ptr::null_mut();
                        continue;
                    }
                    let foreground = unsafe { winapi::GetForegroundWindow() };
                    if foreground == last {
                        continue;
                    }
                    last = foreground;
                    if foreground != hwnd {
                        // Somebody else is in front: the window has to go.
                        // The flag of `egui` is not enough on its own — the
                        // event loop has to be woken up as well, or the frame
                        // that hides the window would wait for the next one.
                        ctx.request_repaint();
                        crate::hotkey::wake(hwnd as usize);
                    }
                }
            });
        if spawned.is_err() {
            log::warn!("the foreground window cannot be watched");
        }
    }
}
