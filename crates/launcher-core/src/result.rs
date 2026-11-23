//! The result model shared by every plugin.
//!
//! Field-for-field this mirrors the `Result` record in Flow Launcher's
//! `Flow.Launcher.Plugin.dll` (30 members), trimmed to the parts that are
//! meaningful for a from-scratch implementation.

use serde::{Deserialize, Serialize};

use std::path::PathBuf;
use std::sync::Arc;

/// Where an icon comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultIcon {
    /// Absolute path to an `.ico` / `.png` / `.exe` on disk.
    Path(PathBuf),
    /// Raw image bytes already in memory (avoids a disk round-trip).
    Bytes(Arc<Vec<u8>>),
    /// A font glyph, e.g. a Segoe MDL2 symbol.
    Glyph {
        font_family: String,
        glyph: String,
    },
    /// No icon.
    None,
}

impl Default for ResultIcon {
    fn default() -> Self {
        Self::None
    }
}

/// A single row in the result list.
#[derive(Debug, Clone, Default)]
pub struct Result {
    /// Primary text.
    pub title: String,
    /// Secondary text.
    pub subtitle: String,
    /// Tooltip for the title.
    pub title_tooltip: String,
    /// Tooltip for the subtitle.
    pub subtitle_tooltip: String,
    /// Icon shown next to the row.
    pub icon: ResultIcon,
    /// Small badge icon.
    pub badge_icon: ResultIcon,
    /// Whether the badge icon should be rendered.
    pub show_badge: bool,
    /// Sort score; higher is better.
    pub score: i32,
    /// Text that should replace the query when accepted (autocomplete).
    pub auto_complete_text: String,
    /// Text copied to the clipboard when the row is executed.
    pub copy_text: String,
    /// Arbitrary plugin-defined payload.
    pub context_data: Option<String>,
    /// The action the plugin wants to run when this row is selected.
    ///
    /// Flow Launcher sends this back to the plugin as a JSON-RPC call, so it
    /// is kept as the raw JSON the plugin sent.
    pub action: Option<serde_json::Value>,
    /// Id of the plugin that produced this result.
    pub plugin_id: String,
    /// Highlight ranges for the title, as byte offsets into [`Result::title`].
    pub title_highlight: Vec<(usize, usize)>,
    /// Identifier of the selected history entry, if any.
    pub record_key: Option<String>,
    /// Optional preview payload.
    pub preview: Option<PreviewData>,
    /// The size of the file, in bytes -?when the index knows it.
    ///
    /// The index holds it already, so it comes with the result and costs
    /// nothing to show: no file has to be opened to know how big it is.
    pub size: Option<u64>,
    /// When the file was last changed, in seconds since the Unix epoch.
    ///
    /// Like the size, this comes with the result.
    pub modified: Option<u64>,
    /// Progress bar state, 0..=100.
    pub progress: Option<u8>,
    /// Whether the query matched the result **only through its path** (or
    /// its description), and not its name.
    ///
    /// A program found through the letters scattered across its path is not
    /// really "found" the way a program is found: it is found the way a file
    /// is found, and it belongs with the files. Without this, `clash` put
    /// *Hermes* -?a program whose path happens to contain the letters -?    /// above every *Clash* on the machine, because programs were absolutely
    /// above files. Defaults to `false`: a result is matched by its name
    /// unless its producer says otherwise.
    pub path_only_match: bool,
}

/// How the results of the different plugins are ordered.
///
/// The launcher is used to *start a program* most of the time, so the
/// programs come first and the files last. The scores of the two kinds are
/// not comparable -?a file name matches a lot better than a program name -?/// and mixing them in one list means the files bury the programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub enum ResultOrder {
    /// Programs first, files last.
    #[default]
    ProgramsFirst,
    /// Everything in one list, ordered by the score alone.
    Mixed,
}

impl ResultOrder {
    /// Every option, in the order the settings show them.
    pub fn all() -> &'static [ResultOrder] {
        &[ResultOrder::ProgramsFirst, ResultOrder::Mixed]
    }

    /// The name shown in the settings.
    pub fn label(self) -> &'static str {
        match self {
            ResultOrder::ProgramsFirst => "程序在前,文件在后",
            ResultOrder::Mixed => "混在一-?按分数排",
        }
    }
}

/// The group a result belongs to.
///
/// A smaller group comes first in the list. The groups only matter when the
/// order is [`ResultOrder::ProgramsFirst`]; with
/// [`ResultOrder::Mixed`] every row is in the same group.
///
/// A program that was matched **by its name** (or its pinyin) is a program
/// the user can start and comes first; one that was matched only through
/// the letters scattered across its *path* belongs with the files -- it was
/// found the way a file is found, and a perfect file match must outrank it.
pub fn result_group(plugin_id: &str, order: ResultOrder, path_only_match: bool) -> u8 {
    if order == ResultOrder::Mixed {
        return 0;
    }
    match plugin_id {
        // The programs the user can start -?by their name.
        "program" | "uwp" if !path_only_match => 0,
        // A program found only through its path: a file that happens to be
        // a program.
        "program" | "uwp" => 2,
        // The files on the disk.
        "file" => 2,
        // Everything else: bookmarks, web searches, system commands, the
        // rows of the external plugins.
        _ => 1,
    }
}

/// How the description of a preview is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewContentType {
    /// The image above the title, the description as plain text below it.
    #[default]
    ImageWithText,
    /// The description rendered as markdown.
    Markdown,
}

impl PreviewContentType {
    /// The name used on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewContentType::ImageWithText => "imageWithText",
            PreviewContentType::Markdown => "markdown",
        }
    }
}

/// Whether the preview pane is shown for a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewVisibility {
    /// Shown when the user asked for previews.
    #[default]
    Optional,
    /// Never shown.
    Never,
    /// Always shown.
    Always,
}

impl PreviewVisibility {
    /// The name used on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewVisibility::Optional => "optional",
            PreviewVisibility::Never => "never",
            PreviewVisibility::Always => "always",
        }
    }
}

/// What the preview pane shows for a result.
///
/// Mirrors the `PreviewInfo` record in Flow Launcher's `Flow.Launcher.Plugin.dll`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewData {
    /// The text shown in the preview pane.
    pub description: String,
    /// An image shown above the description.
    pub image_path: Option<String>,
    /// The file the preview belongs to.
    pub file_path: Option<String>,
    /// How the description is rendered.
    pub content_type: PreviewContentType,
    /// Whether the image fills the width of the pane.
    pub is_media: bool,
    /// Whether the pane is shown for this result.
    pub visibility: PreviewVisibility,
}

impl Result {
    /// Create a result with just a title.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Default::default()
        }
    }

    /// Set the subtitle.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// Set the icon.
    pub fn with_icon(mut self, icon: ResultIcon) -> Self {
        self.icon = icon;
        self
    }

    /// Set the owning plugin id.
    pub fn with_plugin(mut self, plugin_id: impl Into<String>) -> Self {
        self.plugin_id = plugin_id.into();
        self
    }

    /// Set the sort score.
    pub fn with_score(mut self, score: i32) -> Self {
        self.score = score;
        self
    }

    /// Set the text copied to the clipboard on execution.
    pub fn with_copy_text(mut self, text: impl Into<String>) -> Self {
        self.copy_text = text.into();
        self
    }

    /// Text that should replace the query when this row is accepted.
    pub fn auto_complete(&self) -> &str {
        if self.auto_complete_text.is_empty() {
            &self.title
        } else {
            &self.auto_complete_text
        }
    }
}

/// Upper bound for a result score.
///
/// Mirrors `Result.MaxScore` in Flow Launcher; used to normalise scores coming
/// from different plugins onto one scale.
pub const MAX_SCORE: i32 = 1000;

/// How aggressively low-scoring matches are filtered out.
///
/// Mirrors the `SearchPrecisionScore` enum in `Flow.Launcher.Plugin.dll`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchPrecision {
    /// Keep only strong matches (the default).
    #[default]
    Regular,
    /// Keep weak matches too.
    Low,
    /// Keep everything that matched at all.
    None,
}

impl SearchPrecision {
    /// Minimum score a match must reach to be kept.
    pub fn min_score(self) -> i32 {
        match self {
            // Substring, prefix, acronym or boundary-aligned matches.
            SearchPrecision::Regular => 400,
            // Any real subsequence match.
            SearchPrecision::Low => 200,
            // Keep everything that matched at all.
            SearchPrecision::None => i32::MIN,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_methods_chain() {
        let r = Result::new("Chrome")
            .with_subtitle("C:\\Program Files\\Chrome")
            .with_plugin("program")
            .with_score(100);
        assert_eq!(r.title, "Chrome");
        assert_eq!(r.plugin_id, "program");
        assert_eq!(r.score, 100);
    }

    #[test]
    fn auto_complete_falls_back_to_title() {
        let r = Result::new("Chrome");
        assert_eq!(r.auto_complete(), "Chrome");
    }

    #[test]
    fn a_path_only_program_ranks_with_the_files() {
        // A program matched by its name is a program: it comes first.
        assert_eq!(result_group("program", ResultOrder::ProgramsFirst, false), 0);
        // A program matched only through the letters of its path is found
        // the way a file is found, and belongs with the files -- `clash`
        // must not put *Hermes* above every *Clash* on the machine.
        assert_eq!(result_group("program", ResultOrder::ProgramsFirst, true), 2);
        // The order setting the user chose still wins.
        assert_eq!(result_group("program", ResultOrder::Mixed, true), 0);
        // The flag does not move anything that is not a program.
        assert_eq!(result_group("file", ResultOrder::ProgramsFirst, true), 2);
        assert_eq!(result_group("web", ResultOrder::ProgramsFirst, true), 1);
    }
}
