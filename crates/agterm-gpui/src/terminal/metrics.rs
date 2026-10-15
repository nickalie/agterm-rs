//! Cell metrics from the configured font, and the grid's geometry inside a terminal view (padding, balance).
//!
//! Ported from qwertty-term (<https://github.com/joshka/qwertty-term>, MIT, Copyright (c) 2026 Josh `McKinney`):
//! `qwertty-term-font/src/metrics.rs` and `qwertty-term-renderer/src/size.rs`, themselves ports of Ghostty's
//! `src/font/Metrics.zig` and `src/renderer/size.zig` (MIT, Copyright (c) 2024 Mitchell Hashimoto, Ghostty
//! contributors). Adapted: the face is measured through GPUI's text system, the modifier set (`adjust-*`) and the
//! icon heights are not carried, and the geometry works in device pixels with logical conversions for GPUI.
//!
//! Units: Ghostty measures a point at `dpi / 72` pixels with a 96-DPI default on Linux (`font.face.default_dpi`), so
//! a 12-point font is 16 logical pixels; everything is computed and rounded in device pixels (logical × the window's
//! scale), as Ghostty does, and converted back to logical pixels for GPUI.
//!
//! Residual (plan > Task 13 result): GPUI's public text system exposes neither the font's line gap nor its underline
//! metrics, so the face's line gap counts as 0 (the cell is shorter than libghostty's for a font whose `hhea` line gap
//! is not 0; most monospace fonts have none) and underline and strikethrough come from Ghostty's own estimates.

use gpui_kit::{FontId, Pixels, TextSystem, px};

use agterm_core::ghostty_config::{self, PaddingBalance};

/// Ghostty's default DPI on Linux: a point is `96 / 72` pixels.
pub const DEFAULT_DPI: f32 = 96.0;

/// The logical pixels of `points` (Ghostty's `DesiredSize.pixels` at the default DPI).
#[must_use]
pub fn points_to_px(points: f32) -> f32 {
    points * DEFAULT_DPI / 72.0
}

/// A face's metrics in device pixels, as measured (Ghostty's `FaceMetrics`; +Y is up, relative to the baseline).
/// `None` fields are estimated the way Ghostty estimates them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FaceMetrics {
    /// The widest advance among the printable ASCII characters.
    pub cell_width: f64,
    pub ascent: f64,
    /// Below the baseline, so usually negative.
    pub descent: f64,
    pub line_gap: f64,
    /// The top of the underline stroke.
    pub underline_position: Option<f64>,
    pub underline_thickness: Option<f64>,
    /// The top of the strikethrough stroke.
    pub strikethrough_position: Option<f64>,
    pub strikethrough_thickness: Option<f64>,
    pub cap_height: Option<f64>,
    pub ex_height: Option<f64>,
}

fn positive(value: Option<f64>) -> Option<f64> {
    value.filter(|value| *value > 0.0)
}

impl FaceMetrics {
    /// `ascent - descent + line_gap`.
    #[must_use]
    pub fn line_height(&self) -> f64 {
        self.ascent - self.descent + self.line_gap
    }

    /// The cap height, else 75 % of the ascent.
    #[must_use]
    pub fn cap_height(&self) -> f64 {
        positive(self.cap_height).unwrap_or(0.75 * self.ascent)
    }

    /// The ex height, else 75 % of the cap height.
    #[must_use]
    pub fn ex_height(&self) -> f64 {
        positive(self.ex_height).unwrap_or(0.75 * self.cap_height())
    }

    /// The underline thickness, else 15 % of the ex height.
    #[must_use]
    pub fn underline_thickness(&self) -> f64 {
        positive(self.underline_thickness).unwrap_or(0.15 * self.ex_height())
    }

    /// The strikethrough thickness, else the underline's.
    #[must_use]
    pub fn strikethrough_thickness(&self) -> f64 {
        positive(self.strikethrough_thickness).unwrap_or_else(|| self.underline_thickness())
    }

    /// The underline position, else one underline thickness below the baseline.
    #[must_use]
    pub fn underline_position(&self) -> f64 {
        self.underline_position.unwrap_or(-self.underline_thickness())
    }

    /// The strikethrough position, else centered on half the ex height.
    #[must_use]
    pub fn strikethrough_position(&self) -> f64 {
        self.strikethrough_position
            .unwrap_or(f64::midpoint(self.ex_height(), self.strikethrough_thickness()))
    }
}

/// The cell and the placement of its decorations, in device pixels (Ghostty's `Metrics`). Positions are from the top
/// of the cell, except the baseline, which is from its bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellMetrics {
    pub cell_width: u32,
    pub cell_height: u32,
    /// From the bottom of the cell.
    pub cell_baseline: u32,
    pub underline_position: u32,
    pub underline_thickness: u32,
    pub strikethrough_position: u32,
    pub strikethrough_thickness: u32,
    /// May be negative: above the cell.
    pub overline_position: i32,
    pub overline_thickness: u32,
    pub box_thickness: u32,
    pub cursor_thickness: u32,
    pub cursor_height: u32,
}

/// A non-negative, rounded device-pixel value as a `u32`.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=u32::MAX and integral before the cast"
)]
fn device(value: f64) -> u32 {
    value.clamp(0.0, f64::from(u32::MAX)) as u32
}

impl CellMetrics {
    /// Ghostty's `Metrics.calc`: the cell rounded from the face, the baseline centering the face in it.
    #[must_use]
    pub fn calc(face: &FaceMetrics) -> Self {
        let face_width = face.cell_width;
        let face_height = face.line_height();
        // half away from zero: the cell is at most half a pixel off the font's design, at any DPI
        let cell_width = face_width.round();
        let cell_height = face_height.round();
        // half the line gap above the face and half below; the baseline centers the face in the rounded cell
        let face_baseline = face.line_gap / 2.0 - face.descent;
        let cell_baseline = (face_baseline - (cell_height - face_height) / 2.0).round();
        let top_to_baseline = cell_height - cell_baseline;
        let underline_thickness = face.underline_thickness().ceil().max(1.0);
        let strikethrough_thickness = face.strikethrough_thickness().ceil().max(1.0);
        let underline_position = (top_to_baseline - face.underline_position()).round();
        let strikethrough_position = (top_to_baseline - face.strikethrough_position()).round();
        Self {
            cell_width: device(cell_width).max(1),
            cell_height: device(cell_height).max(1),
            cell_baseline: device(cell_baseline),
            underline_position: device(underline_position),
            underline_thickness: device(underline_thickness).max(1),
            strikethrough_position: device(strikethrough_position),
            strikethrough_thickness: device(strikethrough_thickness).max(1),
            overline_position: 0,
            overline_thickness: device(underline_thickness).max(1),
            box_thickness: device(underline_thickness).max(1),
            cursor_thickness: 1,
            cursor_height: device(cell_height).max(1),
        }
    }

    /// The pixels from the top of the cell to the baseline.
    #[must_use]
    pub fn baseline_from_top(&self) -> u32 {
        self.cell_height.saturating_sub(self.cell_baseline)
    }
}

/// The printable ASCII characters whose widest advance is the cell width (Ghostty measures them all).
const PRINTABLE_ASCII: std::ops::RangeInclusive<char> = ' '..='~';

/// Measures the face `font_id` at `size` device pixels through GPUI's text system (its metrics scale linearly, so a
/// device-pixel size gives device-pixel metrics).
#[must_use]
pub fn measure(text: &TextSystem, font_id: FontId, size: Pixels) -> FaceMetrics {
    let cell_width = PRINTABLE_ASCII
        .filter_map(|ch| text.advance(font_id, size, ch).ok())
        .map(|advance| f64::from(f32::from(advance.width)))
        .fold(0.0, f64::max);
    FaceMetrics {
        cell_width,
        ascent: f64::from(f32::from(text.ascent(font_id, size))),
        // GPUI's descent is below the baseline already (negative); keep Ghostty's sign whatever the backend says
        descent: -f64::from(f32::from(text.descent(font_id, size))).abs(),
        line_gap: 0.0,
        underline_position: None,
        underline_thickness: None,
        strikethrough_position: None,
        strikethrough_thickness: None,
        cap_height: Some(f64::from(f32::from(text.cap_height(font_id, size)))),
        ex_height: Some(f64::from(f32::from(text.x_height(font_id, size)))),
    }
}

/// The cell metrics of `font_id` at `points` on a display of `scale`.
#[must_use]
pub fn cell_metrics(text: &TextSystem, font_id: FontId, points: f32, scale: f32) -> CellMetrics {
    CellMetrics::calc(&measure(text, font_id, px(points_to_px(points) * scale)))
}

/// Padding around the grid, in device pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Padding {
    pub top: u32,
    pub bottom: u32,
    pub left: u32,
    pub right: u32,
}

impl Padding {
    /// The configured `window-padding-x`/`-y` (points) at `scale`: Ghostty's `scaledPadding`, each side floored.
    #[must_use]
    pub fn from_config(padding: &ghostty_config::WindowPadding, scale: f32) -> Self {
        let side = |points: u32| {
            #[expect(clippy::cast_precision_loss, reason = "a padding in points is far below 2^24")]
            let points = points as f32;
            device(f64::from((points_to_px(points) * scale).floor()))
        };
        Self {
            top: side(padding.y.start),
            bottom: side(padding.y.end),
            left: side(padding.x.start),
            right: side(padding.x.end),
        }
    }
}

/// Where the grid sits in a view of `screen` device pixels: its size in cells and the padding around it (the
/// configured padding, or the balanced one with `window-padding-balance`; Ghostty's `Size.balancePadding`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
    pub padding: Padding,
}

/// Cells that fit in `space` pixels of `cell` each, at least one (Ghostty's `GridSize.update`).
fn fit(space: u32, cell: u32) -> u16 {
    u16::try_from(space / cell.max(1)).unwrap_or(u16::MAX).max(1)
}

impl Geometry {
    /// The grid of `screen` (width, height in device pixels) with cells of `cell`.
    #[must_use]
    pub fn new(screen: (u32, u32), cell: &CellMetrics, explicit: Padding, balance: PaddingBalance) -> Self {
        let grid = |padding: Padding| {
            (
                fit(screen.0.saturating_sub(padding.left + padding.right), cell.cell_width),
                fit(screen.1.saturating_sub(padding.top + padding.bottom), cell.cell_height),
            )
        };
        let (cols, rows) = grid(explicit);
        let padding = match balance {
            PaddingBalance::False => explicit,
            PaddingBalance::True | PaddingBalance::Equal => {
                let balanced = balanced(screen, (cols, rows), cell);
                if balance == PaddingBalance::True {
                    cap_top(balanced, explicit, cell)
                } else {
                    balanced
                }
            }
        };
        Self { cols, rows, padding }
    }

    /// The grid's size in device pixels.
    #[must_use]
    pub fn grid_pixels(&self, cell: &CellMetrics) -> (u32, u32) {
        (
            u32::from(self.cols) * cell.cell_width,
            u32::from(self.rows) * cell.cell_height,
        )
    }
}

/// Ghostty's `Padding.balanced`: the space the grid leaves split evenly on both sides of each axis, floored.
fn balanced(screen: (u32, u32), grid: (u16, u16), cell: &CellMetrics) -> Padding {
    let horizontal = screen.0.saturating_sub(u32::from(grid.0) * cell.cell_width) / 2;
    let vertical = screen.1.saturating_sub(u32::from(grid.1) * cell.cell_height) / 2;
    Padding {
        top: vertical,
        bottom: vertical,
        left: horizontal,
        right: horizontal,
    }
}

/// `window-padding-balance = true`: the top padding is capped at the explicit horizontal padding's mean plus half a
/// cell, the rest moving to the bottom, so the first row does not drift down in a tall view.
fn cap_top(mut padding: Padding, explicit: Padding, cell: &CellMetrics) -> Padding {
    let max_top = (explicit.left + explicit.right + cell.cell_width) / 2;
    let shift = padding.top.saturating_sub(max_top);
    padding.top -= shift;
    padding.bottom += shift;
    padding
}
