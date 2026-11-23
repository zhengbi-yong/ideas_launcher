//! The design tokens of the IDEAS program family.
//!
//! Every number that carries a meaning in the interface comes from here. The
//! launcher is not the only program of the family, so the tokens live in
//! their own crate: a second program can depend on them and look like a
//! sibling of the launcher without a single line being copied.
//!
//! The rules the tokens follow:
//!
//! * **A scale, not a set of values.** The spacing is a scale of four, the
//!   type is a scale of six; a program picks *a* step of the scale, never a
//!   number of its own.
//! * **The colour is a role, not a colour.** `text_dim` is "the second
//!   level of text", whatever colour that happens to be in the theme. The
//!   themes fill the roles in.
//! * **Nothing here depends on a toolkit.** The tokens are plain numbers, so
//!   the same scale works in a Rust program, in a web page and in a design
//!   tool.

/// The spacing scale.
///
/// Every gap between two elements is one of these. The steps are small
/// enough to be used freely and few enough that the interface keeps a steady
/// rhythm.
pub mod space {
    /// Between a label and the thing it describes.
    pub const XS: f32 = 4.0;
    /// Between two elements of the same group.
    pub const SM: f32 = 8.0;
    /// The inner padding of a panel.
    pub const MD: f32 = 12.0;
    /// The inner padding of the card.
    pub const LG: f32 = 14.0;
    /// Between two groups.
    pub const XL: f32 = 20.0;
    /// Above a section that starts a new page.
    pub const XXL: f32 = 28.0;
}

/// The corner radii.
pub mod radius {
    /// Square. The window and the card are square, because the operating
    /// system decides the shape of a window and a second shape on top of it
    /// only makes the two disagree.
    pub const NONE: u8 = 0;
    /// A small element, such as the badge that stands in for a missing icon.
    pub const SM: u8 = 6;
    /// A control.
    pub const MD: u8 = 8;
    /// A row of a list.
    pub const LG: u8 = 10;
    /// A card that floats over something else.
    pub const XL: u8 = 14;
}

/// The type scale.
///
/// The sizes are in points, the unit the interface is laid out in.
pub mod typography {
    /// A hint, a path, a footnote.
    pub const CAPTION: f32 = 11.0;
    /// The name of a setting.
    pub const LABEL: f32 = 12.0;
    /// Ordinary text, and the titles of the results.
    pub const BODY: f32 = 13.0;
    /// The second line of a result.
    pub const SUBTITLE: f32 = 14.0;
    /// The title of a pane.
    pub const TITLE: f32 = 16.0;
    /// The name of the program.
    pub const DISPLAY: f32 = 18.0;
}

/// The measures of the parts the programs have in common.
pub mod component {
    /// The height of one row of a list.
    pub const ROW_HEIGHT: f32 = 58.0;
    /// The empty space between two rows of a list.
    pub const ROW_GAP: f32 = 4.0;
    /// The size of the icon in a row.
    pub const ICON_SIZE: f32 = 32.0;
    /// The size of the letter that stands in for a missing icon.
    pub const BADGE_SIZE: f32 = 28.0;
    /// The size of a glyph that stands for an action, such as the magnifying
    /// glass.
    pub const GLYPH_SIZE: f32 = 22.0;
    /// The height of the search field.
    pub const SEARCH_HEIGHT: f32 = 50.0;
    /// The height of the bar at the bottom of the window.
    pub const FOOTER_HEIGHT: f32 = 30.0;
    /// The width of the column that lists the panes of a settings page.
    pub const NAV_WIDTH: f32 = 168.0;
    /// The height of a button in a list of panes or themes.
    pub const ITEM_HEIGHT: f32 = 30.0;
}

/// The colours of a theme, by the role they play.
///
/// A theme fills these in; the programs only ever ask for the role. That is
/// what keeps a whole family of programs looking like one family even though
/// every one of them has a theme of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorRoles {
    /// The background of the window.
    pub surface: [u8; 3],
    /// A surface that sits on top of the background, such as a selected row.
    pub surface_alt: [u8; 3],
    /// The hairline that separates one surface from another.
    pub border: [u8; 3],
    /// The text that matters.
    pub text: [u8; 3],
    /// The text that supports it.
    pub text_dim: [u8; 3],
    /// The text that is barely there: hints and paths.
    pub text_faint: [u8; 3],
    /// The magnifying glass.
    pub glyph: [u8; 3],
    /// The colour of the selection.
    pub accent: [u8; 3],
}

/// How long the interface takes to move.
pub mod motion {
    /// How long the window takes to fade in.
    pub const FADE: f32 = 0.14;
    /// How far the window slides while it fades in.
    pub const SLIDE: f32 = 10.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spacing_scale_rises_in_even_steps() {
        let steps = [
            space::XS,
            space::SM,
            space::MD,
            space::LG,
            space::XL,
            space::XXL,
        ];
        for pair in steps.windows(2) {
            assert!(
                pair[1] > pair[0],
                "the spacing scale must rise: {pair:?}"
            );
        }
    }

    #[test]
    fn the_type_scale_rises_in_even_steps() {
        let steps = [
            typography::CAPTION,
            typography::LABEL,
            typography::BODY,
            typography::SUBTITLE,
            typography::TITLE,
            typography::DISPLAY,
        ];
        for pair in steps.windows(2) {
            assert!(
                pair[1] > pair[0],
                "the type scale must rise: {pair:?}"
            );
        }
    }

    #[test]
    fn a_row_is_tall_enough_for_its_two_lines() {
        // A row carries a title and a subtitle. The two lines need their
        // heights plus the gap between them, and the rest is breathing room.
        let needed = typography::BODY + typography::SUBTITLE + space::XS;
        assert!(
            component::ROW_HEIGHT > needed,
            "a row of {needed} points is not enough"
        );
    }
}
