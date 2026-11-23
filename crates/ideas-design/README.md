# ideas-design

The design tokens of the IDEAS program family.

Every number that carries a meaning in the interface comes from here, so that
the launcher and its siblings look like one family instead of a collection of
programs that happen to share a logo.

The crate has **no dependencies** on purpose: the tokens are plain numbers,
so the same scale works in Rust, in a web page and in a design tool.

> **The full design specification lives in
> [`docs/design-system/DESIGN.md`](../../docs/design-system/DESIGN.md).**
> It covers the shape of the window, the colour roles, every component's
> measures, the interaction rules and the things that must not be done.
> This file is only the quick reference to the numbers.

## The three rules

**A scale, not a set of values.** The spacing is a scale of six, the type is
a scale of six. A program picks *a step* of the scale, never a number of its
own.

**The colour is a role, not a colour.** `text_dim` is "the second level of
text", whatever colour that happens to be in the theme. The themes fill the
roles in.

**Nothing is invented twice.** If a measure is not in here, it does not
belong in the interface — it belongs in the tokens.

## The scales

### Spacing — `space`

| Token | Points | Used for |
|---|---|---|
| `XS` | 4 | between a label and the thing it describes |
| `SM` | 8 | between two elements of the same group |
| `MD` | 12 | the inner padding of a panel |
| `LG` | 14 | the inner padding of the card |
| `XL` | 20 | between two groups |
| `XXL` | 28 | above a section that starts a new page |

### Corner radii — `radius`

| Token | Points | Used for |
|---|---|---|
| `NONE` | 0 | the window and the card |
| `SM` | 6 | a small element, such as a letter badge |
| `MD` | 8 | a control |
| `LG` | 10 | a row of a list |
| `XL` | 14 | a card that floats over something else |

The window and the card are **square**. The operating system decides the
shape of a window, and a second shape painted on top of it only makes the two
disagree — that disagreement is what produced the dark smudges in the corners
of the launcher.

### Type — `typography`

| Token | Points | Used for |
|---|---|---|
| `CAPTION` | 11 | a hint, a path, a footnote |
| `LABEL` | 12 | the name of a setting |
| `BODY` | 13 | ordinary text, and the titles of the results |
| `SUBTITLE` | 14 | the second line of a result |
| `TITLE` | 16 | the title of a pane |
| `DISPLAY` | 18 | the name of the program |

### Measures — `component`

| Token | Points | Used for |
|---|---|---|
| `ROW_HEIGHT` | 58 | one row of a list |
| `ROW_GAP` | 4 | the empty space between two rows |
| `ICON_SIZE` | 32 | the icon in a row |
| `BADGE_SIZE` | 28 | the letter that stands in for a missing icon |
| `GLYPH_SIZE` | 22 | a glyph that stands for an action |
| `SEARCH_HEIGHT` | 50 | the search field |
| `FOOTER_HEIGHT` | 30 | the bar at the bottom of the window |
| `NAV_WIDTH` | 168 | the column that lists the panes of a settings page |
| `ITEM_HEIGHT` | 30 | a button in a list of panes or themes |

### Colour roles — `ColorRoles`

`surface`, `surface_alt`, `border`, `text`, `text_dim`, `text_faint`,
`glyph`, `accent`. A theme fills them in; the programs only ever ask for the
role.

### Motion — `motion`

`FADE` is how long the window takes to fade in, `SLIDE` is how far it moves
while it does.

## Using them

```rust
use ideas_design::{component, radius, space, typography};

ui.label(RichText::new("热键").size(typography::LABEL));
ui.add_space(space::SM);
```

The launcher itself keeps its own measures in
`launcher-app::theme::metrics`, but those are only the ones that belong to
the launcher alone — everything else comes from here.
