//! Windows Settings.
//!
//! Mirrors Flow Launcher's `Windows Settings` plugin: the 524 entries of
//! its `WindowsSettings.json`, with the English and Simplified Chinese
//! display names taken from `Resources.resx` and `Resources.zh-cn.resx`.
//!
//! The data is embedded at compile time with `include_str!`, so the launcher
//! stays a single file and never reads anything from disk at runtime.
//!
//! # Wiring the action
//!
//! [`QueryPlugin`] has no hook for "the user pressed enter on this row", so
//! the launcher's result handler has to open the page itself. Three pieces
//! are provided for that:
//!
//! * every result carries the *upstream resource key* of its setting in
//!   [`Result::context_data`], e.g. `AccessWorkOrSchool`,
//! * [`command_for`] turns that key back into the command that opens the
//!   page,
//! * [`open_settings`] runs that command.
//!
//! The command is put into [`Result::copy_text`] as well, which the launcher
//! already uses as "what to open when the row is accepted", so every
//! `ms-settings:` page opens correctly without any change to the handler.

use std::sync::LazyLock;

use launcher_core::matcher::StringMatcher;
use launcher_core::plugin::{PluginResult, QueryPlugin};
use launcher_core::query::{GLOBAL_PLUGIN_WILDCARD, Query};
use launcher_core::result::Result as LauncherResult;

/// The plugin id, used for the settings keys and for result attribution.
pub const PLUGIN_ID: &str = "windows_settings";

/// The settings data, embedded at compile time.
///
/// The compact format uses one object per setting:
///
/// | key  | meaning                                            |
/// |------|----------------------------------------------------|
/// | `n`  | the upstream resource key, e.g. `AccessWorkOrSchool` |
/// | `a`  | the upstream area key, e.g. `Accounts`              |
/// | `c`  | the command that opens the page                     |
/// | `al` | alternative names (upstream resource keys)         |
/// | `az` | alternative names in Chinese                        |
/// | `en` | the English display name                            |
/// | `zh` | the Chinese display name                            |
/// | `as` | the display name of the area                        |
const DATA: &str = include_str!("../../resources/windows_settings.json");

/// The number of results returned for one query.
const MAX_RESULTS: usize = 30;

/// How much a match outside the display name is worth less.
///
/// A hit on the display name is what the user meant; a hit on the area or
/// on an alternative name is only a hint, so it ranks lower.
const NON_NAME_PENALTY: i32 = 50;

/// One Windows settings page, as stored in the data file.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct WindowsSetting {
    /// The upstream resource key, e.g. `AccessWorkOrSchool`.
    ///
    /// This is the `context_data` of the result and the key
    /// [`command_for`] looks up.
    #[serde(rename = "n")]
    pub name: String,
    /// The upstream area key, e.g. `Accounts`.
    #[serde(rename = "a")]
    pub area: String,
    /// The command that opens the page.
    #[serde(rename = "c")]
    pub command: String,
    /// Alternative names, as upstream resource keys.
    #[serde(rename = "al", default)]
    pub alt_names: Vec<String>,
    /// Alternative names in Chinese.
    #[serde(rename = "az", default)]
    pub alt_names_zh: Vec<String>,
    /// The English display name.
    #[serde(rename = "en", default)]
    pub name_en: String,
    /// The Chinese display name, if the upstream resources have one.
    #[serde(rename = "zh", default)]
    pub name_zh: Option<String>,
    /// The display name of the area, e.g. `帐户`.
    #[serde(rename = "as", default)]
    pub area_label: Option<String>,
}

/// A setting, prepared for matching.
struct Indexed {
    /// The display name: Chinese when available, English otherwise.
    title: String,
    /// [`Self::title`], lower-cased.
    ///
    /// The data only holds ASCII and CJK text, and lower-casing keeps the
    /// byte length of both, so a match offset into this string is also a
    /// valid offset into [`Self::title`].
    title_lower: String,
    /// The area the setting belongs to, in the user's language.
    area_label: String,
    /// The upstream resource key.
    name: String,
    /// The command that opens the page.
    command: String,
    /// Every other haystack, lower-cased: the English name, the raw key, the
    /// alternative names, the area key and the area label.
    others: Vec<String>,
}

impl Indexed {
    fn new(setting: &WindowsSetting) -> Self {
        let title = setting
            .name_zh
            .as_deref()
            .filter(|zh| !zh.is_empty())
            .unwrap_or(&setting.name_en)
            .to_string();
        let area_label = setting
            .area_label
            .as_deref()
            .filter(|label| !label.is_empty())
            .unwrap_or(&setting.area)
            .to_string();

        let mut others = Vec::with_capacity(3 + setting.alt_names.len() * 2);
        others.push(setting.name_en.to_lowercase());
        others.push(setting.name.to_lowercase());
        for alt in &setting.alt_names {
            others.push(alt.to_lowercase());
        }
        for alt in &setting.alt_names_zh {
            others.push(alt.to_lowercase());
        }
        others.push(setting.area.to_lowercase());
        others.push(area_label.to_lowercase());

        Self {
            title_lower: title.to_lowercase(),
            title,
            area_label,
            name: setting.name.clone(),
            command: setting.command.clone(),
            others,
        }
    }
}

/// Every setting, parsed from the embedded data.
static SETTINGS: LazyLock<Vec<WindowsSetting>> =
    LazyLock::new(|| serde_json::from_str(DATA).expect("the embedded Windows settings data must parse"));

/// Every setting, prepared for matching.
static INDEX: LazyLock<Vec<Indexed>> =
    LazyLock::new(|| SETTINGS.iter().map(Indexed::new).collect());

/// Every setting the plugin knows about.
pub fn settings() -> &'static [WindowsSetting] {
    &SETTINGS
}

/// The command that opens the setting with the given upstream resource key.
///
/// This is the `context_data` of every result of this plugin, so it is the
/// way back from a selected row to the page it stands for.
///
/// A handful of keys exist twice upstream — `Sound` and `Speech` for
/// instance, once as a system page and once as a control-panel page — and
/// the first entry wins. Use [`open_settings`] with the command of the
/// result instead when the exact page matters.
pub fn command_for(context_data: &str) -> Option<String> {
    SETTINGS
        .iter()
        .find(|setting| setting.name == context_data)
        .map(|setting| setting.command.clone())
}

/// Open a Windows settings page.
///
/// `command` is a command from the data file: either an `ms-settings:` URI
/// or a control-panel command line. `%VAR%` references are expanded first.
///
/// Returns `true` when the process could be started.
pub fn open_settings(command: &str) -> bool {
    let command = expand_env_vars(command.trim());
    if command.is_empty() {
        return false;
    }

    // `ms-settings:` URIs are resolved by the shell. Only the first word is
    // used: one entry upstream is `ms-settings:privacy or privacy-general`,
    // and a URI with a space in it is not a URI.
    if command.to_ascii_lowercase().starts_with("ms-settings") {
        let mut words = command.split_whitespace();
        let mut uri = words.next().unwrap_or("ms-settings").to_string();
        if !uri.contains(':') {
            uri.push(':');
        }
        return spawn_explorer(&uri);
    }

    let (program, arguments) = split_command(&command);
    if arguments.is_empty() {
        // A bare URI or file name: let the shell resolve it.
        return spawn_explorer(program);
    }
    let mut command = std::process::Command::new(program);
    hide_console(&mut command);
    command
        .args(arguments.split_whitespace())
        .spawn()
        .is_ok()
}

/// Hand a target to the shell.
///
/// `explorer.exe` is the shell's own program, so it resolves `ms-settings:`
/// URIs and file system paths alike.
fn spawn_explorer(target: &str) -> bool {
    let mut command = std::process::Command::new("explorer.exe");
    hide_console(&mut command);
    command.arg(target).spawn().is_ok()
}

/// The `CREATE_NO_WINDOW` flag, so that a console program does not flash a
/// window while it starts.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Keep a console program from flashing a window while it starts.
///
/// A window that flashes takes the focus away from the launcher, and the
/// launcher hides itself when it loses the focus — so the list of results
/// would disappear right after it appeared.
#[cfg(windows)]
fn hide_console(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

/// The same on the other platforms: there is nothing to do there.
#[cfg(not(windows))]
fn hide_console(_command: &mut std::process::Command) {}

/// Split a command line into the program and its arguments.
///
/// The program is the longest prefix that ends in `.exe`, so that a path
/// with spaces survives: `C:\Program Files\…\ImagingDevices.exe` is one
/// program and not two. Commands without an `.exe` are split at the first
/// space, the way the upstream plugin does it.
fn split_command(command: &str) -> (&str, &str) {
    let lowered = command.to_ascii_lowercase();
    let mut search = 0usize;
    while let Some(offset) = lowered[search..].find(".exe") {
        let end = search + offset + ".exe".len();
        let next = lowered[end..].chars().next();
        if next.is_none() || next == Some(' ') {
            return (&command[..end], command[end..].trim_start());
        }
        search = end;
    }
    match command.split_once(' ') {
        Some((program, arguments)) => (program, arguments.trim_start()),
        None => (command, ""),
    }
}

/// Expand `%VAR%` references, the way the upstream plugin does.
fn expand_env_vars(input: &str) -> String {
    if !input.contains('%') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            // An unbalanced `%` is not a reference.
            out.push('%');
            out.push_str(after);
            return out;
        };
        let name = &after[..end];
        if name.is_empty() {
            out.push('%');
        } else {
            match std::env::var(name) {
                Ok(value) => out.push_str(&value),
                Err(_) => {
                    out.push('%');
                    out.push_str(name);
                    out.push('%');
                }
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Searches the Windows settings pages.
pub struct WindowsSettingsPlugin {
    matcher: StringMatcher,
}

impl WindowsSettingsPlugin {
    pub fn new() -> Self {
        Self {
            matcher: StringMatcher::new(),
        }
    }
}

impl Default for WindowsSettingsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryPlugin for WindowsSettingsPlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn name(&self) -> &str {
        "Windows Settings"
    }

    fn action_keywords(&self) -> &[String] {
        static KEYWORDS: LazyLock<Vec<String>> = LazyLock::new(|| {
            vec![
                GLOBAL_PLUGIN_WILDCARD.to_string(),
                "settings".to_string(),
                "设置".to_string(),
            ]
        });
        &KEYWORDS
    }

    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
        let search = query.search.trim();
        if search.is_empty() {
            return Ok(Vec::new());
        }
        // The query is prepared once and reused for every setting.
        let prepared = self.matcher.prepare(search);

        let mut scored: Vec<(i32, &Indexed, Vec<(usize, usize)>)> = Vec::new();
        for item in INDEX.iter() {
            let mut best_score = 0;
            let mut highlight: Vec<(usize, usize)> = Vec::new();

            // The display name is worth the most.
            if let Some(hit) = self.matcher.match_prepared_lower(&prepared, &item.title_lower) {
                best_score = hit.score;
                highlight = hit.matched_indices;
            }
            for other in &item.others {
                if let Some(hit) = self.matcher.match_prepared_lower(&prepared, other) {
                    let score = hit.score - NON_NAME_PENALTY;
                    if score > best_score {
                        best_score = score;
                        // The hit is not in the display name, so there is
                        // nothing to highlight in the title.
                        highlight.clear();
                    }
                }
            }

            if best_score > 0 {
                scored.push((best_score, item, highlight));
            }
        }

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.title.cmp(&b.1.title))
        });

        Ok(scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(score, item, highlight)| LauncherResult {
                title: item.title.clone(),
                subtitle: item.area_label.clone(),
                score,
                plugin_id: PLUGIN_ID.to_string(),
                // The upstream resource key, so that the result handler can
                // find the command that opens this page again.
                context_data: Some(item.name.clone()),
                // The command itself, so that the launcher's default
                // "open the row" path already opens the right page.
                copy_text: item.command.clone(),
                title_highlight: highlight,
                ..Default::default()
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_data_parses() {
        assert!(
            SETTINGS.len() >= 500,
            "only {} settings in the embedded data",
            SETTINGS.len()
        );
    }

    #[test]
    fn every_entry_has_a_command() {
        for setting in SETTINGS.iter() {
            assert!(
                !setting.command.trim().is_empty(),
                "{} has no command",
                setting.name
            );
        }
    }

    #[test]
    fn most_entries_have_a_chinese_name() {
        let with_chinese = SETTINGS
            .iter()
            .filter(|setting| {
                setting
                    .name_zh
                    .as_deref()
                    .is_some_and(|zh| !zh.is_empty())
            })
            .count();
        assert!(
            with_chinese >= 20,
            "only {with_chinese} entries have a Chinese name"
        );
    }

    #[test]
    fn a_chinese_query_finds_a_setting() {
        let plugin = WindowsSettingsPlugin::new();
        let results = plugin.query(&Query::plain("蓝牙")).unwrap();
        assert!(!results.is_empty(), "蓝牙 found nothing");
        assert!(
            results.iter().any(|result| result.title.contains("蓝牙")),
            "蓝牙 found no Chinese title: {:?}",
            results.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_chinese_query_finds_a_setting_by_its_sound_name() {
        let plugin = WindowsSettingsPlugin::new();
        let results = plugin.query(&Query::plain("声音")).unwrap();
        assert!(!results.is_empty(), "声音 found nothing");
    }

    #[test]
    fn an_alternative_name_finds_a_setting() {
        let plugin = WindowsSettingsPlugin::new();
        // "Workplace" is an alternative name of "Access work or school".
        let results = plugin.query(&Query::plain("Workplace")).unwrap();
        assert!(
            results
                .iter()
                .any(|result| result.context_data.as_deref() == Some("AccessWorkOrSchool")),
            "Workplace found nothing: {:?}",
            results.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_english_name_finds_a_setting() {
        let plugin = WindowsSettingsPlugin::new();
        let results = plugin.query(&Query::plain("bluetooth")).unwrap();
        assert!(!results.is_empty(), "bluetooth found nothing");
    }

    #[test]
    fn an_empty_query_finds_nothing() {
        let plugin = WindowsSettingsPlugin::new();
        assert!(plugin.query(&Query::plain("   ")).unwrap().is_empty());
    }

    #[test]
    fn results_carry_the_key_and_the_command() {
        let plugin = WindowsSettingsPlugin::new();
        let results = plugin.query(&Query::plain("蓝牙")).unwrap();
        let result = results
            .iter()
            .find(|result| result.title.contains("蓝牙"))
            .expect("a result for 蓝牙");
        assert_eq!(result.plugin_id, PLUGIN_ID);
        assert!(!result.subtitle.is_empty(), "the area is missing");
        let key = result.context_data.as_deref().expect("no context data");
        let command = command_for(key).expect("no command for the key");
        assert!(!command.is_empty());
    }

    #[test]
    fn at_most_thirty_results_are_returned() {
        let plugin = WindowsSettingsPlugin::new();
        // A single letter matches a lot of settings.
        let results = plugin.query(&Query::plain("a")).unwrap();
        assert!(results.len() <= MAX_RESULTS, "{} results", results.len());
    }

    #[test]
    fn the_command_of_a_key_is_found() {
        assert_eq!(
            command_for("AccessWorkOrSchool").as_deref(),
            Some("ms-settings:workplace")
        );
        assert!(command_for("ThereIsNoSuchSetting").is_none());
    }

    #[test]
    fn the_plugin_registers_its_keywords() {
        let plugin = WindowsSettingsPlugin::new();
        assert_eq!(plugin.id(), PLUGIN_ID);
        assert_eq!(plugin.name(), "Windows Settings");
        let keywords = plugin.action_keywords();
        assert!(keywords.contains(&GLOBAL_PLUGIN_WILDCARD.to_string()));
        assert!(keywords.contains(&"settings".to_string()));
        assert!(keywords.contains(&"设置".to_string()));
    }

    #[test]
    fn environment_variables_are_expanded() {
        let expanded = expand_env_vars("%SystemRoot%\\system32\\control.exe");
        assert!(
            !expanded.contains("%SystemRoot%"),
            "the variable was not expanded: {expanded}"
        );
        assert!(expanded.to_lowercase().contains("control.exe"));
    }

    #[test]
    fn an_unknown_variable_is_left_alone() {
        assert_eq!(
            expand_env_vars("%NoSuchVariableHere%\\x"),
            "%NoSuchVariableHere%\\x"
        );
    }

    #[test]
    fn a_program_with_spaces_in_its_path_stays_one_program() {
        let (program, arguments) =
            split_command("C:\\Program Files\\Windows Photo Viewer\\ImagingDevices.exe");
        assert_eq!(
            program,
            "C:\\Program Files\\Windows Photo Viewer\\ImagingDevices.exe"
        );
        assert!(arguments.is_empty());
    }

    #[test]
    fn arguments_are_split_off_the_program() {
        let (program, arguments) = split_command("control /name Microsoft.Sound");
        assert_eq!(program, "control");
        assert_eq!(arguments, "/name Microsoft.Sound");
    }

    #[test]
    fn a_command_without_an_exe_is_split_at_the_first_space() {
        // One upstream entry is `ms-settings:privacy or privacy-general`;
        // only the URI itself is a valid target.
        let (program, _) = split_command("ms-settings:privacy or privacy-general");
        assert_eq!(program, "ms-settings:privacy");
    }
}
