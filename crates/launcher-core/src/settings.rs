//! Persistent application settings.
//!
//! The field set mirrors the `Settings.json` schema observed in
//! `%APPDATA%\FlowLauncher\Settings\Settings.json` on the analysed machine
//! (≈90 top-level keys). Keys that Flow Launcher writes are kept with the
//! same names so an existing configuration can be adopted; every field has a
//! default so a fresh install works without a config file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::result::SearchPrecision;
use crate::result::ResultOrder;

/// How many results the list holds by default.
///
/// The window shows a handful of rows, but the list is scrolled through — so
/// the results are not cut off at the height of the window. A file that is
/// further down has to be findable.
pub const DEFAULT_MAX_RESULTS: usize = 10_000;

/// Where the window appears on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SearchWindowScreen {
    #[default]
    Cursor,
    Primary,
    Custom,
}

/// How the window is aligned within its screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SearchWindowAlign {
    #[default]
    Center,
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Light/dark/system colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ColorScheme {
    #[default]
    System,
    Light,
    Dark,
}

/// Animation speed presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AnimationSpeed {
    Slow,
    #[default]
    Medium,
    Fast,
    NoAnimation,
}

/// How the window background is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum BackdropType {
    #[default]
    None,
    Acrylic,
    Mica,
    Blur,
}

/// Double-pinyin scheme used for Chinese input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum DoublePinyinSchema {
    #[default]
    XiaoHe,
    Xiaohe,
    Ziranma,
    Ziranma2,
}

/// A custom file-manager entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomExplorer {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "FileArgument", default)]
    pub file_argument: String,
    #[serde(rename = "DirectoryArgument", default)]
    pub directory_argument: String,
    #[serde(rename = "Editable", default)]
    pub editable: bool,
}

/// A custom browser entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomBrowser {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "PrivateArg", default)]
    pub private_arg: String,
    #[serde(rename = "EnablePrivate", default)]
    pub enable_private: bool,
    #[serde(rename = "OpenInTab", default)]
    pub open_in_tab: bool,
    #[serde(rename = "Editable", default)]
    pub editable: bool,
}

/// Proxy configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HttpProxy {
    #[serde(rename = "Enabled", default)]
    pub enabled: bool,
    #[serde(rename = "Server", default)]
    pub server: Option<String>,
    #[serde(rename = "Port", default)]
    pub port: u16,
    #[serde(rename = "UserName", default)]
    pub user_name: Option<String>,
    #[serde(rename = "Password", default)]
    pub password: Option<String>,
}

/// Per-plugin overrides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PluginSettings {
    #[serde(rename = "ID", default)]
    pub id: String,
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Version", default)]
    pub version: String,
    #[serde(rename = "ActionKeywords", default)]
    pub action_keywords: Vec<String>,
    #[serde(rename = "Priority", default)]
    pub priority: i32,
    #[serde(rename = "SearchDelayTime", default)]
    pub search_delay_time: Option<u64>,
    #[serde(rename = "Disabled", default)]
    pub disabled: bool,
    #[serde(rename = "HomeDisabled", default)]
    pub home_disabled: bool,
}

/// How the file results are sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub enum FileSort {
    /// By how well the entry matches the query.
    #[default]
    Relevance,
    /// By the file name.
    Name,
    /// By the size, largest first.
    Size,
    /// By the time of the last change, newest first.
    Modified,
    /// By the full path.
    Path,
}

impl FileSort {
    /// The name shown in the settings.
    pub fn label(self) -> &'static str {
        match self {
            FileSort::Relevance => "相关度",
            FileSort::Name => "文件名",
            FileSort::Size => "大小",
            FileSort::Modified => "修改时间",
            FileSort::Path => "完整路径",
        }
    }

    /// Every way of sorting, in the order they are offered.
    pub fn all() -> [FileSort; 5] {
        [
            FileSort::Relevance,
            FileSort::Name,
            FileSort::Size,
            FileSort::Modified,
            FileSort::Path,
        ]
    }
}

/// The complete settings document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Settings {
    // ---- hotkeys -------------------------------------------------------
    pub hotkey: String,
    pub open_result_modifiers: String,
    pub preview_hotkey: String,
    pub auto_complete_hotkey: String,
    pub auto_complete_hotkey2: String,
    pub select_next_item_hotkey: String,
    pub select_next_item_hotkey2: String,
    pub select_prev_item_hotkey: String,
    pub select_prev_item_hotkey2: String,
    pub select_next_page_hotkey: String,
    pub select_prev_page_hotkey: String,
    pub open_context_menu_hotkey: String,
    pub setting_window_hotkey: String,
    pub open_history_hotkey: String,
    pub cycle_history_up_hotkey: String,
    pub cycle_history_down_hotkey: String,
    pub dialog_jump_hotkey: String,

    // ---- appearance ----------------------------------------------------
    pub color_scheme: ColorScheme,
    pub theme: String,
    pub use_drop_shadow_effect: bool,
    pub backdrop_type: BackdropType,
    pub use_glyph_icons: bool,
    pub use_animation: bool,
    pub use_sound: bool,
    pub sound_volume: u8,
    pub show_badges: bool,
    pub show_badges_global_only: bool,
    pub animation_speed: AnimationSpeed,
    pub custom_animation_length: u64,
    pub window_size: u32,
    pub window_height_size: u32,
    pub item_height_size: u32,
    pub query_box_font_size: u32,
    pub result_item_font_size: u32,
    pub result_sub_item_font_size: u32,
    pub query_box_font: String,
    pub result_font: String,
    pub result_sub_font: String,
    pub setting_window_font: String,
    pub use_clock: bool,
    pub use_date: bool,
    pub time_format: String,
    pub date_format: String,
    /// Whether the tray icon stays in the open notification area.
    ///
    /// Windows drops a new icon into the hidden part of the tray, where
    /// nobody finds it. The launcher can ask the shell to keep it in the
    /// open instead — but the default is the other way round, so that the
    /// task bar stays clean.
    pub always_show_tray_icon: bool,

    // ---- behaviour -----------------------------------------------------
    pub language: String,
    pub show_placeholder: bool,
    /// Whether the preview pane reads the file to show a picture or a video.
    ///
    /// Both mean **reading the whole file from the disk**, so they are off
    /// unless the user asks for them: looking through a list of results
    /// must not make the disk busy.
    pub preview_media: bool,
    pub placeholder_text: String,
    pub show_home_page: bool,
    pub show_history_results_for_home_page: bool,
    pub max_history_results_to_show_for_home_page: usize,
    /// How many results the list holds.
    ///
    /// The window is as tall as the results need, so this number decides how
    /// tall the window gets. Twenty rows fill a usual screen without leaving
    /// a hole under a short list.
    pub max_results_to_show: usize,
    pub keep_max_results: bool,
    pub auto_restart_after_changing: bool,
    pub show_unknown_source_warning: bool,
    pub auto_update_plugins: bool,
    pub auto_updates: bool,
    pub custom_plugin_hotkeys: BTreeMap<String, String>,
    pub custom_shortcuts: BTreeMap<String, String>,

    // ---- search --------------------------------------------------------
    pub search_query_results_with_delay: bool,
    pub search_delay_time: u64,
    pub search_window_screen: SearchWindowScreen,
    pub search_window_align: SearchWindowAlign,
    pub custom_screen_number: u32,
    pub query_search_precision: SearchPrecision,
    pub should_use_pinyin: bool,
    pub use_double_pinyin: bool,
    pub double_pinyin_schema: DoublePinyinSchema,
    pub always_start_en: bool,
    pub always_preview: bool,
    pub ignore_hotkeys_on_fullscreen: bool,

    // ---- smart ranking --------------------------------------------------
    /// Learn from what the user picks and move those rows up the list.
    pub enable_smart_ranking: bool,
    /// How often a row has to be picked before it is moved up.
    pub pick_count_threshold: u32,

    // ---- dialog jump ---------------------------------------------------
    pub enable_dialog_jump: bool,
    pub auto_dialog_jump: bool,
    pub show_dialog_jump_window: bool,

    // ---- window placement ----------------------------------------------
    pub window_left: f64,
    pub window_top: f64,
    pub custom_window_left: f64,
    pub custom_window_top: f64,
    pub previous_screen_width: u32,
    pub previous_screen_height: u32,
    pub previous_dpi_x: u32,
    pub previous_dpi_y: u32,
    pub setting_window_width: u32,
    pub setting_window_height: u32,
    pub setting_window_top: f64,
    pub setting_window_left: f64,

    // ---- full disk index -------------------------------------------------
    /// Whether the whole file system is indexed.
    ///
    /// Reading the master file table needs administrative privileges, so the
    /// launcher asks for them and falls back to the folder walk when it does
    /// not get them.
    pub enable_full_disk_index: bool,
    /// Whether the launcher starts with administrative privileges.
    ///
    /// Reading the NTFS master file table needs them, and that is the index
    /// that sees every file on the machine. So a copy that does not have
    /// them starts one that does — and the user is asked to say yes once.
    ///
    /// Turning this off keeps the launcher an ordinary program: it then
    /// searches the folders of the user and nothing else.
    pub run_as_administrator: bool,
    /// The volumes that are indexed, by their letter (`C`).
    ///
    /// Empty means every NTFS volume.
    pub indexed_volumes: Vec<String>,
    /// Folders that are never indexed, by their full path.
    pub excluded_paths: Vec<String>,
    /// File extensions that are never indexed, without the dot.
    pub excluded_extensions: Vec<String>,
    /// How the file results are sorted.
    pub file_sort: FileSort,
    /// How long a saved table is used before it is read again, in hours.
    ///
    /// Zero means that the table is never read again on its own — the button
    /// in the settings is then the only way to bring it up to date.
    pub full_disk_index_max_age_hours: u64,
    /// How often the changes of the file system are put into the index, in
    /// seconds.
    ///
    /// **The changes are put in by rounds, not one by one.** A program that
    /// writes a thousand files would otherwise have the index rewritten a
    /// thousand times — and the disk read a thousand times — for a file that
    /// nobody is going to look for in the next half minute.
    ///
    /// A minute is the default: long enough that the disk stays quiet, and
    /// short enough that a file that was just made is found again soon.
    pub index_update_interval: u64,
    /// Whether the launcher offers to start itself as administrator.
    pub offer_admin_start: bool,

    // ---- the order of the results ----------------------------------------
    /// How the programs and the files are ordered.
    ///
    /// The launcher is used to start a program far more often than to look
    /// for a file, so the programs come first by default.
    pub result_order: ResultOrder,

    // ---- integrations --------------------------------------------------
    pub custom_explorer_index: usize,
    pub custom_explorer_list: Vec<CustomExplorer>,
    pub custom_browser_index: usize,
    pub custom_browser_list: Vec<CustomBrowser>,
    pub proxy: HttpProxy,

    // ---- startup -------------------------------------------------------
    pub start_flow_launcher_on_system_startup: bool,
    pub use_logon_task_for_startup: bool,
    pub hide_on_startup: bool,
    pub hide_notify_icon: bool,
    pub leave_cmd_open: bool,
    pub hide_when_deactivated: bool,
    pub show_taskbar_when_invoked: bool,
    pub show_at_topmost: bool,
    pub show_command_query: bool,
    pub first_launch: bool,
    pub activate_times: u64,
    pub release_notes_version: String,
    pub last_query_mode: String,
    pub log_level: String,
    pub dont_prompt_update_msg: bool,
    pub enable_update_log: bool,

    /// Per-plugin settings, keyed by plugin id.
    pub plugin_settings: BTreeMap<String, PluginSettings>,
    /// Plugin-specific runtime paths (python / node executables).
    pub python_executable_path: String,
    pub node_executable_path: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: "Alt + Space".into(),
            open_result_modifiers: "Alt".into(),
            preview_hotkey: String::new(),
            auto_complete_hotkey: String::new(),
            auto_complete_hotkey2: String::new(),
            select_next_item_hotkey: String::new(),
            select_next_item_hotkey2: String::new(),
            select_prev_item_hotkey: String::new(),
            select_prev_item_hotkey2: String::new(),
            select_next_page_hotkey: "PageUp".into(),
            select_prev_page_hotkey: "PageDown".into(),
            open_context_menu_hotkey: String::new(),
            setting_window_hotkey: String::new(),
            open_history_hotkey: String::new(),
            cycle_history_up_hotkey: String::new(),
            cycle_history_down_hotkey: String::new(),
            dialog_jump_hotkey: String::new(),

            color_scheme: ColorScheme::System,
            // No theme picked yet — the colour scheme decides.
            theme: String::new(),
            use_drop_shadow_effect: true,
            backdrop_type: BackdropType::default(),
            use_glyph_icons: true,
            use_animation: true,
            use_sound: true,
            sound_volume: 50,
            show_badges: false,
            show_badges_global_only: false,
            animation_speed: AnimationSpeed::Medium,
            custom_animation_length: 360,
            window_size: 580,
            window_height_size: 42,
            item_height_size: 58,
            query_box_font_size: 18,
            result_item_font_size: 16,
            result_sub_item_font_size: 13,
            query_box_font: "Segoe UI".into(),
            result_font: "Segoe UI".into(),
            result_sub_font: "Segoe UI".into(),
            setting_window_font: "Segoe UI".into(),
            use_clock: true,
            use_date: true,
            time_format: "hh:mm tt".into(),
            date_format: "MM'/'dd ddd".into(),
            // The icon stays in the hidden part of the tray, so that it does
            // not take up a place on the task bar.
            always_show_tray_icon: false,

            language: "zh-cn".into(),
            show_placeholder: true,
            // The preview shows what the index already knows. Reading a file
            // to show its picture is left to the user who asks for it.
            preview_media: false,
            placeholder_text: String::new(),
            show_home_page: true,
            show_history_results_for_home_page: false,
            max_history_results_to_show_for_home_page: 5,
            max_results_to_show: DEFAULT_MAX_RESULTS,
            keep_max_results: false,
            auto_restart_after_changing: true,
            show_unknown_source_warning: true,
            auto_update_plugins: true,
            auto_updates: true,
            custom_plugin_hotkeys: BTreeMap::new(),
            custom_shortcuts: BTreeMap::new(),

            search_query_results_with_delay: false,
            search_delay_time: 150,
            search_window_screen: SearchWindowScreen::Cursor,
            search_window_align: SearchWindowAlign::Center,
            custom_screen_number: 1,
            query_search_precision: SearchPrecision::Regular,
            should_use_pinyin: false,
            use_double_pinyin: false,
            double_pinyin_schema: DoublePinyinSchema::default(),
            always_start_en: false,
            always_preview: false,
            ignore_hotkeys_on_fullscreen: false,

            enable_smart_ranking: true,
            pick_count_threshold: crate::history::DEFAULT_PICK_THRESHOLD,

            enable_dialog_jump: false,
            auto_dialog_jump: false,
            show_dialog_jump_window: false,

            window_left: 0.0,
            window_top: 0.0,
            custom_window_left: 0.0,
            custom_window_top: 0.0,
            previous_screen_width: 0,
            previous_screen_height: 0,
            previous_dpi_x: 0,
            previous_dpi_y: 0,
            setting_window_width: 940,
            setting_window_height: 600,
            setting_window_top: 0.0,
            setting_window_left: 0.0,

            enable_full_disk_index: true,
            run_as_administrator: true,
            indexed_volumes: Vec::new(),
            excluded_paths: Vec::new(),
            excluded_extensions: Vec::new(),
            file_sort: FileSort::default(),
            full_disk_index_max_age_hours: 24,
        index_update_interval: 60,
            offer_admin_start: true,

            result_order: ResultOrder::default(),

            custom_explorer_index: 0,
            custom_explorer_list: Vec::new(),
            custom_browser_index: 0,
            custom_browser_list: Vec::new(),
            proxy: HttpProxy::default(),

            start_flow_launcher_on_system_startup: true,
            use_logon_task_for_startup: true,
            hide_on_startup: true,
            hide_notify_icon: false,
            leave_cmd_open: false,
            hide_when_deactivated: true,
            show_taskbar_when_invoked: false,
            show_at_topmost: true,
            show_command_query: false,
            first_launch: true,
            activate_times: 0,
            release_notes_version: String::new(),
            last_query_mode: "Empty".into(),
            log_level: "INFO".into(),
            dont_prompt_update_msg: false,
            enable_update_log: false,

            plugin_settings: BTreeMap::new(),
            python_executable_path: String::new(),
            node_executable_path: String::new(),
        }
    }
}

impl Settings {
    /// Load settings from a JSON file, falling back to defaults on any error.
    pub fn load(path: &Path) -> Self {
        let mut settings = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        };
        // The number of results the list holds used to be twenty, and the
        // slider that sets it went up to fifty. A value that small can only
        // come from that old slider, and it would cut the list off again —
        // so it is raised to the new default.
        if settings.max_results_to_show < 100 {
            settings.max_results_to_show = DEFAULT_MAX_RESULTS;
        }
        settings
    }

    /// Write settings to a JSON file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)
    }

    /// The directory settings live in.
    pub fn default_directory() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("IdeasLauncher")
    }

    /// The default settings file path.
    ///
    /// It is the same file the launcher reads and writes — the settings used
    /// to be read from `IdeasLauncher/Settings.json` and written to
    /// `IdeasLauncher/Settings/Settings.json`, so every change was lost when
    /// the launcher was started again.
    pub fn default_path() -> PathBuf {
        Self::default_directory().join("Settings").join("Settings.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_settings_file_is_the_one_the_launcher_uses() {
        let path = Settings::default_path();
        assert_eq!(path.file_name().unwrap(), "Settings.json");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "Settings");
    }

    #[test]
    fn defaults_round_trip_through_json() {        let s = Settings::default();
        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn missing_keys_fall_back_to_defaults() {
        let json = r#"{"Hotkey":"Ctrl + Space"}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.hotkey, "Ctrl + Space");
        assert_eq!(s.max_results_to_show, DEFAULT_MAX_RESULTS);
    }

    #[test]
    fn saves_and_loads() {
        let dir = std::env::temp_dir().join("ideas-launcher-settings-test");
        let path = dir.join("Settings.json");
        let mut s = Settings::default();
        s.theme = "BlurDark".into();
        s.save(&path).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.theme, "BlurDark");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
