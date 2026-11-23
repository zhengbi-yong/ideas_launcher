//! Writes the design tokens of the family to a JSON file.
//!
//! The tokens are exported from the code rather than written down by hand, so
//! the file can never drift away from what the programs actually use. Run it
//! with:
//!
//! ```text
//! cargo run --release -p ideas-launcher --bin export-design-tokens
//! ```
//!
//! The result is `docs/design-system/tokens.json`.

use std::fmt::Write as _;

use ideas_design::{component, motion, radius, space, typography};
use launcher_core::theme::Theme;

/// The roles of a colour, in the order they are written out.
const COLOR_ROLES: [&str; 8] = [
    "surface",
    "surface_alt",
    "border",
    "text",
    "text_dim",
    "text_faint",
    "glyph",
    "accent",
];

fn main() {
    let mut out = String::new();

    out.push_str("{\n");
    out.push_str("  \"$schema\": \"https://design.ideas.internal/tokens/v1\",\n");
    out.push_str("  \"name\": \"IDEAS\",\n");
    out.push_str(
        "  \"note\": \"Generated from the code by `cargo run -p ideas-launcher --bin export-design-tokens`. Do not edit by hand.\",\n",
    );

    // ---- spacing --------------------------------------------------------
    out.push_str("  \"space\": {\n");
    for (name, value) in [
        ("XS", space::XS),
        ("SM", space::SM),
        ("MD", space::MD),
        ("LG", space::LG),
        ("XL", space::XL),
        ("XXL", space::XXL),
    ] {
        writeln!(out, "    \"{name}\": {value},").unwrap();
    }
    out.push_str("  },\n");

    // ---- radii ----------------------------------------------------------
    out.push_str("  \"radius\": {\n");
    for (name, value) in [
        ("NONE", radius::NONE),
        ("SM", radius::SM),
        ("MD", radius::MD),
        ("LG", radius::LG),
        ("XL", radius::XL),
    ] {
        writeln!(out, "    \"{name}\": {value},").unwrap();
    }
    out.push_str("  },\n");

    // ---- type -----------------------------------------------------------
    out.push_str("  \"typography\": {\n");
    for (name, value) in [
        ("CAPTION", typography::CAPTION),
        ("LABEL", typography::LABEL),
        ("BODY", typography::BODY),
        ("SUBTITLE", typography::SUBTITLE),
        ("TITLE", typography::TITLE),
        ("DISPLAY", typography::DISPLAY),
    ] {
        writeln!(out, "    \"{name}\": {value},").unwrap();
    }
    out.push_str("  },\n");

    // ---- components -----------------------------------------------------
    out.push_str("  \"component\": {\n");
    for (name, value) in [
        ("ROW_HEIGHT", component::ROW_HEIGHT),
        ("ROW_GAP", component::ROW_GAP),
        ("ICON_SIZE", component::ICON_SIZE),
        ("BADGE_SIZE", component::BADGE_SIZE),
        ("GLYPH_SIZE", component::GLYPH_SIZE),
        ("SEARCH_HEIGHT", component::SEARCH_HEIGHT),
        ("FOOTER_HEIGHT", component::FOOTER_HEIGHT),
        ("NAV_WIDTH", component::NAV_WIDTH),
        ("ITEM_HEIGHT", component::ITEM_HEIGHT),
    ] {
        writeln!(out, "    \"{name}\": {value},").unwrap();
    }
    out.push_str("  },\n");

    // ---- motion ---------------------------------------------------------
    out.push_str("  \"motion\": {\n");
    writeln!(out, "    \"FADE\": {},", motion::FADE).unwrap();
    writeln!(out, "    \"SLIDE\": {}", motion::SLIDE).unwrap();
    out.push_str("  },\n");

    // ---- themes ---------------------------------------------------------
    out.push_str("  \"themes\": [\n");
    let themes = Theme::builtin_names();
    for (index, name) in themes.iter().enumerate() {
        let theme = Theme::resolve(name);
        let roles = ideas_design::ColorRoles {
            surface: theme.colors.surface,
            surface_alt: theme.colors.surface_alt,
            border: theme.colors.border,
            text: theme.colors.text,
            text_dim: theme.colors.text_dim,
            text_faint: theme.colors.text_faint,
            glyph: theme.colors.glyph,
            accent: theme.colors.accent,
        };
        out.push_str("    {\n");
        writeln!(out, "      \"name\": \"{}\",", theme.name).unwrap();
        writeln!(
            out,
            "      \"kind\": \"{}\",",
            match theme.kind {
                launcher_core::theme::ThemeKind::Dark => "dark",
                launcher_core::theme::ThemeKind::Light => "light",
            }
        )
        .unwrap();
        out.push_str("      \"colors\": {\n");
        for (role_index, role) in COLOR_ROLES.iter().enumerate() {
            let value = match role_index {
                0 => roles.surface,
                1 => roles.surface_alt,
                2 => roles.border,
                3 => roles.text,
                4 => roles.text_dim,
                5 => roles.text_faint,
                6 => roles.glyph,
                _ => roles.accent,
            };
            writeln!(
                out,
                "        \"{role}\": \"#{:02X}{:02X}{:02X}\",",
                value[0], value[1], value[2]
            )
            .unwrap();
        }
        out.push_str("      }\n");
        let comma = if index + 1 == themes.len() { "" } else { "," };
        out.push_str(&format!("    }}{comma}\n"));
    }
    out.push_str("  ]\n}\n");

    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "docs/design-system/tokens.json".to_string());
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, out).expect("the tokens could not be written");
    println!("the tokens were written to {path}");
}
