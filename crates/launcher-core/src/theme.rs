//! The theme model.
//!
//! Flow Launcher ships 20 XAML `ResourceDictionary` themes under
//! `Themes/` (`Base.xaml` plus `Dracula`, `Nord Darker`, `Ubuntu`, `Sublime`,
//! `Discord Dark`, `Win11Light`, `SlimLight`, …), each a WPF resource
//! dictionary. Themes there are only reachable from a WPF resource system and
//! cannot be reused outside it.
//!
//! Here a theme is plain data: a palette, a blur setting and a couple of
//! metrics. That makes it portable, cheap to serialise and — because the
//! launcher paints its own card — enough to express the same set of looks.
//! The built-in themes mirror the names and moods of the originals.

use serde::{Deserialize, Serialize};

/// Whether a theme is designed for a light or a dark desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ThemeKind {
    Light,
    #[default]
    Dark,
}

/// The full colour palette of a theme.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeColors {
    /// The card background.
    pub surface: [u8; 3],
    /// A slightly raised surface, used for the selected row.
    pub surface_alt: [u8; 3],
    /// The hairline border around the card.
    pub border: [u8; 3],
    /// Primary text.
    pub text: [u8; 3],
    /// Secondary text.
    pub text_dim: [u8; 3],
    /// Faint text, used for hints.
    pub text_faint: [u8; 3],
    /// The search glyph.
    pub glyph: [u8; 3],
    /// The accent used for selection and focus.
    pub accent: [u8; 3],
}

/// A complete theme.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    /// The theme name, shown in the theme picker.
    pub name: String,
    /// Light or dark.
    pub kind: ThemeKind,
    /// The palette.
    pub colors: ThemeColors,
    /// Whether the card casts a shadow.
    pub shadow: bool,
}

impl Theme {
    /// The theme used when nothing has been configured.
    pub fn default_theme() -> Self {
        Self::builtin("Midnight")
            .expect("the built-in theme set always contains Midnight")
    }

    /// Look up a built-in theme by name.
    pub fn builtin(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        builtin_themes()
            .into_iter()
            .find(|t| t.name.to_ascii_lowercase() == lower)
    }

    /// The names of all built-in themes, in display order.
    pub fn builtin_names() -> Vec<String> {
        builtin_themes().into_iter().map(|t| t.name).collect()
    }

    /// Resolve a theme name to a theme, falling back to the default.
    pub fn resolve(name: &str) -> Self {
        Self::builtin(name).unwrap_or_else(Self::default_theme)
    }
}

/// Every built-in theme.
pub fn builtin_themes() -> Vec<Theme> {
    vec![
        theme("Midnight", ThemeKind::Dark, [28, 30, 36], [38, 41, 49], [58, 62, 74], [236, 239, 245], [154, 160, 174], [104, 110, 124], [130, 137, 152], [88, 156, 255]),
        theme("Darker", ThemeKind::Dark, [18, 18, 20], [26, 26, 30], [44, 44, 52], [224, 226, 232], [140, 143, 154], [94, 97, 108], [120, 124, 136], [96, 160, 240]),
        theme("Gray", ThemeKind::Dark, [44, 44, 48], [54, 54, 60], [72, 72, 80], [232, 232, 236], [160, 160, 168], [112, 112, 120], [140, 140, 148], [128, 168, 232]),
        theme("Dracula", ThemeKind::Dark, [40, 42, 54], [68, 71, 90], [98, 100, 128], [248, 248, 242], [189, 189, 198], [128, 128, 146], [140, 140, 150], [189, 147, 249]),
        theme("Nord Darker", ThemeKind::Dark, [46, 52, 64], [59, 66, 82], [76, 86, 106], [236, 239, 244], [149, 157, 176], [102, 110, 133], [129, 138, 158], [136, 192, 208]),
        theme("Sublime", ThemeKind::Dark, [38, 38, 38], [48, 48, 48], [64, 64, 64], [220, 220, 220], [150, 150, 150], [104, 104, 104], [128, 128, 128], [99, 190, 123]),
        theme("Ubuntu", ThemeKind::Dark, [48, 46, 41], [58, 56, 50], [78, 76, 68], [233, 233, 220], [170, 168, 158], [118, 116, 108], [140, 138, 130], [229, 165, 52]),
        theme("Discord Dark", ThemeKind::Dark, [43, 45, 49], [56, 58, 64], [72, 74, 82], [219, 222, 225], [148, 151, 156], [99, 102, 108], [120, 124, 130], [88, 166, 255]),
        theme("Cyan Dark", ThemeKind::Dark, [22, 26, 32], [30, 36, 44], [44, 54, 64], [222, 232, 240], [140, 156, 170], [92, 106, 118], [118, 134, 148], [64, 196, 220]),
        theme("Pink", ThemeKind::Dark, [40, 26, 34], [52, 34, 44], [70, 46, 58], [244, 232, 238], [188, 160, 176], [134, 112, 124], [158, 134, 146], [236, 112, 168]),
        theme("League", ThemeKind::Dark, [22, 24, 30], [32, 35, 42], [46, 50, 58], [222, 226, 234], [142, 148, 158], [96, 102, 112], [120, 126, 136], [212, 175, 55]),
        theme("Win11", ThemeKind::Light, [243, 243, 246], [252, 252, 253], [214, 214, 222], [32, 32, 32], [110, 110, 118], [150, 150, 158], [130, 130, 138], [0, 95, 184]),
        theme("Win10System", ThemeKind::Light, [246, 246, 248], [252, 252, 253], [216, 216, 222], [28, 28, 30], [106, 106, 112], [148, 148, 154], [128, 128, 134], [0, 120, 212]),
        theme("SlimLight", ThemeKind::Light, [255, 255, 255], [244, 245, 248], [228, 229, 234], [30, 32, 36], [112, 116, 124], [154, 158, 166], [132, 136, 144], [24, 119, 242]),
        theme("BlurWhite", ThemeKind::Light, [250, 250, 252], [255, 255, 255], [222, 224, 230], [28, 30, 34], [108, 112, 120], [150, 154, 162], [130, 134, 142], [28, 116, 226]),
        theme("Base", ThemeKind::Light, [250, 250, 250], [255, 255, 255], [225, 225, 228], [26, 26, 26], [110, 110, 112], [152, 152, 155], [132, 132, 135], [0, 120, 212]),
    ]
}

/// Helper to build a theme with the defaults most themes share.
#[allow(clippy::too_many_arguments)]
fn theme(
    name: &str,
    kind: ThemeKind,
    surface: [u8; 3],
    surface_alt: [u8; 3],
    border: [u8; 3],
    text: [u8; 3],
    text_dim: [u8; 3],
    text_faint: [u8; 3],
    glyph: [u8; 3],
    accent: [u8; 3],
) -> Theme {
    Theme {
        name: name.to_string(),
        kind,
        colors: ThemeColors {
            surface,
            surface_alt,
            border,
            text,
            text_dim,
            text_faint,
            glyph,
            accent,
        },
        shadow: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_theme_is_unique_and_findable() {
        let themes = builtin_themes();
        assert!(themes.len() >= 12);
        let mut names: Vec<String> = themes.iter().map(|t| t.name.clone()).collect();
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count, "theme names must be unique");
        for t in &themes {
            assert!(Theme::builtin(&t.name).is_some());
        }
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(Theme::builtin("dracula").unwrap().name, "Dracula");
        assert!(Theme::builtin("nope").is_none());
    }

    #[test]
    fn unknown_theme_falls_back_to_the_default() {
        assert_eq!(Theme::resolve("nope").name, Theme::default_theme().name);
    }

    #[test]
    fn theme_round_trips_through_json() {
        let t = Theme::builtin("Dracula").unwrap();
        let json = serde_json::to_string(&t).unwrap();
        let back: Theme = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn both_kinds_are_represented() {
        let themes = builtin_themes();
        assert!(themes.iter().any(|t| t.kind == ThemeKind::Dark));
        assert!(themes.iter().any(|t| t.kind == ThemeKind::Light));
    }
}
