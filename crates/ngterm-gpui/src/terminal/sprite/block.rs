//! Block Elements, U+2580–U+259F (Ghostty's `draw/block.zig`, through qwertty-term's `block.rs`); [`block`],
//! [`block_shade`] and [`full_block_shade`] serve the legacy computing glyphs too.

use super::super::metrics::CellMetrics;
use super::canvas::Canvas;
use super::common::{Alignment, Fraction, Horizontal, Quads, Shade, Vertical, count, fill, int, round};

pub(super) const ONE_EIGHTH: f64 = 0.125;
pub(super) const ONE_QUARTER: f64 = 0.25;
pub(super) const THREE_EIGHTHS: f64 = 0.375;
pub(super) const HALF: f64 = 0.5;
pub(super) const FIVE_EIGHTHS: f64 = 0.625;
pub(super) const THREE_QUARTERS: f64 = 0.75;
pub(super) const SEVEN_EIGHTHS: f64 = 0.875;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Glyph {
    /// A solid block of a fraction of the cell's width and height.
    Block(Alignment, f64, f64),
    Shade(Shade),
    Quadrants(Quads),
}

const fn upper(height: f64) -> Glyph {
    Glyph::Block(Alignment::UPPER, 1.0, height)
}

const fn lower(height: f64) -> Glyph {
    Glyph::Block(Alignment::LOWER, 1.0, height)
}

const fn left(width: f64) -> Glyph {
    Glyph::Block(Alignment::LEFT, width, 1.0)
}

const fn right(width: f64) -> Glyph {
    Glyph::Block(Alignment::RIGHT, width, 1.0)
}

/// Quadrants top-left, top-right, bottom-left, bottom-right as bits 0–3.
const fn quads(bits: u8) -> Glyph {
    Glyph::Quadrants(Quads::bits(bits))
}

/// U+2580 + index.
const GLYPHS: [Glyph; 0x20] = [
    upper(HALF),           // ▀
    lower(ONE_EIGHTH),     // ▁
    lower(ONE_QUARTER),    // ▂
    lower(THREE_EIGHTHS),  // ▃
    lower(HALF),           // ▄
    lower(FIVE_EIGHTHS),   // ▅
    lower(THREE_QUARTERS), // ▆
    lower(SEVEN_EIGHTHS),  // ▇
    Glyph::Shade(Shade::On),
    left(SEVEN_EIGHTHS),  // ▉
    left(THREE_QUARTERS), // ▊
    left(FIVE_EIGHTHS),   // ▋
    left(HALF),           // ▌
    left(THREE_EIGHTHS),  // ▍
    left(ONE_QUARTER),    // ▎
    left(ONE_EIGHTH),     // ▏
    right(HALF),          // ▐
    Glyph::Shade(Shade::Light),
    Glyph::Shade(Shade::Medium),
    Glyph::Shade(Shade::Dark),
    upper(ONE_EIGHTH), // ▔
    right(ONE_EIGHTH), // ▕
    quads(0b0100),     // ▖
    quads(0b1000),     // ▗
    quads(0b0001),     // ▘
    quads(0b1101),     // ▙
    quads(0b1001),     // ▚
    quads(0b0111),     // ▛
    quads(0b1011),     // ▜
    quads(0b0010),     // ▝
    quads(0b0110),     // ▞
    quads(0b1110),     // ▟
];

pub(super) fn draw(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    let Some(glyph) = cp
        .checked_sub(0x2580)
        .and_then(|index| GLYPHS.get(usize::try_from(index).ok()?))
    else {
        return;
    };
    match *glyph {
        Glyph::Block(alignment, width, height) => block(m, canvas, alignment, (width, height)),
        Glyph::Shade(shade) => full_block_shade(m, canvas, shade),
        Glyph::Quadrants(quads) => quadrants(m, canvas, quads),
    }
}

/// A solid block of `size` (fractions of the cell's width and height) at `alignment`.
pub(super) fn block(m: &CellMetrics, canvas: &mut Canvas, alignment: Alignment, size: (f64, f64)) {
    block_shade(m, canvas, alignment, size, Shade::On);
}

/// A block of `size` (fractions of the cell's width and height) at `alignment`, in `shade`.
pub(super) fn block_shade(m: &CellMetrics, canvas: &mut Canvas, alignment: Alignment, size: (f64, f64), shade: Shade) {
    let width = count(round(f64::from(m.cell_width) * size.0));
    let height = count(round(f64::from(m.cell_height) * size.1));
    let x = match alignment.horizontal {
        Horizontal::Left => 0,
        Horizontal::Right => m.cell_width.saturating_sub(width),
        Horizontal::Center => m.cell_width.saturating_sub(width) / 2,
    };
    let y = match alignment.vertical {
        Vertical::Top => 0,
        Vertical::Bottom => m.cell_height.saturating_sub(height),
        Vertical::Middle => m.cell_height.saturating_sub(height) / 2,
    };
    canvas.rect((int(x), int(y)), (int(width), int(height)), shade.alpha());
}

/// The whole cell in `shade`.
pub(super) fn full_block_shade(m: &CellMetrics, canvas: &mut Canvas, shade: Shade) {
    canvas.box_fill((0, 0), (int(m.cell_width), int(m.cell_height)), shade.alpha());
}

fn quadrants(m: &CellMetrics, canvas: &mut Canvas, quads: Quads) {
    use Fraction::{Half, One, Zero};
    let corners = [
        ((Zero, Half), (Zero, Half)),
        ((Half, One), (Zero, Half)),
        ((Zero, Half), (Half, One)),
        ((Half, One), (Half, One)),
    ];
    for (index, (x, y)) in corners.into_iter().enumerate() {
        if quads.has(index) {
            fill(m, canvas, x, y);
        }
    }
}
