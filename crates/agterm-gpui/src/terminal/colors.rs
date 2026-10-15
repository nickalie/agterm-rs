//! The colors cells and the cursor are drawn with: Ghostty's renderer rules (`src/renderer/generic.zig`,
//! `rebuildRow` and the cursor color, MIT, Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors) over a
//! frame's colors (the theme with the program's OSC 4/10/11/12 overrides) and the config's selection, search and
//! cursor colors.

use agterm_core::chrome;
use agterm_core::ghostty_config::{GhosttyConfig, TerminalColor};
use agterm_term::{Cell, CellFlags, Colors, Rgb};
use gpui_kit::{Hsla, Rgba};

/// GPUI's color for `rgb` at `alpha` (0–255).
#[must_use]
pub fn hsla(rgb: Rgb, alpha: u8) -> Hsla {
    Rgba {
        r: f32::from(rgb.r) / 255.0,
        g: f32::from(rgb.g) / 255.0,
        b: f32::from(rgb.b) / 255.0,
        a: f32::from(alpha) / 255.0,
    }
    .into()
}

/// An opacity (0.0–1.0) as an alpha byte, rounded up as Ghostty's `faint-opacity` is.
#[must_use]
pub fn alpha(opacity: f64) -> u8 {
    let scaled = (opacity.clamp(0.0, 1.0) * 255.0).ceil();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=255"
    )]
    let byte = scaled as u8;
    byte
}

/// A config color that may follow the cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pick {
    Color(Rgb),
    CellForeground,
    CellBackground,
}

impl From<TerminalColor> for Pick {
    fn from(color: TerminalColor) -> Self {
        match color {
            TerminalColor::Color(color) => Self::Color(rgb(color)),
            TerminalColor::CellForeground => Self::CellForeground,
            TerminalColor::CellBackground => Self::CellBackground,
        }
    }
}

fn rgb(color: chrome::Rgb) -> Rgb {
    Rgb::new(color.r, color.g, color.b)
}

impl Pick {
    /// The color for a cell whose style colors are `fg` and `bg`, swapped for an inverse cell (Ghostty: the opposite
    /// cell color when the cell is reversed).
    fn resolve(self, inverse: bool, fg: Rgb, bg: Rgb) -> Rgb {
        match (self, inverse) {
            (Self::Color(color), _) => color,
            (Self::CellForeground, false) | (Self::CellBackground, true) => fg,
            (Self::CellForeground, true) | (Self::CellBackground, false) => bg,
        }
    }
}

/// The config's colors the renderer applies over a frame's (Ghostty's derived renderer config).
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub selection_foreground: Option<Pick>,
    pub selection_background: Option<Pick>,
    pub search_foreground: Pick,
    pub search_background: Pick,
    pub search_selected_foreground: Pick,
    pub search_selected_background: Pick,
    pub cursor_color: Option<Pick>,
    pub cursor_text: Option<Pick>,
    /// `cursor-opacity` as an alpha byte.
    pub cursor_alpha: u8,
    /// `faint-opacity` (Ghostty's default 0.5) as an alpha byte.
    pub faint_alpha: u8,
    /// `background-opacity` as an alpha byte, for the default background.
    pub background_alpha: u8,
}

impl Default for Palette {
    fn default() -> Self {
        Self::from_config(&GhosttyConfig::default())
    }
}

/// Ghostty's search colors (`search-*`), which agterm's config does not carry.
const SEARCH_FOREGROUND: Rgb = Rgb::new(0, 0, 0);
const SEARCH_BACKGROUND: Rgb = Rgb::new(0xFF, 0xE0, 0x82);
const SEARCH_SELECTED_BACKGROUND: Rgb = Rgb::new(0xF2, 0xA5, 0x7E);
/// Ghostty's `faint-opacity` default.
const FAINT_OPACITY: f64 = 0.5;

impl Palette {
    #[must_use]
    pub fn from_config(config: &GhosttyConfig) -> Self {
        Self {
            selection_foreground: config.selection.foreground.map(Pick::from),
            selection_background: config.selection.background.map(Pick::from),
            search_foreground: Pick::Color(SEARCH_FOREGROUND),
            search_background: Pick::Color(SEARCH_BACKGROUND),
            search_selected_foreground: Pick::Color(SEARCH_FOREGROUND),
            search_selected_background: Pick::Color(SEARCH_SELECTED_BACKGROUND),
            cursor_color: config.cursor.color.map(Pick::from),
            cursor_text: config.cursor.text.map(Pick::from),
            cursor_alpha: alpha(config.cursor.opacity),
            faint_alpha: alpha(FAINT_OPACITY),
            background_alpha: alpha(config.background_opacity),
        }
    }
}

/// How a cell is marked, in Ghostty's precedence: the selection over a search match.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Mark {
    #[default]
    None,
    Selection,
    Search,
    SearchSelected,
}

/// What a cell is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellPaint {
    /// The background to draw; `None` lets the default background show.
    pub background: Option<Rgb>,
    pub foreground: Rgb,
    /// The text's alpha (faint text is dimmed).
    pub alpha: u8,
}

/// U+2588 FULL BLOCK covers its cell: Ghostty uses its foreground as the cell's background, so the padding extends
/// right.
fn is_covering(text: &str) -> bool {
    text == "\u{2588}"
}

/// The colors of `cell` (its text `text`) marked `mark`, per `rebuildRow`.
#[must_use]
pub fn cell_paint(cell: &Cell, text: &str, mark: Mark, colors: &Colors, palette: &Palette) -> CellPaint {
    let inverse = cell.flags.contains(CellFlags::INVERSE);
    let fg_style = cell.foreground.unwrap_or(colors.foreground);
    let bg_style = cell.background;
    let final_bg = bg_style.unwrap_or(colors.background);
    let pick = |pick: Pick| pick.resolve(inverse, fg_style, final_bg);
    let (background, foreground) = match mark {
        Mark::Selection => (
            Some(palette.selection_background.map_or(colors.foreground, pick)),
            palette.selection_foreground.map_or(colors.background, pick),
        ),
        Mark::Search => (Some(pick(palette.search_background)), pick(palette.search_foreground)),
        Mark::SearchSelected => (
            Some(pick(palette.search_selected_background)),
            pick(palette.search_selected_foreground),
        ),
        Mark::None => {
            // the inverse flag and a covering glyph each swap in the foreground; both cancel out
            let background = if inverse == is_covering(text) {
                bg_style
            } else {
                Some(fg_style)
            };
            (background, if inverse { final_bg } else { fg_style })
        }
    };
    CellPaint {
        background,
        foreground,
        alpha: if cell.flags.contains(CellFlags::FAINT) {
            palette.faint_alpha
        } else {
            u8::MAX
        },
    }
}

/// The cursor's color over `cell` (the cell under it, `None` when blank): the program's OSC 12 or a fixed
/// `cursor-color` (both in the frame's colors), then a `cursor-color` that follows the cell, then the foreground.
#[must_use]
pub fn cursor_color(cell: Option<&Cell>, colors: &Colors, palette: &Palette) -> Rgb {
    if let Some(color) = colors.cursor {
        return color;
    }
    match palette.cursor_color {
        Some(pick) => cursor_pick(pick, cell, colors),
        None => colors.foreground,
    }
}

/// The color of the text under a block cursor: `cursor-text`, else the background.
#[must_use]
pub fn cursor_text(cell: Option<&Cell>, colors: &Colors, palette: &Palette) -> Rgb {
    match palette.cursor_text {
        Some(pick) => cursor_pick(pick, cell, colors),
        None => colors.background,
    }
}

fn cursor_pick(pick: Pick, cell: Option<&Cell>, colors: &Colors) -> Rgb {
    let inverse = cell.is_some_and(|cell| cell.flags.contains(CellFlags::INVERSE));
    let fg = cell.and_then(|cell| cell.foreground).unwrap_or(colors.foreground);
    let bg = cell.and_then(|cell| cell.background).unwrap_or(colors.background);
    pick.resolve(inverse, fg, bg)
}
