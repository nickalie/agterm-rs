//! The helpers every sprite family shares: line thicknesses, shades, cell fractions and the lines and fills built on
//! them (Ghostty's `src/font/sprite/draw/common.zig`, through qwertty-term's `common.rs`), and the few numeric
//! conversions the drawing needs, each in one place.

use super::super::metrics::CellMetrics;
use super::canvas::Canvas;

/// A `u32` pixel count as a signed coordinate (saturating; cells are far below `i32::MAX`).
pub(super) fn int(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// A signed coordinate as a pixel count, negatives as 0.
pub(super) fn count(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

/// The nearest whole pixel (half away from zero), saturating.
#[expect(
    clippy::cast_possible_truncation,
    reason = "rounded and clamped to the i32 range before the cast"
)]
pub(super) fn round(value: f64) -> i32 {
    value.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// Rounds down to a whole pixel, saturating.
#[expect(
    clippy::cast_possible_truncation,
    reason = "floored and clamped to the i32 range before the cast"
)]
pub(super) fn floor(value: f64) -> i32 {
    value.floor().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// Rounds up to a whole pixel, saturating.
#[expect(
    clippy::cast_possible_truncation,
    reason = "ceiled and clamped to the i32 range before the cast"
)]
pub(super) fn ceil(value: f64) -> i32 {
    value.ceil().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// A coverage in `0.0..=255.0` as a byte, rounded.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rounded and clamped to 0..=255 before the cast"
)]
pub(super) fn byte(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// A path coordinate for the rasterizer, which works in `f32`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "sprite coordinates are a few hundred pixels at most"
)]
pub(super) fn single(value: f64) -> f32 {
    value as f32
}

/// The thickness of a line, relative to the cell's box thickness (Ghostty's `super_light` draws nothing here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Thickness {
    Light,
    /// Twice the box thickness.
    Heavy,
}

impl Thickness {
    /// The line's height in pixels for a box thickness of `base`.
    pub(super) fn height(self, base: u32) -> u32 {
        match self {
            Self::Light => base,
            Self::Heavy => base * 2,
        }
    }
}

/// The coverages of the shaded blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shade {
    Light,
    Medium,
    Dark,
    On,
}

impl Shade {
    pub(super) fn alpha(self) -> u8 {
        match self {
            Self::Light => 0x40,
            Self::Medium => 0x80,
            Self::Dark => 0xc0,
            Self::On => 0xff,
        }
    }
}

/// Full coverage.
pub(super) const ON: u8 = 0xff;

/// Which quadrants of a cell a figure covers, as bits 0–3: top-left, top-right, bottom-left, bottom-right.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Quads(u8);

impl Quads {
    pub(super) const fn bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Whether quadrant `index` (0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right) is covered.
    pub(super) fn has(self, index: usize) -> bool {
        index < 4 && self.0 & (1 << index) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Corner {
    Tl,
    Tr,
    Bl,
    Br,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Edge {
    Top,
    Left,
    Bottom,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Horizontal {
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Vertical {
    Top,
    Bottom,
    Middle,
}

/// Where a figure sits in a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Alignment {
    pub horizontal: Horizontal,
    pub vertical: Vertical,
}

impl Alignment {
    pub(super) const CENTER: Self = Self::new(Horizontal::Center, Vertical::Middle);
    pub(super) const UPPER: Self = Self::new(Horizontal::Center, Vertical::Top);
    pub(super) const LOWER: Self = Self::new(Horizontal::Center, Vertical::Bottom);
    pub(super) const LEFT: Self = Self::new(Horizontal::Left, Vertical::Middle);
    pub(super) const RIGHT: Self = Self::new(Horizontal::Right, Vertical::Middle);
    pub(super) const UPPER_LEFT: Self = Self::new(Horizontal::Left, Vertical::Top);
    pub(super) const UPPER_RIGHT: Self = Self::new(Horizontal::Right, Vertical::Top);
    pub(super) const LOWER_LEFT: Self = Self::new(Horizontal::Left, Vertical::Bottom);
    pub(super) const LOWER_RIGHT: Self = Self::new(Horizontal::Right, Vertical::Bottom);

    const fn new(horizontal: Horizontal, vertical: Vertical) -> Self {
        Self { horizontal, vertical }
    }

    /// The anchor's point in a `width` x `height` cell.
    pub(super) fn point(self, width: f64, height: f64) -> (f64, f64) {
        let x = match self.horizontal {
            Horizontal::Left => 0.0,
            Horizontal::Right => width,
            Horizontal::Center => width / 2.0,
        };
        let y = match self.vertical {
            Vertical::Top => 0.0,
            Vertical::Bottom => height,
            Vertical::Middle => height / 2.0,
        };
        (x, y)
    }
}

/// A fraction of the way across a cell. Its pixel position rounds differently as a near edge ([`Fraction::min`])
/// than as a far one ([`Fraction::max`]), so a figure ending at a fraction in one cell and one starting there in the
/// next meet on the same pixel: Ghostty's seam-free rule, kept exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fraction {
    Zero,
    OneEighth,
    OneQuarter,
    OneThird,
    ThreeEighths,
    Half,
    FiveEighths,
    TwoThirds,
    ThreeQuarters,
    SevenEighths,
    One,
}

impl Fraction {
    /// `EIGHTHS[i]` is `i/8`.
    pub(super) const EIGHTHS: [Self; 9] = [
        Self::Zero,
        Self::OneEighth,
        Self::OneQuarter,
        Self::ThreeEighths,
        Self::Half,
        Self::FiveEighths,
        Self::ThreeQuarters,
        Self::SevenEighths,
        Self::One,
    ];

    /// `QUARTERS[i]` is `i/4`.
    pub(super) const QUARTERS: [Self; 5] = [Self::Zero, Self::OneQuarter, Self::Half, Self::ThreeQuarters, Self::One];

    pub(super) fn value(self) -> f64 {
        match self {
            Self::Zero => 0.0,
            Self::OneEighth => 0.125,
            Self::OneQuarter => 0.25,
            Self::OneThird => 1.0 / 3.0,
            Self::ThreeEighths => 0.375,
            Self::Half => 0.5,
            Self::FiveEighths => 0.625,
            Self::TwoThirds => 2.0 / 3.0,
            Self::ThreeQuarters => 0.75,
            Self::SevenEighths => 0.875,
            Self::One => 1.0,
        }
    }

    /// The pixel of this fraction of `size` as a near (left or top) edge: rounded from the far end. For 7 pixels and
    /// a half, `7 - round(0.5 * 7) = 3` here and `round(0.5 * 7) = 4` as a far edge.
    pub(super) fn min(self, size: u32) -> i32 {
        let size = f64::from(size);
        round(size - ((1.0 - self.value()) * size).round())
    }

    /// The pixel of this fraction of `size` as a far (right or bottom) edge.
    pub(super) fn max(self, size: u32) -> i32 {
        round(self.value() * f64::from(size))
    }

    /// This fraction of `size`, unaligned, for paths.
    pub(super) fn float(self, size: u32) -> f64 {
        self.value() * f64::from(size)
    }
}

/// Fills the part of the cell between the fractions `x0..x1` and `y0..y1`.
pub(super) fn fill(
    m: &CellMetrics,
    canvas: &mut Canvas,
    (x0, x1): (Fraction, Fraction),
    (y0, y1): (Fraction, Fraction),
) {
    canvas.box_fill(
        (x0.min(m.cell_width), y0.min(m.cell_height)),
        (x1.max(m.cell_width), y1.max(m.cell_height)),
        ON,
    );
}

/// A vertical line of `thickness` through the middle of the cell.
pub(super) fn vline_middle(m: &CellMetrics, canvas: &mut Canvas, thickness: Thickness) {
    let thick = thickness.height(m.box_thickness);
    vline(
        canvas,
        (0, int(m.cell_height)),
        int(m.cell_width.saturating_sub(thick) / 2),
        thick,
    );
}

/// A horizontal line of `thickness` through the middle of the cell.
pub(super) fn hline_middle(m: &CellMetrics, canvas: &mut Canvas, thickness: Thickness) {
    let thick = thickness.height(m.box_thickness);
    hline(
        canvas,
        (0, int(m.cell_width)),
        int(m.cell_height.saturating_sub(thick) / 2),
        thick,
    );
}

/// A vertical line from `y0` to `y1` whose left edge is `x`.
pub(super) fn vline(canvas: &mut Canvas, (y0, y1): (i32, i32), x: i32, thickness: u32) {
    canvas.box_fill((x, y0), (x + int(thickness), y1), ON);
}

/// A horizontal line from `x0` to `x1` whose top edge is `y`.
pub(super) fn hline(canvas: &mut Canvas, (x0, x1): (i32, i32), y: i32, thickness: u32) {
    canvas.box_fill((x0, y), (x1, y + int(thickness)), ON);
}
