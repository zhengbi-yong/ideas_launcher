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

// The one impl block is split across child modules by concern; every one
// of them sees the private fields, which is what keeps the split a pure
// code movement.
mod actions;
mod construct;
mod eframe_impl;
mod lifecycle;
mod query;

/// The distance from the top of one row to the top of the next.
///
/// The rows are laid out by `egui`, which leaves the gap of the layout
/// between two rows. The gap is set by hand in the list, so the pitch is the
/// same everywhere and the window can be made exactly as tall as the rows.
pub(super) fn row_pitch() -> f32 {
    metrics::ROW_HEIGHT + metrics::ROW_GAP
}

/// The height of a list of `count` rows.
///
/// The last row has no gap under it, so the list is a little shorter than
/// `count` pitches.
pub(super) fn list_content_height(count: usize) -> f32 {
    (count as f32 * row_pitch() - metrics::ROW_GAP).max(0.0)
}

/// The palette for the current settings.
pub(super) fn palette_for(settings: &Settings) -> Palette {
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
pub(super) fn system_prefers_dark() -> bool {
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
pub(super) fn hotkey_candidates(primary: &str) -> Vec<String> {
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
pub(super) fn sys_action_for(title: &str) -> Option<plugins::sys::Action> {
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
