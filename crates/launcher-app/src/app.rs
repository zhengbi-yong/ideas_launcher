//! The launcher application.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, CornerRadius, FontId, Rect, Response, RichText, Sense, Stroke, TextEdit,
    Ui, Vec2,
};
use launcher_core::engine::Engine;
use launcher_core::history::{History, SelectionMemory};
use launcher_core::plugin::QueryPlugin;
use launcher_core::result::Result as LauncherResult;
use launcher_core::settings::{ColorScheme, Settings};
use launcher_core::theme::Theme;
use launcher_index::files::{FileIndex, SearchOptions};
use parking_lot::Mutex;

use crate::details;
use crate::hotkey::GlobalHotkey;
use crate::icons::{self, IconCache};
use crate::plugins::{
    self, bookmarks::BookmarkPlugin, files::FilePlugin, jsonrpc::JsonRpcPlugin,
    processes::ProcessPlugin, sys::SysPlugin, uwp::UwpPlugin, ProgramPlugin,
};
use crate::preview;
use crate::settings_view::SettingsView;
use crate::theme::{metrics, Palette};
use crate::video;
use crate::winapi;

/// What the window is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// The search window.
    Search,
    /// The settings overlay.
    Settings,
}

/// A query handed to the worker thread.
///
/// The generation number makes stale answers recognisable: only the newest
/// request may replace the result list.
#[derive(Debug, Clone)]
struct QueryRequest {
    generation: u64,
    text: String,
    /// When the request left the UI thread.
    sent_at: Instant,
}

/// The answer the worker thread sends back.
#[derive(Debug)]
struct QueryResponse {
    generation: u64,
    text: String,
    results: Vec<LauncherResult>,
    elapsed: Duration,
    /// When the request left the UI thread.
    sent_at: Instant,
    /// When the worker picked the request up.
    picked_at: Instant,
    /// When the query finished.
    finished_at: Instant,
    /// Whether more results may follow.
    ///
    /// The worker sends the results of every plugin as they come in, so the
    /// window can fill up while a slow plugin is still working.
    partial: bool,
}

/// The state the external plugins share with the app.
///
/// A plugin runs in its own process, so it cannot touch the app directly; it
/// asks for things through the host API, and the app picks the requests up
/// on its own thread.
#[derive(Default)]
struct HostState {
    /// A query a plugin asked for.
    pending_query: Mutex<Option<String>>,
    /// The messages a plugin asked to show.
    messages: Mutex<Vec<(String, String)>>,
}

/// The host API the external plugins talk to.
///
/// This is the counterpart of `JsonRPCPublicAPI` in Flow Launcher: the
/// original hands the plugin the whole `IPublicAPI` surface, this is the
/// part of it a plugin realistically uses.
struct LauncherHostApi {
    state: Arc<HostState>,
    visible_flag: Arc<AtomicBool>,
    /// The native window handle, as an integer so it can cross threads.
    hwnd: usize,
    ctx: egui::Context,
    matcher: Mutex<launcher_core::matcher::StringMatcher>,
}

impl crate::plugins::jsonrpc::HostApi for LauncherHostApi {
    fn show_msg(&self, title: &str, subtitle: &str) {
        self.state
            .messages
            .lock()
            .push((title.to_owned(), subtitle.to_owned()));
        self.ctx.request_repaint();
    }

    fn show_error(&self, title: &str, subtitle: &str) {
        log::error!("plugin message: {title} — {subtitle}");
        self.show_msg(title, subtitle);
    }

    fn copy_to_clipboard(&self, text: &str) {
        self.ctx.copy_text(text.to_owned());
    }

    fn shell_run(&self, command: &str, filename: &str) {
        let filename = if filename.is_empty() { "cmd.exe" } else { filename };
        // A console program without a console window: the window that
        // appears out of nowhere takes the focus away from the launcher.
        icons::shell_run_console(filename, command);
    }

    fn open_url(&self, url: &str) {
        icons::shell_open(url);
    }

    fn open_directory(&self, path: &str) {
        icons::shell_open(path);
    }

    fn change_query(&self, query: &str) {
        *self.state.pending_query.lock() = Some(query.to_owned());
        self.ctx.request_repaint();
    }

    fn hide_window(&self) {
        self.visible_flag.store(false, Ordering::SeqCst);
        crate::hotkey::set_window_visible(self.hwnd as winapi::HWND, false);
        crate::hotkey::wake(self.hwnd);
    }

    fn show_window(&self) {
        self.visible_flag.store(true, Ordering::SeqCst);
        crate::hotkey::force_foreground(self.hwnd as winapi::HWND);
    }

    fn log(&self, level: &str, class: &str, message: &str) {
        match level {
            "error" => log::error!("{class}: {message}"),
            "warn" => log::warn!("{class}: {message}"),
            "debug" => log::debug!("{class}: {message}"),
            _ => log::info!("{class}: {message}"),
        }
    }

    fn fuzzy_search(&self, query: &str, text: &str) -> Option<i32> {
        let matcher = self.matcher.lock();
        matcher.match_result(query, text).map(|r| r.score)
    }
}

/// The application state.
pub struct LauncherApp {
    pub settings: Settings,
    pub palette: Palette,

    /// The engine. Kept alive because it owns the plugin instances, several
    /// of which are not referenced anywhere else.
    #[allow(dead_code)]
    engine: Engine,
    // Kept alive so the plugins and the hotkey stay registered for the
    // lifetime of the application.
    #[allow(dead_code)]
    program_plugin: Arc<ProgramPlugin>,
    #[allow(dead_code)]
    file_plugin: Arc<FilePlugin>,
    /// The shared file index, used for the "indexing…" hint.
    file_index: Arc<FileIndex>,
    #[allow(dead_code)]
    bookmark_plugin: Arc<BookmarkPlugin>,
    /// The Store applications.
    #[allow(dead_code)]
    uwp_plugin: Arc<UwpPlugin>,
    icons: IconCache,
    /// The icons that came as image data rather than as a path.
    byte_icons: icons::ByteIconCache,
    /// The thumbnails shown in the preview pane.
    thumbnails: preview::ThumbnailCache,
    /// The details of the files that were looked at.
    details: details::DetailsCache,
    /// The player of the video that is looked at.
    video: Option<video::VideoPlayer>,
    /// A video that is looked at, and when it was first noticed.
    ///
    /// The pointer chooses the row it is over, and a row that is passed
    /// quickly is not really being looked at. The player waits a moment, so
    /// that a sweep through the list does not start a decoder for every row
    /// it passes.
    pending_video: Option<(String, Instant)>,

    /// The external plugins the user installed.
    ///
    /// They are kept here because the app needs to find the one that produced
    /// a result when the user picks it.
    json_rpc_plugins: Vec<Arc<JsonRpcPlugin>>,
    /// The state the plugins share with the app.
    host_state: Arc<HostState>,

    /// The query history.
    pub history: History,
    /// What the user picked before, used to rank the results.
    selection: Arc<SelectionMemory>,

    query_text: String,
    results: Vec<LauncherResult>,
    selected: usize,
    /// When the selected row changed the last time.
    ///
    /// The details of a file are read from the disk — the size, the times,
    /// the index number, the streams beside it, and for a folder how many
    /// things are in it. Reading all that for every row the pointer crosses
    /// would keep the disk busy for as long as the pointer is moving, and
    /// the machine with it.
    ///
    /// So the reading waits until the pointer has come to rest on a row.
    selected_at: Instant,
    /// The row that was selected the last time it was looked at.
    selected_seen: usize,
    /// Whether the preview pane has a place in the window.
    ///
    /// The pane takes its space from the list, so the width of the list
    /// depends on it. The pointer chooses the row that is looked at, and
    /// every row with a preview would make the pane appear and every row
    /// without one would make it disappear — so the list changed its width
    /// with every row the pointer crossed, the paths were cut off and
    /// brought back, and the whole list jumped sideways.
    ///
    /// So the pane, once it is there, stays: it is decided again when the
    /// query changes.
    preview_pane: bool,
    /// Where the list is scrolled to.
    ///
    /// The list itself is scrolled by `egui`; this is the offset it ended up
    /// with, read back after every frame.
    scroll_offset: f32,
    /// An offset the app wants the list to jump to.
    ///
    /// It is written into the scroll area once and then forgotten. Writing it
    /// every frame would throw away what the user did with the wheel or with
    /// the scroll bar.
    pending_scroll: Option<f32>,
    /// Whether the window has to be made taller (or shorter) for the results.
    ///
    /// It is set when a **new** query comes in, and never when the results
    /// change by themselves — the index of the user's folders is watched, so
    /// a query runs again every now and then, and a window that was resized
    /// every time would make the list flicker.
    fit_window: bool,
    /// The query the window was last made to fit.
    ///
    /// The text in the box is the same for a new query and for a refresh of
    /// it, so the **generation** is what tells them apart.
    fitted_generation: u64,

    /// Whether the launcher window is currently on screen.
    visible: bool,
    /// Whether the window held the keyboard focus on the previous frame.
    ///
    /// Used to detect the "window was shown but never took focus" case and to
    /// implement Flow Launcher's hide-on-deactivate behaviour.
    was_focused: bool,
    /// Which view the window is showing.
    view: View,
    /// The settings overlay.
    settings_view: SettingsView,

    /// Set by the hotkey thread.
    toggle_requested: Arc<AtomicBool>,
    /// Mirrors the OS-level window visibility; owned by the hotkey thread.
    visible_flag: Arc<AtomicBool>,
    /// Kept alive so the hotkey stays registered for the lifetime of the app.
    #[allow(dead_code)]
    hotkey: Option<(GlobalHotkey, String)>,

    /// Sends new queries to the worker thread.
    query_tx: Sender<QueryRequest>,
    /// The answers the worker thread produced.
    query_rx: Receiver<QueryResponse>,
    /// The generation of the newest query that was sent.
    query_generation: u64,
    /// The text of the query before the current one.
    ///
    /// It is what the prefix cache filters: a query that grows at the end
    /// (`r` → `ru`) can only shrink the answer, so the list that is on the
    /// screen can be narrowed down to it **at once**, while the real query
    /// walks the index in the background and replaces the list when it is
    /// done.
    last_query_text: String,
    /// Where the highlight of the selected row is, in row units — a whole
    /// number when it rests, a fraction while it glides to the next row.
    highlight_row: f32,
    /// How fast the highlight is gliding, in rows per second.
    ///
    /// A spring with velocity keeps its momentum across rows: a sweep down
    /// the list builds up speed and the highlight follows like something
    /// with mass, instead of restarting a slide at every row.
    highlight_vel: f32,
    /// The row whose tip is showing, and when it began.
    ///
    /// The tip — the whole title and path of the row under the pointer — is
    /// drawn by the launcher itself: the tip of `egui` rides on an `Area`,
    /// and an `Area` fades in with a bare `request_repaint` per frame, which
    /// on a machine whose frames are expensive is a stutter every time a tip
    /// appears. This one fades in too, but at a paced frame rate, and the
    /// drawing is nothing but a rounded box and two lines of text.
    tip_row: Option<usize>,
    tip_since: Instant,
    /// The rows as they were **actually laid out** last frame:
    /// (index of the first visible row, its top, the pitch, the scroll of
    /// that moment). The highlight and the tip are drawn from this — the
    /// arithmetical guess (`list_top + i × pitch − scroll`) is close, but
    /// the rows are the truth, and a highlight that sits half a row away
    /// from the row it belongs to reads as "the highlight is wrong".
    row_geometry: (usize, f32, f32, f32),
    /// Since when the pointer has been resting on a row other than the
    /// selected one — a diagnostic: a mismatch that lasts is a bug, one
    /// that lasts a moment is the highlight gliding.
    mismatch_since: Option<(usize, Instant, bool)>,
    /// When the selected row last changed.
    ///
    /// Changes that come close together mean the pointer is **sweeping**
    /// the list, and while it sweeps the highlight has to hold on to it —
    /// a soft spring trails a third of a row behind at sweep speed, which
    /// reads as "the highlight is on the wrong row". A spring that has
    /// *just* been stirred is therefore much stiffer than one that is
    /// asked for a single quiet move.
    row_changed_at: Instant,
    /// Whether changes of the selected row are coming **fast** — a sweep of
    /// the pointer, a run of wheel notches, a held arrow key.
    ///
    /// The distinction is what keeps the highlight both tight and silky:
    /// while changes come close together the spring is stiff, so a sweep is
    /// tracked without trailing; a **solitary** change — one crossing, one
    /// key press — glides at the soft rate, because that is the move the
    /// eye wants to see travel.
    sweeping: bool,
    /// Where the pointer was last frame — the hover is **event-driven**:
    /// it is re-evaluated when the pointer moves, not on every frame.
    ///
    /// This is the principle that keeps the highlight from fighting
    /// everything else that moves the list: a selection that follows a
    /// *stationary* pointer while the list scrolls under it (keyboard,
    /// wheel) is a feedback loop — hover moves the selection, the
    /// selection scrolls the list, the list moves the row under the
    /// pointer, the hover moves the selection again. With the hover bound
    /// to pointer *moves*, a resting pointer holds its peace.
    pointer_seen: Option<egui::Pos2>,
    /// When the pointer last moved (a delivered move, not a jitter).
    pointer_moved_at: Instant,
    /// The row under the pointer, if any — a diagnostic companion to the
    /// profile log line.
    tip_hovered: Option<usize>,
    /// The row whose preview is shown.
    ///
    /// The preview is switched when the pointer **rests** on a row, not when
    /// it crosses one: a swept-past row is not being looked at, and
    /// re-laying the whole pane out for every row of a sweep is a burst of
    /// text layout — and of new glyphs — for nothing.
    preview_row: Option<usize>,
    /// Lets an overtaken query stop early.
    cancel: launcher_core::cancel::Cancel,

    /// Tracks the fade/slide-in animation.
    shown_at: Option<Instant>,
    /// Last measured query duration.
    last_query_time: Duration,
    /// Number of queries served.
    query_count: u64,

    ctx: egui::Context,
    /// Native window handle, used to show and hide the window.
    hwnd: winapi::HWND,
    /// Whether the layout diagnostic has been logged.
    logged_layout: bool,
    /// How many frames have been drawn since the last report, and when that
    /// report was made.
    ///
    /// The frame rate is the number that says whether the window is smooth.
    /// A window that redraws itself while nothing happens keeps the machine
    /// busy for nothing — and the fan with it.
    frames: u64,
    frames_at: Instant,
    /// How long ago the last frame was drawn.
    ///
    /// It is the measure of how fast the window really gets its frames onto
    /// the screen — and it is what says whether the screen can keep up with
    /// what the window asks for.
    last_frame: Instant,
    /// The gaps between the frames, from the newest to the oldest.
    frame_gaps: Vec<Duration>,
    /// How many mouse messages were let through the last time.
    mouse_passed_before: u64,
    /// Whether every frame is written to the log.
    ///
    /// The events of a frame are a debugging aid, and writing them out costs
    /// a format and a write to the disk on every frame. The environment
    /// variable `IDEAS_TRACE_FRAMES` turns them on.
    trace_frames: bool,
    /// Whether Windows refused to give the window the focus.
    ///
    /// Then the window is on the screen without being in front, and it must
    /// not take that for an answer: the user pressed the hotkey and expects
    /// to see something. The window hides itself only once it really had the
    /// focus and then lost it.
    foreground_refused: bool,
    /// The file that is being given another name.
    rename_path: Option<String>,
    /// The name that is being typed.
    rename_text: String,
    /// Until when the window keeps itself, even without the focus.
    ///
    /// The dialogs of the system — *open with*, *properties*, the question
    /// before a file goes into the bin — take the focus away from the
    /// window. Without this the window would be gone before the user has
    /// answered the question.
    ///
    /// *Open with* and *properties* put the launcher away themselves — the
    /// launcher is always on top and would lie over the dialog. The question
    /// before a file goes into the bin blocks the thread that asks it, so
    /// the window cannot go away while it is open anyway.
    system_dialog_until: Option<Instant>,
    /// Set when the window has to be placed again.
    ///
    /// The height of the result list, in points.
    ///
    /// The list is as tall as the window, and the window follows the results,
    /// so the height has to be measured rather than guessed.
    list_height: f32,
    /// Whether the first frame has been painted.
    first_frame: bool,
    /// The widget id of the search box.
    search_id: Option<egui::Id>,
    /// Set when the search box should take focus.
    pending_focus: bool,
    /// Set when the window should open itself on start-up.
    ///
    /// Used by the `--show` command line switch, which makes it possible to
    /// take a screenshot or to drive the launcher without a hotkey.
    startup_show: bool,
    /// A query to run right after the window was opened.
    startup_query: Option<String>,
    /// A self test: run this query once the window has settled and log how
    /// long the answer took to come back.
    selftest: Option<String>,
    /// When the self test should start.
    selftest_at: Option<Instant>,
    /// Set while a self test query is in flight.
    selftest_running: bool,
    /// Whether the window closes when it loses the focus.
    ///
    /// This is Flow Launcher's `HideWhenDeactivated`. It is turned off by the
    /// `--show` switch so that a screenshot can be taken while another
    /// application has the focus.
    hide_on_deactivate: bool,
    /// Whether the window has been in front since it was opened.
    ///
    /// A window that has just been opened may not be in front yet — that is
    /// normal. Once it has been in front, losing the front means the user
    /// went somewhere else.
    ever_focused: bool,
    /// When the window lost the focus, if it has.
    ///
    /// A focus that comes straight back is not a departure: the candidate
    /// window of the input method takes the focus for a moment when the text
    /// is committed, and a console program that is started takes it too. The
    /// window waits a moment before it goes away, and stays if the focus
    /// comes back.
    focus_lost_at: Option<Instant>,
    /// The icon in the notification area.
    tray: Option<crate::tray::Tray>,
    /// The last state of the input method request, for the log.
    ime_reported: Option<bool>,

    /// Whether the window should open on the settings panel.
    ///
    /// The flag is used up when the window is shown, so the hotkey always
    /// opens the search box.
    open_settings_on_show: bool,

    /// Whether the indexes were ready the last time they were looked at.
    ///
    /// They are built in the background, so a query that runs before they are
    /// there comes back short. When one of them shows up, the query runs
    /// again — otherwise the list would stay empty until the user typed
    /// another character.
    program_index_ready: bool,
    uwp_index_ready: bool,
    file_index_ready: bool,
    /// Whether the full-disk index was there the last time it was looked at.
    full_index_ready: bool,
    /// How many changes the index of the user's own folders has seen.
    ///
    /// The folders of the user are watched, so a file that was created a
    /// moment ago is in the index within milliseconds. When that happens, the
    /// query runs again — otherwise the file would only be found after the
    /// next keystroke.
    ///
    /// It does not run again: the watcher keeps the index up to date by
    /// itself, and the **next** search finds the new file. The search that
    /// is on the screen is not disturbed while the user is looking at it.
    live_version_seen: u64,
    /// The clock the moving part of the progress bar runs on.
    ///
    /// It is a monotonic clock: the wall clock only ticks every fifteen
    /// milliseconds, and the piece of the bar would jump in steps.
    frame_clock: Instant,
}

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
    fn watch_foreground(ctx: egui::Context, hwnd: usize, visible: Arc<AtomicBool>) {
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
    fn adapt_frame_rate(&mut self) {
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
    fn on_shown(&mut self) {
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
    fn show_from_tray(&mut self, settings: bool) {
        self.visible_flag.store(true, Ordering::SeqCst);
        self.foreground_refused = !crate::hotkey::force_foreground(self.hwnd);
        self.open_settings_on_show = settings;
        self.on_shown();
        self.ctx.request_repaint();
        crate::hotkey::wake(self.hwnd as usize);
    }

    /// Hide the launcher.
    fn hide(&mut self) {
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
    fn position_at_cursor(&self) {
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

    /// Hand the current query to the worker thread.
    ///
    /// The answer comes back through `apply_results`; the UI thread never
    /// blocks on a plugin.
    fn run_query(&mut self) {
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
    fn drain_query_results(&mut self) {
        while let Ok(response) = self.query_rx.try_recv() {
            // A late answer for an older query must not overwrite a newer one.
            if response.generation < self.query_generation {
                continue;
            }
            self.apply_results(response);
        }
    }

    /// Show the results of one query.
    fn apply_results(&mut self, response: QueryResponse) {
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
    fn fit_window_to_results(&mut self, ui: &Ui) {
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
    fn screen_limits(&self) -> (f32, f32) {
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

    fn open_selected(&mut self) {
        let Some(result) = self.results.get(self.selected).cloned() else {
            return;
        };
        self.open_result(&result);
    }

    /// Open one result, whatever route it was picked by.
    ///
    /// The keyboard and the mouse both end up here.
    fn open_result(&mut self, result: &LauncherResult) {
        let query = self.query_text.clone();
        let index = self.selected;

        // Remember the choice. The row is counted by its identity, so the
        // learning survives the list being in a different order the next
        // time — and a row that is picked often enough climbs up on its own.
        let item = launcher_core::history::item_key(
            &result.plugin_id,
            &result.title,
            &result.subtitle,
        );
        self.selection.record_pick(&query, item);
        self.selection.record(&query, index);
        self.history.record(&query);

        // The deeper details of a file — its index number, its streams, what
        // opens it, how many things a folder holds — are read **here**, when
        // the item is opened, and not while the list is being browsed.
        //
        // Browsing the list must not touch the disk: every row the pointer
        // crosses would open a file, and the machine would be busy the whole
        // time the pointer moves.
        if result.plugin_id == "file" {
            let path = if result.copy_text.is_empty() {
                result.subtitle.clone()
            } else {
                result.copy_text.clone()
            };
            if !path.is_empty() {
                // This queues the reading on a thread of its own; it does not
                // hold up the opening.
                self.details.get(std::path::Path::new(&path));
            }
        }

        // The icon of the program is taken at the same moment: the file is
        // being opened anyway, so that is the moment to read its icon — and
        // it is kept, so that the next time this program comes up, its row
        // has an icon without reading anything.
        if let launcher_core::result::ResultIcon::Path(icon) = &result.icon {
            self.icons.learn(icon);
        }

        let target = if result.copy_text.is_empty() {
            result.title.clone()
        } else {
            result.copy_text.clone()
        };

        match result.plugin_id.as_str() {
            "calculator" => {
                icons::shell_open_with("clip://", &target);
            }
            "shell" => {
                let mut cmd = String::from("cmd /c ");
                cmd.push_str(&target);
                icons::shell_run_console("cmd.exe", &cmd);
            }
            "process" => {
                if let Ok(pid) = target.parse::<u32>() {
                    if let Err(e) = plugins::processes::kill(pid) {
                        log::warn!("could not kill process {pid}: {e}");
                    }
                }
            }
            "sys" => {
                if let Some(action) = sys_action_for(&result.title) {
                    if let Err(e) = plugins::sys::execute(action) {
                        log::warn!("system command failed: {e}");
                    }
                }
            }
            "uwp" => {
                // The Store applications are not files: they are started by
                // their id, through the shell of the installed applications.
                let aumid = result.context_data.clone().unwrap_or_default();
                if aumid.is_empty() {
                    log::warn!("the Store application has no id: {:?}", result.title);
                } else {
                    icons::shell_open(&format!("shell:AppsFolder\\{aumid}"));
                }
            }
            "windows_settings" => {
                // The command of the row opens the page; half of them are
                // control-panel commands that the shell cannot resolve on
                // its own.
                if !plugins::windows_settings::open_settings(&target) {
                    log::warn!("could not open the settings page: {target}");
                }
            }
            _ => {
                // An external plugin acts on its own results.
                if self
                    .json_rpc_plugins
                    .iter()
                    .any(|plugin| plugin.id() == result.plugin_id)
                {
                    let hide = self.engine.execute(&result);
                    if hide {
                        self.hide();
                    }
                    return;
                }
                icons::shell_open(&target);
            }
        }

        self.hide();
    }

    /// Write the settings to disk.
    ///
    /// The settings panel changes the settings in place, so they are written
    /// when the panel is closed and when the theme changes.
    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&crate::paths::settings_file()) {
            log::error!("could not write the settings: {e}");
        }
        // The ranking learns with a threshold; a new one takes effect at once.
        self.selection.set_threshold(if self.settings.enable_smart_ranking {
            self.settings.pick_count_threshold
        } else {
            0
        });
        // The settings of the full-disk index are applied at once, so that a
        // changed exclusion or a changed volume shows up right away.
        self.file_plugin.configure(&self.settings);
        if !self.settings.enable_full_disk_index && self.file_plugin.has_full_index() {
            log::info!("the full-disk index was switched off");
            self.file_plugin.clear_cache();
        }
        // The order of the results is applied at once as well.
        self.engine.set_result_order(self.settings.result_order);
        // The number of results the list holds, too: the user asked for
        // more, and the list is scrolled through.
        self.engine.set_max_results(self.settings.max_results_to_show);
    }

    /// Copy the selected result to the clipboard.
    fn copy_selected(&mut self) {
        let Some(result) = self.results.get(self.selected) else {
            return;
        };
        let text = if result.copy_text.is_empty() {
            result.title.clone()
        } else {
            result.copy_text.clone()
        };
        if text.is_empty() {
            return;
        }
        self.ctx.copy_text(text.clone());
        log::info!("copied {text:?} to the clipboard");
    }

    /// Complete the query with the selected result.
    fn complete_selected(&mut self) {
        let Some(result) = self.results.get(self.selected) else {
            return;
        };
        let completion = if result.auto_complete_text.is_empty() {
            result.title.clone()
        } else {
            result.auto_complete_text.clone()
        };
        if completion.is_empty() || completion == self.query_text {
            return;
        }
        self.query_text = completion;
        self.run_query();
    }

    /// Move the selection by `delta`, wrapping around.
    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let len = self.results.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
        self.sync_scroll();
    }

    /// Keep the selected row inside the visible area.
    ///
    /// The height of the list is the one the window actually has. It used to
    /// be a fixed number, and a window that grew past it made the list jump
    /// up over the search box.
    ///
    /// The list itself is scrolled by `egui`, and the offset it ends up with
    /// is read back after every frame. This function only asks for a jump:
    /// writing the offset every frame would throw away what the user did with
    /// the wheel or with the scroll bar.
    fn sync_scroll(&mut self) {
        let row = row_pitch();
        let target = self.selected as f32 * row;
        let height = if self.list_height > 0.0 {
            self.list_height
        } else {
            320.0
        };
        let mut wanted = self.scroll_offset;
        if target < wanted {
            wanted = target;
        } else if target + row > wanted + height {
            wanted = target + row - height;
        }
        if (wanted - self.scroll_offset).abs() > 0.5 {
            self.scroll_offset = wanted;
            self.pending_scroll = Some(wanted);
        }
    }


    /// Remember that a dialog of the system was opened.
    ///
    /// The window keeps itself for a while, even without the focus: the
    /// dialog took the focus away from it, and it needs a moment to come up.
    fn system_dialog_opened(&mut self) {
        self.system_dialog_until = Some(Instant::now() + Duration::from_secs(8));
    }

    /// Give the preview pane a player for the video that is looked at.
    ///
    /// The player belongs to one file: when another file is looked at, it is
    /// let go — a decoder that keeps running is a decoder that keeps the
    /// disk busy.
    fn prepare_video(&mut self, content: &preview::PreviewContent) {
        let preview::PreviewContent::File { path, video, .. } = content else {
            self.video = None;
            self.pending_video = None;
            return;
        };
        if !*video {
            self.video = None;
            self.pending_video = None;
            return;
        }
        let same = self
            .video
            .as_ref()
            .map(|player| player.path() == path)
            .unwrap_or(false);
        if same {
            return;
        }
        // The player is not started right away: the pointer moves through
        // the list quickly, and every video it passes would start a
        // decoder of its own.
        let waiting = self
            .pending_video
            .as_ref()
            .map(|(other, _)| other == path)
            .unwrap_or(false);
        if !waiting {
            self.pending_video = Some((path.clone(), Instant::now()));
        }
    }

    /// Give the video that is being looked at its player.
    ///
    /// The player waits a moment: the pointer chooses the row it is over,
    /// and a row that is only passed over is not being looked at. Without
    /// the wait, a sweep through a list of videos starts a decoder for
    /// every one of them.
    fn start_pending_video(&mut self) {
        let Some((path, since)) = self.pending_video.clone() else {
            return;
        };
        if since.elapsed() < Duration::from_millis(150) {
            return;
        }
        self.pending_video = None;
        // What the details pane already found out about the file is handed
        // over, so that the player does not have to ask FFmpeg about it a
        // second time.
        let known = self
            .details
            .get_cached(std::path::Path::new(&path))
            .and_then(|details| details.video.clone());
        self.video = Some(video::VideoPlayer::new(
            std::path::Path::new(&path),
            self.ctx.clone(),
            known,
        ));
    }

    /// Apply a theme change coming from the settings overlay.
    fn apply_theme(&mut self, name: &str) {
        self.settings.theme = name.to_string();
        self.palette = palette_for(&self.settings);
        crate::theme::apply_visuals(&self.ctx, &self.palette);
    }
}

/// The distance from the top of one row to the top of the next.
///
/// The rows are laid out by `egui`, which leaves the gap of the layout
/// between two rows. The gap is set by hand in the list, so the pitch is the
/// same everywhere and the window can be made exactly as tall as the rows.
fn row_pitch() -> f32 {
    metrics::ROW_HEIGHT + metrics::ROW_GAP
}

/// The height of a list of `count` rows.
///
/// The last row has no gap under it, so the list is a little shorter than
/// `count` pitches.
fn list_content_height(count: usize) -> f32 {
    (count as f32 * row_pitch() - metrics::ROW_GAP).max(0.0)
}

/// The palette for the current settings.
fn palette_for(settings: &Settings) -> Palette {
    let theme = match settings.color_scheme {
        ColorScheme::Light => Theme::resolve("Win11"),
        ColorScheme::Dark => Theme::resolve("Midnight"),
        ColorScheme::System => {
            if system_prefers_dark() {
                Theme::resolve("Midnight")
            } else {
                Theme::resolve("Win11")
            }
        }
    };
    // A theme the user picked wins over the colour scheme. An empty name
    // means "none picked", and the colour scheme decides then.
    //
    // There used to be an exception for "BlurWhite" here, which was the
    // default — but that made the theme *unselectable*: picking it in the
    // settings did nothing at all.
    if !settings.theme.is_empty() {
        if let Some(named) = Theme::builtin(&settings.theme) {
            return Palette::from_theme(&named);
        }
    }
    Palette::from_theme(&theme)
}

/// Ask Windows whether the system prefers dark mode.
fn system_prefers_dark() -> bool {
    #[cfg(windows)]
    {
        crate::winapi::registry_light_theme_prefers_dark()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// The hotkey combinations to try, in order of preference.
fn hotkey_candidates(primary: &str) -> Vec<String> {
    let mut candidates = vec![primary.trim().to_string()];
    for fallback in [
        "Ctrl + Space",
        "Ctrl + Alt + Space",
        "Alt + K",
        "Ctrl + J",
    ] {
        if !candidates
            .iter()
            .any(|c| c.eq_ignore_ascii_case(fallback))
        {
            candidates.push(fallback.to_string());
        }
    }
    candidates
}

/// Map a system command's display name back to its action.
fn sys_action_for(title: &str) -> Option<plugins::sys::Action> {
    use plugins::sys::Action;
    Some(match title {
        "Shutdown" => Action::Shutdown,
        "Restart" => Action::Restart,
        "Restart With Advanced Boot Options" => Action::RestartAdvanced,
        "Log Off / Sign Out" => Action::LogOff,
        "Lock" => Action::Lock,
        "Sleep" => Action::Sleep,
        "Hibernate" => Action::Hibernate,
        "Empty Recycle Bin" => Action::EmptyRecycleBin,
        "Open Recycle Bin" => Action::OpenRecycleBin,
        "Windows Settings Index" => Action::IndexOption,
        "Reload Plugin Data" => Action::ReloadPluginData,
        "Open Log Location" => Action::OpenLogLocation,
        "Open Data Folder" => Action::OpenDataFolder,
        "Settings" => Action::OpenSettings,
        "Exit" => Action::Exit,
        _ => return None,
    })
}

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

/// The frame of the menu of a row.
///
/// The menu is a part of the launcher, so it looks like the launcher: the
/// card of the window, the hairline around it, and the round corners of the
/// design.
fn menu_frame(palette: &crate::theme::Palette, ctx: &egui::Context) -> egui::Frame {
    let style = ctx.style_of(ctx.theme());
    egui::Frame::popup(&style)
        .fill(palette.surface)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(metrics::MENU_RADIUS))
        .inner_margin(egui::Margin::symmetric(
            metrics::MENU_PADDING_X as i8,
            metrics::MENU_PADDING_Y as i8,
        ))
        .shadow(egui::Shadow {
            offset: [0, 4],
            blur: 18,
            spread: 0,
            color: egui::Color32::from_black_alpha(if palette.kind == launcher_core::theme::ThemeKind::Dark
            {
                110
            } else {
                46
            }),
        })
}

/// One entry of the menu.
///
/// The entry is not a button: a button has a frame of its own, and a menu
/// entry should not. The row of the result list is the same shape, so the
/// menu looks like the window it belongs to.
fn menu_item(ui: &mut egui::Ui, palette: &crate::theme::Palette, text: &str) -> egui::Response {
    // The highlight of the entry is the one of the selected row, so that the
    // menu and the list look like one thing.
    ui.visuals_mut().selection.bg_fill = palette.accent_soft;
    let text = egui::RichText::new(text)
        .size(metrics::TEXT_BODY)
        .color(palette.text);
    ui.spacing_mut().item_spacing.y = metrics::MENU_ITEM_GAP;
    ui.selectable_label(false, text)
}

/// A thin line between two groups of entries.
fn menu_separator(ui: &mut egui::Ui, palette: &crate::theme::Palette) {
    // The line needs room of its own, or it is drawn over the text.
    ui.add_space(metrics::MENU_ITEM_GAP + 2.0);
    let rect = ui.available_rect_before_wrap();
    ui.painter().hline(
        rect.x_range(),
        rect.top(),
        egui::Stroke::new(1.0, palette.border),
    );
    ui.add_space(metrics::MENU_ITEM_GAP + 2.0);
}

/// What the menu of a row was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowAction {
    Open,
    /// Let the user pick the program the row is opened with.
    OpenWith,
    /// Show the file in the file manager.
    OpenFolder,
    /// Put the path of the row on the clipboard.
    CopyPath,
    /// Put the name of the row on the clipboard.
    CopyName,
    /// Give the row another name.
    Rename,
    /// Throw the row into the bin.
    Delete,
    /// Show the properties of the row.
    Properties,
}

/// The path of a row, when it has one.
///
/// A file has its path in the second line; a program has it in the text that
/// is opened. The Store applications have no path at all.
fn result_path(result: &LauncherResult) -> Option<String> {
    if result.plugin_id == "file" {
        if result.subtitle.is_empty() {
            None
        } else {
            Some(result.subtitle.clone())
        }
    } else if result.copy_text.contains('\\') {
        Some(result.copy_text.clone())
    } else {
        None
    }
}

/// Show a file in the file manager, with the file itself selected.
fn open_folder_of(path: &str) {
    icons::shell_open_with("explorer.exe", &format!("/select,\"{path}\""));
}

/// Give a file another name.
///
/// The change is noticed by the index of the file system through the change
/// journal, so the file shows up under its new name within a few seconds.
fn rename_file(path: &str, name: &str) {
    if name.is_empty() {
        return;
    }
    let old = std::path::Path::new(path);
    let Some(folder) = old.parent() else {
        return;
    };
    let new = folder.join(name);
    if new == old {
        return;
    }
    match std::fs::rename(old, &new) {
        Ok(()) => log::info!("the file was renamed: {path} -> {}", new.display()),
        Err(e) => log::warn!("the file could not be renamed: {e}"),
    }
}

/// Draw one result row.
#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &mut Ui,
    rect: Rect,
    result: &LauncherResult,
    palette: &Palette,
    icons: &mut IconCache,
    byte_icons: &icons::ByteIconCache,
    ctx: &egui::Context,
    opacity: f32,
    title_size: f32,
    subtitle_size: f32,
    title_height: f32,
    subtitle_height: f32,
    _index: usize,
    // The hit response of the row, made by the caller (it knows the list
    // and clamps the hit rectangle to it).
    response: Response,
) -> Response {
    let mut painter = ui.painter().clone();
    // Clip to the row so long paths cannot bleed past the card.
    //
    // The clip is *narrowed*, not replaced: `egui` draws one row more than
    // the list can show, and a row that replaces the clip would walk out of
    // the list, over the footer and out of the window.
    painter.shrink_clip_rect(rect);

    // The row is a little shorter than the slot it sits in, so that two rows
    // never touch. The space is taken from the top and from the bottom
    // equally.
    let row_rect = rect.shrink2(Vec2::new(0.0, metrics::ROW_GAP * 0.5));

    // ---- the text block -------------------------------------------------
    //
    // The highlight of the selected row is **not** drawn here: it is one
    // rectangle of its own, painted before the rows, at a fractional row
    // position — the selection glides between rows instead of teleporting
    // (see the list drawing, "the highlight of the selected row").
    //
    // The two lines are placed with the real metrics of the font, so that the
    // gap between the title and the subtitle stays the same whatever font
    // size the user picked. A fixed offset would let the two lines collide
    // as soon as the text grew.
    let title_font = FontId::proportional(title_size);
    let subtitle_font = FontId::proportional(subtitle_size);
    let has_subtitle = !result.subtitle.is_empty();

    // The height of the text is asked of the font collection — **once for
    // the whole list**, not once per row (the caller passes it in). The
    // collection is written to whenever a new glyph shows up, and the write
    // lock is the one thing the drawing must not wait for.
    let text_height = title_height
        + if has_subtitle {
            metrics::TITLE_GAP + subtitle_height
        } else {
            0.0
        };
    let block_top = row_rect.center().y - text_height * 0.5;
    let title_top = block_top;
    let subtitle_top = block_top + title_height + metrics::TITLE_GAP;

    // Icon.
    let icon_size = metrics::ICON_SIZE;
    let icon_rect = Rect::from_center_size(
        egui::pos2(
            row_rect.left() + metrics::ROW_PAD_X + icon_size * 0.5,
            row_rect.center().y,
        ),
        Vec2::splat(icon_size),
    );

    // A row that has no icon of its own — a web search, a bookmark, a system
    // command — gets a badge with the first character of its title, so the
    // list looks even instead of full of holes.
    let mut icon_drawn = false;
    match &result.icon {
        launcher_core::result::ResultIcon::Path(path) => {
            // The icon is only drawn when it is already in memory. Reading it
            // from the disk while the user is looking at the list is exactly
            // what made the launcher slow — the icon is read the one time the
            // program is opened, and kept for the next time.
            if let Some(handle) = icons.texture(path, ctx) {
                painter.image(
                    handle.id(),
                    icon_rect,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                icon_drawn = true;
            }
        }
        launcher_core::result::ResultIcon::Bytes(bytes) => {
            // The Store applications hand their icon over as image data.
            if let Some(handle) = byte_icons.texture(bytes, ctx) {
                painter.image(
                    handle.id(),
                    icon_rect,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                icon_drawn = true;
            }
        }
        launcher_core::result::ResultIcon::Glyph { glyph, .. } => {
            painter.text(
                icon_rect.center(),
                Align2::CENTER_CENTER,
                glyph,
                crate::fonts::icon_font_id(icon_size * 0.8),
                palette.glyph.gamma_multiply(opacity),
            );
            icon_drawn = true;
        }
        launcher_core::result::ResultIcon::None => {}
    }

    if !icon_drawn {
        draw_badge(&painter, icon_rect, &result.title, palette, opacity);
    }

    // Title and subtitle.
    let text_left = icon_rect.right() + metrics::ICON_GAP;
    // The row is used to its full width now that the score badge is gone.
    let available = (row_rect.right() - metrics::ROW_PAD_RIGHT - text_left).max(40.0);

    // The title is drawn in two runs so the matched part can be highlighted.
    let title = &result.title;
    let highlight = result
        .title_highlight
        .iter()
        .copied()
        .find(|(s, e)| *e > *s);

    let text_color = palette.text.gamma_multiply(opacity);
    let matched_color = palette.accent.gamma_multiply(opacity);

    match highlight {
        Some((start, end)) if start < title.len() && end <= title.len() => {
            let before = &title[..start];
            let matched = &title[start..end];
            let after = &title[end..];

            // The matched part is the interesting one, so it is kept on screen
            // even when the text before it is long.
            //
            // Every piece of text is laid out **once**, and the layout is told
            // how much room it has — so the layout itself cuts the text and
            // puts the ellipsis at the end.
            let before_galley = fitted_galley(&painter, before, &title_font, text_color, available * 0.6);
            let before_w = before_galley.size().x;
            let matched_galley = fitted_galley(&painter, matched, &title_font, matched_color, available);
            let matched_w = matched_galley.size().x;

            // Whatever is left of the row belongs to the rest of the title.
            let rest = available - before_w - matched_w;
            if rest > 8.0 {
                let after_galley = fitted_galley(&painter, after, &title_font, text_color, rest);
                painter.galley(
                    egui::pos2(text_left + before_w + matched_w, title_top),
                    after_galley,
                    text_color,
                );
            }

            painter.galley(
                egui::pos2(text_left, title_top),
                before_galley,
                text_color,
            );
            painter.galley(
                egui::pos2(text_left + before_w, title_top),
                matched_galley,
                matched_color,
            );
        }
        _ => {
            let galley = fitted_galley(&painter, title, &title_font, text_color, available);
            painter.galley(egui::pos2(text_left, title_top), galley, text_color);
        }
    }

    if has_subtitle {
        let subtitle_color = palette.text_dim.gamma_multiply(opacity);
        let galley = fitted_galley(&painter, &result.subtitle, &subtitle_font, subtitle_color, available);
        painter.galley(egui::pos2(text_left, subtitle_top), galley, subtitle_color);
    }

    // The score is an internal number and says nothing to the user, so it is
    // not drawn. What *is* worth showing is the full text of a row that had
    // to be shortened to fit.
    //
    // The hit rectangle of the row — the whole slot, half a gap beyond the
    // row on either side and clamped to the bounds of the list — is made by
    // the caller, where the list is known. See there for why the gaps
    // belong to the rows and why nothing reaches past the edge of the list.
    // The tip — the whole title and path of the row — is not drawn here any
    // more: it is one box of its own, painted after the list with a fade
    // that is paced by the launcher itself. The tip of `egui` rides on an
    // `Area`, and an `Area` fades in with a bare `request_repaint` for every
    // frame — on a machine whose frames are expensive, a stutter every time
    // a tip appears (see the list drawing, "the tip of the resting row").
    // The drawing hands back the response it was given, unchanged — the
    // caller made it and the caller reads it.
    response
}

/// Group a number with thousands separators: 1234567 becomes "1,234,567".
pub fn format_thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// What the index is doing, for the settings pane.
///
/// The pane cannot reach the index itself, so the window looks at it and
/// hands the state over.
fn index_status(
    file_plugin: &plugins::files::FilePlugin,
) -> crate::settings_view::IndexStatus {
    let progress = file_plugin.progress();
    let problem = match file_plugin.problem() {
        plugins::files::IndexProblem::None => None,
        plugins::files::IndexProblem::NeedsAdministrator => {
            Some("全盘索引需要管理员权限才能建立。".to_string())
        }
        plugins::files::IndexProblem::NoNtfsVolume => {
            Some("本机没有 NTFS 卷，无法建立全盘索引。".to_string())
        }
        plugins::files::IndexProblem::Failed(reason) => Some(format!("全盘索引不可用：{reason}")),
    };
    crate::settings_view::IndexStatus {
        building: file_plugin.is_building(),
        entries: progress.entries,
        fraction: progress.fraction,
        phase: progress.phase,
        problem,
        elevated: launcher_index::usn::is_elevated(),
        live_entries: file_plugin.live_len(),
        full_entries: file_plugin.full_len(),
    }
}

/// The state of the file index, shown in the footer.
///
/// The footer normally carries nothing but the key hints — a sentence about
/// the index is of little use to the person who is typing. While the index is
/// being built, the bar and the count take its place.
#[derive(Debug, Clone, PartialEq)]
enum FileIndexState {
    /// Nothing to say: the hints are enough.
    Ready,
    /// The index is being built.
    Building {
        /// How many entries there are so far.
        entries: usize,
        /// How far the work is done, between 0 and 1.
        fraction: Option<f32>,
        /// What is being read right now.
        phase: String,
    },
    /// The folders of the user are being walked through.
    Scanning(usize),
}

/// Where the moving part of the progress bar is right now.
///
/// The clock is a monotonic one: the wall clock only ticks every fifteen
/// milliseconds, and the piece of the bar would jump in steps.
fn animation_phase(clock: Instant) -> f32 {
    (clock.elapsed().as_secs_f32() % 1.6) / 1.6
}

/// Draw the footer with key hints and the clock.
fn draw_footer(
    ui: &mut Ui,
    rect: Rect,
    palette: &Palette,
    opacity: f32,
    state: FileIndexState,
    paging: bool,
    phase: f32,
) {
    let mut painter = ui.painter().clone();
    painter.set_clip_rect(ui.clip_rect());

    // The footer sits on top of the card, so it hides the rows that may run
    // past the end of the list — `egui` draws one row more than fits.
    painter.rect_filled(
        rect,
        CornerRadius::ZERO,
        palette.surface.gamma_multiply(opacity),
    );

    // A hairline separates the footer from the list, so that the hints do not
    // look like they belong to the last row.
    painter.line_segment(
        [
            egui::pos2(rect.left(), rect.top()),
            egui::pos2(rect.right(), rect.top()),
        ],
        Stroke::new(1.0, palette.border.gamma_multiply(opacity * 0.8)),
    );

    // The keys are the same for every state; only the paging hint comes and
    // goes, because a list that fits on one screen has nothing to page.
    let keys = if paging {
        "↑ ↓  上下选择    PgUp/PgDn  翻页    ↵  打开    esc  关闭"
    } else {
        "↑ ↓  上下选择    ↵  打开    esc  关闭"
    };
    // The bar of the progress is drawn to the left of the text, so the text
    // starts further to the right while the index is being built.
    let mut bar: Option<(Option<f32>, String)> = None;
    let hints = match &state {
        FileIndexState::Building {
            entries,
            fraction,
            phase,
        } => {
            let count = format_thousands(*entries);
            let phase = if phase.is_empty() {
                "正在建立索引".to_string()
            } else {
                phase.clone()
            };
            bar = Some((*fraction, phase.clone()));
            format!("{phase}… {count} 项    {keys}")
        }
        FileIndexState::Scanning(count) => {
            let count = format_thousands(*count);
            bar = Some((None, "正在扫描用户目录".to_string()));
            format!("正在扫描用户目录… {count} 项    {keys}")
        }
        // Nothing but the hints: a sentence about the index would only be in
        // the way of the person who is typing.
        FileIndexState::Ready => keys.to_string(),
    };

    // The progress bar.
    //
    // A bar that knows how much of the work is done is filled up from the
    // left; a bar that does not know moves a piece back and forth, so that
    // it is clear that something is still running.
    let text_left = match bar {
        Some((fraction, _)) => {
            let width = 132.0;
            let height = 5.0;
            let left = rect.left() + 14.0;
            let top = rect.center().y - height / 2.0;
            let frame = Rect::from_min_size(egui::pos2(left, top), egui::vec2(width, height));
            painter.rect_filled(
                frame,
                CornerRadius::same(3),
                palette.border.gamma_multiply(opacity * 0.9),
            );
            match fraction {
                Some(fraction) => {
                    let filled = (width * fraction.clamp(0.0, 1.0)).max(2.0);
                    painter.rect_filled(
                        Rect::from_min_size(frame.min, egui::vec2(filled, height)),
                        CornerRadius::same(3),
                        palette.accent.gamma_multiply(opacity),
                    );
                }
                None => {
                    // The piece that moves. Its place comes from the clock of
                    // the window, so that it moves on its own.
                    let piece = 34.0;
                    let travel = (width - piece).max(1.0);
                    let offset = (phase * 2.0).rem_euclid(1.0) * travel;
                    painter.rect_filled(
                        Rect::from_min_size(
                            egui::pos2(frame.left() + offset, frame.top()),
                            egui::vec2(piece, height),
                        ),
                        CornerRadius::same(3),
                        palette.accent.gamma_multiply(opacity),
                    );
                }
            }
            frame.right() + 14.0
        }
        None => rect.left(),
    };

    painter.text(
        egui::pos2(text_left, rect.center().y),
        Align2::LEFT_CENTER,
        hints,
        FontId::proportional(12.0),
        palette.text_faint.gamma_multiply(opacity),
    );

    // Clock on the right.
    let now = chrono_like_now();
    painter.text(
        egui::pos2(rect.right(), rect.center().y),
        Align2::RIGHT_CENTER,
        now,
        FontId::proportional(12.0),
        palette.text_faint.gamma_multiply(opacity),
    );
}

/// Handle the keyboard shortcuts.
fn handle_keyboard(ui: &mut Ui, app: &mut LauncherApp) {
    let ctx = ui.ctx().clone();
    ctx.input(|i| {
        for event in &i.events {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            {
                // This is written at the `debug` level on purpose.
                //
                // A key that is held down makes `egui` repeat it, and then this
                // line would be written thirty times a second — the log would
                // grow by megabytes an hour for nothing. The level can be
                // turned up in the settings when the keys are being chased.
                log::debug!("key event: {key:?} modifiers={modifiers:?}");
                match key {
                    egui::Key::ArrowDown => app.move_selection(1),
                    egui::Key::ArrowUp => app.move_selection(-1),
                    egui::Key::PageDown => app.move_selection(8),
                    egui::Key::PageUp => app.move_selection(-8),
                    egui::Key::Enter => app.open_selected(),
                    egui::Key::Escape => app.hide(),
                    egui::Key::Home => {
                        app.selected = 0;
                        app.sync_scroll();
                    }
                    egui::Key::End => {
                        app.selected = app.results.len().saturating_sub(1);
                        app.sync_scroll();
                    }
                    _ => {
                        if modifiers.ctrl {
                            match key {
                                egui::Key::N => app.move_selection(1),
                                egui::Key::P => app.move_selection(-1),
                                egui::Key::M => app.hide(),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    });
}

/// Draw the letter badge that stands in for a missing icon.
///
/// The badge is a rounded square in a colour that belongs to the title, with
/// the first letter of the title on it. The colour is derived from the
/// title, so a program always looks the same, and it stays close to the
/// theme's accent colour so the list does not turn into a patchwork.
fn draw_badge(
    painter: &egui::Painter,
    rect: Rect,
    title: &str,
    palette: &Palette,
    opacity: f32,
) {
    let color = crate::theme::badge_color(palette, title);
    let alpha = (52.0 * opacity.clamp(0.0, 1.0)) as u8;

    // The badge is a little smaller than the icon it stands in for, so that
    // the list keeps the same rhythm whether a row has an icon or not.
    let badge_rect = Rect::from_center_size(rect.center(), Vec2::splat(metrics::BADGE_SIZE));
    painter.rect_filled(
        badge_rect,
        CornerRadius::same(8),
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
    );

    let letter = crate::theme::badge_letter(title);
    painter.text(
        badge_rect.center(),
        Align2::CENTER_CENTER,
        letter,
        FontId::proportional(badge_rect.height() * 0.5),
        color.gamma_multiply(opacity),
    );
}

/// Lay a piece of text out so that it fits into `max_width`.
///
/// The text is laid out **once**. The layout is told how much room the text
/// may take, and it cuts the text itself and puts the ellipsis at the end.
///
/// The old way did a binary search over the text and laid out a piece of it
/// again and again — seven times for a long path — and **every time from
/// scratch**: `egui` throws away the layouts that were not drawn, and those
/// seven pieces are never drawn. So the whole search was repeated on every
/// frame, for every row: the window spent its time shaping fonts instead of
/// showing pictures.
///
/// The layout that comes out of here **is** the one that is drawn, so `egui`
/// keeps it, and the next frame finds it again without doing any work.
fn fitted_galley(
    painter: &egui::Painter,
    text: &str,
    font: &FontId,
    color: Color32,
    max_width: f32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font.clone(), color);
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    painter.layout_job(job)
}

/// Draw a soft veil over the bottom of the result list.
///
/// A scrolling list is cut off in the middle of a row, and a row that is cut
/// in half looks like a mistake. The veil lets the last row fade out
/// instead, which reads as "there is more below".
fn draw_bottom_veil(painter: &egui::Painter, list: Rect, palette: &Palette, opacity: f32) {
    const STEPS: usize = 16;
    const HEIGHT: f32 = 26.0;

    if list.height() < HEIGHT {
        return;
    }

    let top = list.bottom() - HEIGHT;
    let step = HEIGHT / STEPS as f32;
    let mut painter = painter.clone();
    painter.shrink_clip_rect(list);

    for index in 0..STEPS {
        // The alpha grows towards the bottom, so the row dissolves into the
        // card instead of being cut off.
        let fraction = (index + 1) as f32 / STEPS as f32;
        let alpha = (fraction * fraction * 255.0 * opacity.clamp(0.0, 1.0)) as u8;
        let rect = Rect::from_min_size(
            egui::pos2(list.left(), top + step * index as f32),
            Vec2::new(list.width(), step + 0.5),
        );
        painter.rect_filled(
            rect,
            CornerRadius::ZERO,
            Color32::from_rgba_unmultiplied(
                palette.surface.r(),
                palette.surface.g(),
                palette.surface.b(),
                alpha,
            ),
        );
    }
}

/// Format the current local time.
fn chrono_like_now() -> String {
    #[cfg(windows)]
    {
        let mut st = winapi::SYSTEMTIME {
            wYear: 0,
            wMonth: 0,
            wDayOfWeek: 0,
            wDay: 0,
            wHour: 0,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        };
        unsafe { winapi::GetLocalTime(&mut st) };
        format!("{:02}:{:02}", st.wHour, st.wMinute)
    }
    #[cfg(not(windows))]
    {
        String::new()
    }
}

/// Get the native window handle from the creation context.
fn native_window_handle(cc: &eframe::CreationContext<'_>) -> winapi::HWND {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    match cc.window_handle() {
        Ok(handle) => match handle.as_raw() {
            RawWindowHandle::Win32(win32) => win32.hwnd.get() as winapi::HWND,
            _ => std::ptr::null_mut(),
        },
        Err(_) => std::ptr::null_mut(),
    }
}

/// Keep the settings-view types reachable from outside this module.
pub use crate::settings_view::SettingsSection as Section;

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_core::theme::ThemeKind;

    #[test]
    fn a_named_theme_wins_over_the_colour_scheme() {
        let mut settings = Settings::default();
        settings.color_scheme = ColorScheme::Light;
        settings.theme = "Midnight".into();
        assert_eq!(palette_for(&settings).kind, ThemeKind::Dark);
    }

    #[test]
    fn every_built_in_theme_can_be_picked() {
        // "BlurWhite" used to be excluded, which made it impossible to
        // select — the settings showed it, but picking it did nothing.
        for name in Theme::builtin_names() {
            let mut settings = Settings::default();
            settings.theme = name.clone();
            let palette = palette_for(&settings);
            assert!(palette.kind == ThemeKind::Dark || palette.kind == ThemeKind::Light);
        }
    }

    #[test]
    fn an_empty_theme_leaves_the_colour_scheme_in_charge() {
        let mut settings = Settings::default();
        settings.color_scheme = ColorScheme::Dark;
        settings.theme = String::new();
        assert_eq!(palette_for(&settings).kind, ThemeKind::Dark);

        settings.color_scheme = ColorScheme::Light;
        assert_eq!(palette_for(&settings).kind, ThemeKind::Light);
    }

    #[test]
    fn the_settings_are_read_and_written_in_the_same_place() {
        // They used to be read from one file and written to another, so every
        // change was lost on the next start.
        let path = crate::paths::settings_file();
        assert_eq!(path, Settings::default_path());
        assert_eq!(path.file_name().unwrap(), "Settings.json");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "Settings");
    }
}
