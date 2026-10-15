//! Symbols for Legacy Computing Supplement, the subset of U+1CC00–U+1CEBF Ghostty draws (Ghostty's
//! `draw/symbols_for_legacy_computing_supplement.zig`, through qwertty-term's `legacy_computing_supplement.rs`):
//! octants (from Ghostty's `octants.txt`), separated quadrants and sextants, sixteenth blocks, box drawing with corner
//! stubs, and circle and ellipse pieces.

use std::sync::LazyLock;

use super::super::metrics::CellMetrics;
use super::box_drawing::{Style, arms, lines_char};
use super::canvas::{Canvas, Path};
use super::common::{Alignment, Corner, Fraction, ON, fill, int};
use super::legacy::circle;

pub(super) fn draw(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    match cp {
        0x1cc1b..=0x1cc1e => box_stubs(cp, canvas, m),
        0x1cc21..=0x1cc2f => separated_quadrants(cp, canvas),
        0x1cc30..=0x1cc3f => circle_piece(canvas, m, CIRCLE_PIECES[index(cp, 0x1cc30)]),
        0x1cd00..=0x1cde5 => octant(cp, canvas, m),
        0x1ce00 => {
            circle(m, canvas, Alignment::LEFT, false);
            circle(m, canvas, Alignment::RIGHT, false);
        }
        0x1ce01 => {
            circle(m, canvas, Alignment::UPPER, false);
            circle(m, canvas, Alignment::LOWER, false);
        }
        0x1ce0b => {
            circle_piece(canvas, m, piece((0.0, 0.0), (1.0, 0.5), Corner::Tl));
            circle_piece(canvas, m, piece((0.0, 0.0), (1.0, 0.5), Corner::Bl));
        }
        0x1ce0c => {
            circle_piece(canvas, m, piece((1.0, 0.0), (1.0, 0.5), Corner::Tr));
            circle_piece(canvas, m, piece((1.0, 0.0), (1.0, 0.5), Corner::Br));
        }
        0x1ce16..=0x1ce19 => vertical_stubs(cp, canvas, m),
        0x1ce51..=0x1ce8f => separated_sextants(cp, canvas),
        0x1ce90..=0x1ceaf => {
            let [x0, x1, y0, y1] = SIXTEENTHS[index(cp, 0x1ce90)];
            let q = Fraction::QUARTERS;
            fill(m, canvas, (q[x0], q[x1]), (q[y0], q[y1]));
        }
        _ => {}
    }
}

fn index(cp: u32, base: u32) -> usize {
    usize::try_from(cp.saturating_sub(base)).unwrap_or(0)
}

/// 𜰛 𜰜 𜰝 𜰞 light lines along an edge with a stub down a side.
fn box_stubs(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    let (w, h, t) = (int(canvas.width()), int(canvas.height()), int(m.box_thickness));
    let horizontal = arms(Style::None, Style::Light, Style::None, Style::Light);
    match cp {
        0x1cc1b => {
            lines_char(m, canvas, horizontal);
            canvas.box_fill((w - t, 0), (w, h.div_euclid(2)), ON);
        }
        0x1cc1c => {
            lines_char(m, canvas, horizontal);
            canvas.box_fill((w - t, h.div_euclid(2)), (w, h), ON);
        }
        0x1cc1d => {
            canvas.box_fill((0, 0), (w, t), ON);
            canvas.box_fill((0, 0), (t, h.div_euclid(2)), ON);
        }
        _ => {
            canvas.box_fill((0, h - t), (w, h), ON);
            canvas.box_fill((0, h.div_euclid(2)), (t, h), ON);
        }
    }
}

/// A light vertical line with a stub along the top or bottom edge, right or left.
fn vertical_stubs(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    let (w, h, t) = (int(canvas.width()), int(canvas.height()), int(m.box_thickness));
    lines_char(m, canvas, arms(Style::Light, Style::None, Style::Light, Style::None));
    match cp {
        0x1ce16 => canvas.box_fill((w / 2, 0), (w, t), ON),
        0x1ce17 => canvas.box_fill((w / 2, h - t), (w, h), ON),
        0x1ce18 => canvas.box_fill((0, 0), (w / 2, t), ON),
        _ => canvas.box_fill((0, h - t), (w / 2, h), ON),
    }
}

/// The blocks of a separated mosaic: a gap of a twelfth of the width (at least a pixel) around and between them.
struct Separated {
    gap: i32,
    /// The gap between the columns, and between the rows.
    mid_x: i32,
    mid_y: i32,
    /// A block's width, and the heights of the rows.
    w: i32,
    rows: Vec<i32>,
}

impl Separated {
    /// The top of each row.
    fn tops(&self) -> Vec<i32> {
        let mut y = self.gap;
        self.rows
            .iter()
            .map(|height| {
                let top = y;
                y += height + self.mid_y;
                top
            })
            .collect()
    }

    /// Fills the blocks whose bits (left then right, row by row) are set.
    fn draw(&self, canvas: &mut Canvas, bits: u32) {
        let lefts = [self.gap, self.gap + self.w + self.mid_x];
        for (row, (top, height)) in self.tops().into_iter().zip(&self.rows).enumerate() {
            for (column, left) in lefts.into_iter().enumerate() {
                if bits & (1 << (row * 2 + column)) != 0 {
                    canvas.box_fill((left, top), (left + self.w, top + height), ON);
                }
            }
        }
    }
}

/// Separated block quadrants, U+1CC21–U+1CC2F.
fn separated_quadrants(cp: u32, canvas: &mut Canvas) {
    let (width, height) = (canvas.width(), canvas.height());
    let gap = int((width / 12).max(1));
    let mid_x = gap * 2 + int(width % 2);
    let mid_y = gap * 2 + int(height % 2);
    let h = (int(height) - gap * 2 - mid_y) / 2;
    let layout = Separated {
        gap,
        mid_x,
        mid_y,
        w: (int(width) - gap * 2 - mid_x) / 2,
        rows: vec![h, h],
    };
    layout.draw(canvas, (cp - 0x1cc20) & 0xf);
}

/// Separated block sextants, U+1CE51–U+1CE8F: the middle row takes what the even split leaves.
fn separated_sextants(cp: u32, canvas: &mut Canvas) {
    let (width, height) = (canvas.width(), canvas.height());
    let gap = int((width / 12).max(1));
    let mid_x = gap * 2 + int(width % 2);
    let mid_y = gap * 2 + int(height % 3) / 2;
    let h = (int(height) - gap * 2 - mid_y * 2) / 3;
    let middle = int(height) - gap * 2 - mid_y * 2 - h * 2;
    let layout = Separated {
        gap,
        mid_x,
        mid_y,
        w: (int(width) - gap * 2 - mid_x) / 2,
        rows: vec![h, middle, h],
    };
    layout.draw(canvas, (cp - 0x1ce50) & 0x3f);
}

/// U+1CE90 + index: the quarter columns `x0..x1` and rows `y0..y1` each sixteenth block covers.
const SIXTEENTHS: [[usize; 4]; 0x20] = [
    [0, 1, 0, 1],
    [1, 2, 0, 1],
    [2, 3, 0, 1],
    [3, 4, 0, 1],
    [0, 1, 1, 2],
    [1, 2, 1, 2],
    [2, 3, 1, 2],
    [3, 4, 1, 2],
    [0, 1, 2, 3],
    [1, 2, 2, 3],
    [2, 3, 2, 3],
    [3, 4, 2, 3],
    [0, 1, 3, 4],
    [1, 2, 3, 4],
    [2, 3, 3, 4],
    [3, 4, 3, 4],
    [2, 4, 3, 4],
    [1, 4, 3, 4],
    [0, 3, 3, 4],
    [0, 2, 3, 4],
    [0, 1, 2, 4],
    [0, 1, 1, 4],
    [0, 1, 0, 3],
    [0, 1, 0, 2],
    [0, 2, 0, 1],
    [0, 3, 0, 1],
    [1, 4, 0, 1],
    [2, 4, 0, 1],
    [3, 4, 0, 2],
    [3, 4, 0, 3],
    [3, 4, 1, 4],
    [3, 4, 2, 4],
];

/// The octants' cells as bits (1–8 in `octants.txt`, top-left to bottom-right by rows of two), U+1CD00 + index.
static OCTANTS: LazyLock<Vec<u8>> = LazyLock::new(|| {
    include_str!("octants.txt")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.rsplit('-')
                .next()
                .unwrap_or_default()
                .chars()
                .filter_map(|digit| digit.to_digit(10))
                .filter(|digit| (1..=8).contains(digit))
                .fold(0_u8, |bits, digit| bits | 1 << (digit - 1))
        })
        .collect()
});

fn octant(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    use Fraction::{Half, One, OneQuarter, ThreeQuarters, Zero};
    let Some(bits) = OCTANTS.get(index(cp, 0x1cd00)).copied() else {
        return;
    };
    let rows = [
        (Zero, OneQuarter),
        (OneQuarter, Half),
        (Half, ThreeQuarters),
        (ThreeQuarters, One),
    ];
    for bit in 0..8 {
        if bits & (1 << bit) != 0 {
            let x = if bit % 2 == 0 { (Zero, Half) } else { (Half, One) };
            fill(m, canvas, x, rows[bit / 2]);
        }
    }
}

/// A slice of a quarter ellipse: the cell's offset into the ellipse's bounding box (in cells), the ellipse's half axes
/// (in cells), and which quarter of it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Piece {
    offset: (f64, f64),
    radii: (f64, f64),
    quarter: Corner,
}

const fn piece(offset: (f64, f64), radii: (f64, f64), quarter: Corner) -> Piece {
    Piece { offset, radii, quarter }
}

/// U+1CC30 + index, the twelfth and quarter circle pieces.
const CIRCLE_PIECES: [Piece; 0x10] = [
    piece((0.0, 0.0), (2.0, 2.0), Corner::Tl),
    piece((1.0, 0.0), (2.0, 2.0), Corner::Tl),
    piece((2.0, 0.0), (2.0, 2.0), Corner::Tr),
    piece((3.0, 0.0), (2.0, 2.0), Corner::Tr),
    piece((0.0, 1.0), (2.0, 2.0), Corner::Tl),
    piece((0.0, 0.0), (1.0, 1.0), Corner::Tl),
    piece((1.0, 0.0), (1.0, 1.0), Corner::Tr),
    piece((3.0, 1.0), (2.0, 2.0), Corner::Tr),
    piece((0.0, 2.0), (2.0, 2.0), Corner::Bl),
    piece((0.0, 1.0), (1.0, 1.0), Corner::Bl),
    piece((1.0, 1.0), (1.0, 1.0), Corner::Br),
    piece((3.0, 2.0), (2.0, 2.0), Corner::Br),
    piece((0.0, 3.0), (2.0, 2.0), Corner::Bl),
    piece((1.0, 3.0), (2.0, 2.0), Corner::Bl),
    piece((2.0, 3.0), (2.0, 2.0), Corner::Br),
    piece((3.0, 3.0), (2.0, 2.0), Corner::Br),
];

/// The part of a quarter ellipse (one cubic Bézier) that lands in this cell, stroked at the box thickness and kept
/// inside the cell (Ghostty's `circlePiece`).
fn circle_piece(canvas: &mut Canvas, metrics: &CellMetrics, piece: Piece) {
    let (width, height) = (f64::from(canvas.width()), f64::from(canvas.height()));
    let (rw, rh) = (width * piece.radii.0, height * piece.radii.1);
    let (left, top) = (width * piece.offset.0, height * piece.offset.1);
    canvas.clip_to_cell();
    let coefficient = (std::f64::consts::SQRT_2 - 1.0) * 4.0 / 3.0;
    let (cw, ch) = (coefficient * rw, coefficient * rh);
    let thick = f64::from(metrics.box_thickness);
    let ht = thick * 0.5;
    let at = |px: f64, py: f64| (px - left, py - top);
    let (from, c1, c2, to) = match piece.quarter {
        Corner::Tl => (at(rw, ht), at(rw - cw, ht), at(ht, rh - ch), at(ht, rh)),
        Corner::Tr => (
            at(rw, ht),
            at(rw + cw, ht),
            at(rw * 2.0 - ht, rh - ch),
            at(rw * 2.0 - ht, rh),
        ),
        Corner::Bl => (
            at(ht, rh),
            at(ht, rh + ch),
            at(rw - cw, rh * 2.0 - ht),
            at(rw, rh * 2.0 - ht),
        ),
        Corner::Br => (
            at(rw * 2.0 - ht, rh),
            at(rw * 2.0 - ht, rh + ch),
            at(rw + cw, rh * 2.0 - ht),
            at(rw, rh * 2.0 - ht),
        ),
    };
    let mut path = Path::new();
    path.move_to(from);
    path.cubic_to(c1, c2, to);
    canvas.stroke(path, thick, ON);
}
