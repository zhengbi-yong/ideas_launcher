//! Bridges the core theme model to egui visuals.

use egui::{Color32, CornerRadius, Stroke, Visuals};
use launcher_core::theme::{Theme, ThemeKind};

/// The colour of the letter badge of a result.
///
/// Not every result has an icon of its own — a web search, a bookmark, a
/// system command — and a row without an icon looks broken next to the ones
/// that have one. The badge fills that gap: the first character of the title
/// on a tinted square.
///
/// The hue is derived from the title, so the same program always gets the
/// same colour, and it is kept close to the accent colour so the badges stay
/// in tune with the theme instead of turning the list into a rainbow.
pub fn badge_color(palette: &Palette, seed: &str) -> Color32 {
    let (hue, saturation, _) = rgb_to_hsv(palette.accent);
    // A small, stable offset per title: enough to tell two neighbours apart,
    // not enough to leave the theme's colour range.
    let offset = (fnv1a(seed) % 111) as f32 - 55.0;
    let hue = (hue + offset).rem_euclid(360.0);
    let saturation = saturation.max(0.45).min(0.85);
    // The letter has to be readable on the card, so its brightness follows
    // the theme rather than the accent.
    let value = match palette.kind {
        ThemeKind::Dark => 0.88,
        ThemeKind::Light => 0.50,
    };
    let (r, g, b) = hsv_to_rgb(hue, saturation, value);
    Color32::from_rgb(r, g, b)
}

/// A stable hash of a string.
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// The hue, saturation and value of a colour.
fn rgb_to_hsv(color: Color32) -> (f32, f32, f32) {
    let r = color.r() as f32 / 255.0;
    let g = color.g() as f32 / 255.0;
    let b = color.b() as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let hue = if delta <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };

    let saturation = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };

    (hue.rem_euclid(360.0), saturation, max)
}

/// The colour of a hue, saturation and value.
fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> (u8, u8, u8) {
    let chroma = value * saturation;
    let hue = hue.rem_euclid(360.0);
    let x = chroma * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match hue as u32 / 60 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = value - chroma;
    (
        ((r + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

/// The colours and metrics the launcher paints with.
#[derive(Debug, Clone)]
pub struct Palette {
    /// Light or dark.
    pub kind: ThemeKind,
    /// The card background.
    pub surface: Color32,
    /// A slightly raised surface, used for the selected row.
    pub surface_alt: Color32,
    /// The hairline border around the card.
    pub border: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text.
    pub text_dim: Color32,
    /// Faint text, used for hints.
    pub text_faint: Color32,
    /// The search glyph.
    pub glyph: Color32,
    /// The accent used for selection and focus.
    pub accent: Color32,
    /// The accent at low opacity, for the selection background.
    pub accent_soft: Color32,
}

impl Palette {
    /// Derive the palette from a core theme.
    pub fn from_theme(theme: &Theme) -> Self {
        let c = &theme.colors;
        let accent = Color32::from_rgb(c.accent[0], c.accent[1], c.accent[2]);
        Self {
            kind: theme.kind,
            surface: Color32::from_rgb(c.surface[0], c.surface[1], c.surface[2]),
            surface_alt: Color32::from_rgb(c.surface_alt[0], c.surface_alt[1], c.surface_alt[2]),
            border: Color32::from_rgb(c.border[0], c.border[1], c.border[2]),
            text: Color32::from_rgb(c.text[0], c.text[1], c.text[2]),
            text_dim: Color32::from_rgb(c.text_dim[0], c.text_dim[1], c.text_dim[2]),
            text_faint: Color32::from_rgb(c.text_faint[0], c.text_faint[1], c.text_faint[2]),
            glyph: Color32::from_rgb(c.glyph[0], c.glyph[1], c.glyph[2]),
            accent,
            accent_soft: Color32::from_rgba_unmultiplied(
                c.accent[0],
                c.accent[1],
                c.accent[2],
                38,
            ),
        }
    }

    /// The palette used when the system prefers a dark desktop.
    pub fn default_dark() -> Self {
        Self::from_theme(&Theme::resolve("Midnight"))
    }

    /// The palette used when the system prefers a light desktop.
    pub fn default_light() -> Self {
        Self::from_theme(&Theme::resolve("Win11"))
    }
}

/// Turn a palette into egui visuals.
pub fn visuals(palette: &Palette) -> Visuals {
    let mut visuals = if palette.kind == ThemeKind::Dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    visuals.override_text_color = Some(palette.text);
    visuals.window_corner_radius = CornerRadius::ZERO;
    visuals.window_shadow.color = Color32::from_black_alpha(if palette.kind == ThemeKind::Dark {
        120
    } else {
        48
    });
    visuals.window_stroke = Stroke::new(1.0, palette.border);
    visuals.panel_fill = palette.surface;

    // Every widget state is set explicitly. The widgets fall back to the
    // light defaults for the parts that are not set, which in a dark theme
    // showed up as white input fields and buttons.
    visuals.widgets.active.bg_fill = palette.accent;
    visuals.widgets.active.weak_bg_fill = palette.accent;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, palette.accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.inactive.bg_fill = palette.surface_alt;
    visuals.widgets.inactive.weak_bg_fill = palette.surface_alt;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::TRANSPARENT);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);
    visuals.widgets.hovered.bg_fill = palette.accent_soft;
    visuals.widgets.hovered.weak_bg_fill = palette.accent_soft;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.border);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, palette.text);
    visuals.widgets.noninteractive.bg_fill = palette.surface_alt;
    visuals.widgets.noninteractive.weak_bg_fill = palette.surface_alt;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::TRANSPARENT);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text);
    visuals.widgets.open.bg_fill = palette.surface_alt;
    visuals.widgets.open.weak_bg_fill = palette.surface_alt;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, palette.border);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, palette.text);

    // The background of the text fields and the scroll bars.
    visuals.extreme_bg_color = palette.surface_alt;
    visuals.text_edit_bg_color = Some(palette.surface_alt);
    visuals.faint_bg_color = palette.surface;
    visuals.code_bg_color = palette.surface;

    visuals.selection.bg_fill = palette.accent_soft;
    visuals.selection.stroke = Stroke::new(1.0, Color32::TRANSPARENT);
    visuals.hyperlink_color = palette.accent;
    visuals.error_fg_color = Color32::from_rgb(233, 96, 96);
    visuals.warn_fg_color = Color32::from_rgb(233, 176, 96);
    visuals.dark_mode = palette.kind == ThemeKind::Dark;
    visuals
}

/// Apply a palette to an egui context.
///
/// `egui` keeps one style per theme — a dark one and a light one — and
/// `Context::set_visuals` only changes the *currently active* one. Which one
/// that is depends on the system theme, which is not known yet when the
/// launcher starts. So both are set, and the theme is pinned to the one the
/// palette belongs to.
///
/// Without this the settings panel showed the default light widgets on top of
/// a dark window.
pub fn apply_visuals(ctx: &egui::Context, palette: &Palette) {
    let visuals = visuals(palette);
    ctx.set_visuals_of(egui::Theme::Dark, visuals.clone());
    ctx.set_visuals_of(egui::Theme::Light, visuals);

    // The built-in fade of an `Area` — every popup, every menu — asks for
    // the next frame **at once** for as long as the fade lasts (a bare
    // `request_repaint` per frame). On a machine whose frames are expensive
    // that is a stutter each time a menu opens. The launcher draws its own
    // fades where it wants them (the highlight glides, the tip of a resting
    // row fades in — both paced); the built-in one is switched off by
    // giving it no time at all.
    ctx.global_style_mut(|style| {
        style.animation_time = 0.001;
    });

    // The scroll bar of the result list.
    //
    // The default of `egui` is a bar that floats over the rows and is
    // invisible while it is not touched — so the user cannot tell that the
    // list can be dragged. Here it is a solid bar that is always there.
    //
    // The tooltips of `egui` wait for the pointer to be still, and the
    // waiting is a **loop**: as long as the pointer moves, every frame asks
    // for the next one at once (egui 0.37, `tooltip.rs` — "wait for mouse to
    // stop" → `request_repaint()`). A sweep through the result list then drew
    // a hundred frames a second, none of them wanted by the launcher, while
    // the mouse was held back to thirty messages a second. On a machine
    // whose frames take tens of milliseconds to reach the screen that is
    // what made the pointer feel heavy and the fan spin.
    //
    // With the switch off, the same code asks for one frame after the
    // tooltip delay instead, and every movement restarts that delay — so the
    // tooltip still appears only under a pointer that rests, but a moving
    // pointer costs no frame at all.
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            style.spacing.scroll = egui::style::ScrollStyle::solid();
            style.spacing.scroll.bar_width = 8.0;
            style.spacing.scroll.dormant_handle_opacity = 0.5;
            style.spacing.scroll.dormant_background_opacity = 0.08;
            // The bar of `egui` fades away when nothing happens for a while.
            // Here it stays: the user has to see that the list can be dragged.
            style.interaction.show_tooltips_only_when_still = false;
        });
    }

    ctx.set_theme(match palette.kind {
        ThemeKind::Dark => egui::ThemePreference::Dark,
        ThemeKind::Light => egui::ThemePreference::Light,
    });
}

/// Sizes used across the UI.
///
/// The numbers come from the design tokens of the family, so that this
/// program and its siblings stay in step. Only the measures that belong to
/// the launcher alone live here.
pub mod metrics {
    use egui::Vec2;
    use ideas_design::{component, radius, space, typography};

    /// Corner radius of the window and of the card.
    ///
    /// Square: the operating system decides the shape of a window, and a
    /// second shape painted on top of it only makes the two disagree.
    pub const CARD_RADIUS: u8 = radius::NONE;
    /// Corner radius of a row of the result list.
    pub const ROW_RADIUS: u8 = radius::LG;
    /// Corner radius of the letter that stands in for a missing icon.
    pub const BADGE_RADIUS: u8 = radius::MD;
    /// Height of one result row.
    pub const ROW_HEIGHT: f32 = component::ROW_HEIGHT;
    /// The empty space between two result rows.
    pub const ROW_GAP: f32 = component::ROW_GAP;
    /// Size of the icon inside a row.
    pub const ICON_SIZE: f32 = component::ICON_SIZE;
    /// Size of the letter badge that stands in for a missing icon.
    pub const BADGE_SIZE: f32 = component::BADGE_SIZE;
    /// Size of the search glyph.
    pub const SEARCH_GLYPH_SIZE: f32 = component::GLYPH_SIZE;
    /// Width of the column that lists the panes of the settings.
    pub const NAV_WIDTH: f32 = component::NAV_WIDTH;
    /// Height of a button in a list of panes or themes.
    pub const ITEM_HEIGHT: f32 = component::ITEM_HEIGHT;
    /// Inner padding of the card.
    pub const PAD: f32 = space::LG;
    /// Gap between the search row and the result list.
    pub const GAP: f32 = space::LG;
    /// Height of the search row.
    pub const SEARCH_HEIGHT: f32 = component::SEARCH_HEIGHT;
    /// Height of the footer.
    pub const FOOTER_HEIGHT: f32 = component::FOOTER_HEIGHT;
    /// The place the scroll bar of the result list takes.
    pub const SCROLLBAR_WIDTH: f32 = 12.0;
    /// The round corners of the menu of a row.
    pub const MENU_RADIUS: u8 = radius::MD;
    /// The space around the entries of the menu.
    pub const MENU_PADDING_X: f32 = space::XS;
    /// The space above and below the entries of the menu.
    pub const MENU_PADDING_Y: f32 = space::XS;
    /// The space between two entries of the menu.
    pub const MENU_ITEM_GAP: f32 = 2.0;
    /// Space between the title and the subtitle of a row.
    pub const TITLE_GAP: f32 = space::XS;
    /// Space between the icon and the text of a row.
    pub const ICON_GAP: f32 = space::LG;
    /// Space between the left edge of a row and its icon.
    pub const ROW_PAD_X: f32 = space::SM;
    /// Space between the right edge of a row and its text.
    pub const ROW_PAD_RIGHT: f32 = space::LG;
    /// Default window size.
    ///
    /// The window *is* the card: a square, opaque rectangle that reaches every
    /// edge of the client area, with the hairline drawn along it. Windows is
    /// told to keep its hands off the corners, so nothing of the desktop ever
    /// shows at them.
    pub const WINDOW_SIZE: Vec2 = Vec2::new(720.0, 520.0);
    /// Minimum window size.
    pub const MIN_WINDOW_SIZE: Vec2 = Vec2::new(420.0, 220.0);
    /// The size of the text that carries the ordinary content.
    pub const TEXT_BODY: f32 = typography::BODY;
    /// The size of the text that names a setting.
    pub const TEXT_LABEL: f32 = typography::LABEL;
    /// The size of a hint.
    pub const TEXT_CAPTION: f32 = typography::CAPTION;
    /// The size of the title of a pane.
    pub const TEXT_TITLE: f32 = typography::TITLE;
}

/// The character that goes on the letter badge of a result.
///
/// The first character of a title is not always a letter. A title may start
/// with a bracket, a quote or a digit, and a badge that shows a lone bracket
/// looks like a mistake rather than a label. Punctuation is therefore
/// skipped, and the first letter of the title is used instead.
pub fn badge_letter(title: &str) -> String {
    // A letter first — for a Chinese title that is the first ideograph.
    for ch in title.chars() {
        if ch.is_alphabetic() {
            return ch.to_uppercase().collect();
        }
    }
    // Then a digit.
    for ch in title.chars() {
        if ch.is_numeric() {
            return ch.to_string();
        }
    }
    // Nothing letter-like at all: the first character will have to do.
    title
        .chars()
        .next()
        .map(|ch| ch.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_owned())
}

/// The font family used for the Segoe icon glyphs.
pub const ICON_FONT_FAMILY: &str = crate::fonts::ICON_FONT_FAMILY;

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_core::theme::Theme;

    fn a_theme() -> Theme {
        Theme::default_theme()
    }

    #[test]
    fn the_badge_colour_is_stable_for_a_title() {
        let palette = Palette::from_theme(&a_theme());
        assert_eq!(
            badge_color(&palette, "Chrome"),
            badge_color(&palette, "Chrome"),
            "the same title must always get the same colour"
        );
    }

    #[test]
    fn different_titles_get_different_badges() {
        let palette = Palette::from_theme(&a_theme());
        assert_ne!(
            badge_color(&palette, "Chrome"),
            badge_color(&palette, "Firefox")
        );
        assert_ne!(
            badge_color(&palette, "Edge"),
            badge_color(&palette, "Word")
        );
    }

    #[test]
    fn the_badge_colour_follows_the_theme() {
        let dark = Palette::from_theme(&a_theme());
        let mut light_theme = a_theme();
        light_theme.kind = ThemeKind::Light;
        let light = Palette::from_theme(&light_theme);

        // The letter has to be readable on the card, so the dark theme gets a
        // brighter letter than the light one.
        let dark_badge = badge_color(&dark, "Chrome");
        let light_badge = badge_color(&light, "Chrome");
        assert!(
            dark_badge.gamma_multiply(1.0) != light_badge,
            "the badge must follow the theme: {dark_badge:?} vs {light_badge:?}"
        );
    }

    #[test]
    fn the_badge_letter_skips_leading_punctuation() {
        // A title that starts with a bracket must not end up with a bracket on
        // its badge.
        assert_eq!(badge_letter("（48条消息）一篇文章"), "条");
        assert_eq!(badge_letter("《深入理解计算机系统》"), "深");
        assert_eq!(badge_letter("Chrome"), "C");
        assert_eq!(badge_letter("reg"), "R");
        assert_eq!(badge_letter("7-Zip"), "Z");
        assert_eq!(badge_letter("  spaced"), "S");
    }

    #[test]
    fn the_badge_letter_falls_back_to_a_digit() {
        assert_eq!(badge_letter("7-Zip"), "Z");
        assert_eq!(badge_letter("123D Viewer"), "D");
        assert_eq!(badge_letter("《3D》"), "D");
        assert_eq!(badge_letter("42"), "4");
    }

    #[test]
    fn the_badge_letter_survives_an_empty_title() {
        assert_eq!(badge_letter(""), "?");
        assert_eq!(badge_letter("!!!"), "!");
    }
}
