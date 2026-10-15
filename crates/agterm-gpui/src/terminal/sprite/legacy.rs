//! Symbols for Legacy Computing, U+1FB00–U+1FBEF (Ghostty's `draw/symbols_for_legacy_computing.zig`, through
//! qwertty-term's `legacy_computing.rs`): sextants, the smooth mosaics (polygons from Ghostty's 3x4 patterns), edge
//! triangles, eighth blocks, shaded and checkered blocks, diagonal fills, corner and cell diagonals, circles.

use super::super::metrics::CellMetrics;
use super::block::{
    FIVE_EIGHTHS, HALF, ONE_EIGHTH, ONE_QUARTER, SEVEN_EIGHTHS, THREE_EIGHTHS, THREE_QUARTERS, block, block_shade,
    full_block_shade,
};
use super::box_drawing::{Style, arms, diagonal_cross, lines_char};
use super::canvas::{Canvas, Path};
use super::common::{Alignment, Corner, Edge, Fraction, ON, Quads, Shade, Thickness, fill, index, int, round};
use super::geometric::corner_triangle_shade;

const ONE_THIRD: f64 = 1.0 / 3.0;
const TWO_THIRDS: f64 = 2.0 / 3.0;

pub(super) fn draw(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    match cp {
        0x1fb00..=0x1fb3b => sextant(cp, canvas, m),
        0x1fb3c..=0x1fb67 => smooth_mosaic(cp, canvas, m),
        0x1fb68..=0x1fb6b => {
            edge_triangle(m, canvas, EDGES[index(cp, 0x1fb68)]);
            canvas.invert();
            canvas.clip_to_cell();
        }
        0x1fb6c..=0x1fb6f => edge_triangle(m, canvas, EDGES[index(cp, 0x1fb6c)]),
        0x1fb70..=0x1fb75 => {
            let n = index(cp, 0x1fb6f);
            fill(
                m,
                canvas,
                (Fraction::EIGHTHS[n], Fraction::EIGHTHS[n + 1]),
                (Fraction::Zero, Fraction::One),
            );
        }
        0x1fb76..=0x1fb7b => horizontal_eighth(m, canvas, index(cp, 0x1fb75)),
        0x1fb7c..=0x1fb97 => mixed(cp, canvas, m),
        0x1fb98 => hatch(m, canvas, false),
        0x1fb99 => hatch(m, canvas, true),
        0x1fb9a => {
            edge_triangle(m, canvas, Edge::Top);
            edge_triangle(m, canvas, Edge::Bottom);
        }
        0x1fb9b => {
            edge_triangle(m, canvas, Edge::Left);
            edge_triangle(m, canvas, Edge::Right);
        }
        0x1fb9c..=0x1fb9f => {
            let corner = [Corner::Tl, Corner::Tr, Corner::Br, Corner::Bl][index(cp, 0x1fb9c)];
            corner_triangle_shade(m, canvas, corner, Shade::Medium);
        }
        0x1fba0..=0x1fbae => corner_diagonals(m, canvas, Quads::bits(CORNER_DIAGONALS[index(cp, 0x1fba0)])),
        0x1fbaf => lines_char(m, canvas, arms(Style::Heavy, Style::Light, Style::Heavy, Style::Light)),
        0x1fbbd..=0x1fbbf => {
            match cp {
                0x1fbbd => diagonal_cross(m, canvas),
                0x1fbbe => corner_diagonals(m, canvas, Quads::bits(0b1000)),
                _ => corner_diagonals(m, canvas, Quads::bits(0b1111)),
            }
            canvas.invert();
            canvas.clip_to_cell();
        }
        0x1fbce => block(m, canvas, Alignment::LEFT, (TWO_THIRDS, 1.0)),
        0x1fbcf => block(m, canvas, Alignment::LEFT, (ONE_THIRD, 1.0)),
        0x1fbd0..=0x1fbdf => {
            for (from, to) in CELL_DIAGONALS[index(cp, 0x1fbd0)].iter().flatten() {
                cell_diagonal(m, canvas, *from, *to);
            }
        }
        0x1fbe0..=0x1fbef => circles(cp, canvas, m),
        _ => {}
    }
}

/// The edges of the edge triangles, in code point order.
const EDGES: [Edge; 4] = [Edge::Left, Edge::Top, Edge::Right, Edge::Bottom];

/// Sextants: the code point's offset, skipping the two the block elements already have, as six bits (top-left,
/// top-right, middle-left, middle-right, bottom-left, bottom-right).
fn sextant(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    use Fraction::{Half, One, OneThird, TwoThirds, Zero};
    let offset = cp - 0x1fb00;
    let bits = offset + offset / 0x14 + 1;
    let rows = [(Zero, OneThird), (OneThird, TwoThirds), (TwoThirds, One)];
    for (bit, (column, row)) in [(0, 0), (1, 0), (0, 1), (1, 1), (0, 2), (1, 2)].into_iter().enumerate() {
        if bits & (1 << bit) != 0 {
            let x = if column == 0 { (Zero, Half) } else { (Half, One) };
            fill(m, canvas, x, rows[row]);
        }
    }
}

/// A horizontal eighth block: row `n` of the eight.
fn horizontal_eighth(m: &CellMetrics, canvas: &mut Canvas, n: usize) {
    fill(
        m,
        canvas,
        (Fraction::Zero, Fraction::One),
        (Fraction::EIGHTHS[n], Fraction::EIGHTHS[n + 1]),
    );
}

/// The smooth mosaics' 3x4 patterns, as Ghostty writes them: `#` filled, `.` empty, `/` and `\` where the edge runs
/// diagonally.
const MOSAICS: [[&str; 4]; 44] = [
    ["...", "...", "#..", "##."],
    ["...", "...", "#\\.", "###"],
    ["...", "#..", "#\\.", "##."],
    ["...", "#..", "##.", "###"],
    ["#..", "#..", "##.", "##."],
    ["/##", "###", "###", "###"],
    ["./#", "###", "###", "###"],
    [".##", ".##", "###", "###"],
    ["..#", ".##", "###", "###"],
    [".##", ".##", ".##", "###"],
    ["...", "./#", "###", "###"],
    ["...", "...", "..#", ".##"],
    ["...", "...", "./#", "###"],
    ["...", "..#", "./#", ".##"],
    ["...", "..#", ".##", "###"],
    ["..#", "..#", ".##", ".##"],
    ["##\\", "###", "###", "###"],
    ["#\\.", "###", "###", "###"],
    ["##.", "##.", "###", "###"],
    ["#..", "##.", "###", "###"],
    ["##.", "##.", "##.", "###"],
    ["...", "#\\.", "###", "###"],
    ["###", "###", "###", "\\##"],
    ["###", "###", "###", ".\\#"],
    ["###", "###", ".##", ".##"],
    ["###", "###", ".##", "..#"],
    ["###", ".##", ".##", ".##"],
    ["##.", "#..", "...", "..."],
    ["###", "#/.", "...", "..."],
    ["##.", "#/.", "#..", "..."],
    ["###", "##.", "#..", "..."],
    ["##.", "##.", "#..", "#.."],
    ["###", "###", "#/.", "..."],
    ["###", "###", "###", "##/"],
    ["###", "###", "###", "#/."],
    ["###", "###", "##.", "##."],
    ["###", "###", "##.", "#.."],
    ["###", "##.", "##.", "##."],
    [".##", "..#", "...", "..."],
    ["###", ".\\#", "...", "..."],
    [".##", ".\\#", "..#", "..."],
    ["###", ".##", "..#", "..."],
    [".##", ".##", "..#", "..#"],
    ["###", "###", ".\\#", "..."],
];

/// The polygon of a smooth mosaic: its corners clockwise from the top-left, each of the ten anchor points (the
/// corners, the thirds of the sides, the middles of the top and bottom) that Ghostty's `SmoothMosaic.from` keeps.
fn smooth_mosaic(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    let rows = MOSAICS[index(cp, 0x1fb3c)];
    let at = |row: usize, col: usize| rows[row].as_bytes().get(col) == Some(&b'#');
    // an anchor on a side is a corner of the polygon unless both its neighbours along that side are filled
    let side = |cell: bool, a: bool, b: bool| cell && (!a || !b);
    let (width, height) = (f64::from(m.cell_width), f64::from(m.cell_height));
    let upper = Fraction::OneThird.float(m.cell_height);
    let lower = Fraction::TwoThirds.float(m.cell_height);
    let center = Fraction::Half.float(m.cell_width);
    let anchors = [
        (at(0, 0), (0.0, 0.0)),
        (side(at(1, 0), at(0, 0), at(2, 0)), (0.0, upper)),
        (side(at(2, 0), at(1, 0), at(3, 0)), (0.0, lower)),
        (at(3, 0), (0.0, height)),
        (side(at(3, 1), at(3, 0), at(3, 2)), (center, height)),
        (at(3, 2), (width, height)),
        (side(at(2, 2), at(3, 2), at(1, 2)), (width, lower)),
        (side(at(1, 2), at(2, 2), at(0, 2)), (width, upper)),
        (at(0, 2), (width, 0.0)),
        (side(at(0, 1), at(0, 2), at(0, 0)), (center, 0.0)),
    ];
    let points: Vec<(f64, f64)> = anchors
        .into_iter()
        .filter_map(|(on, point)| on.then_some(point))
        .collect();
    canvas.fill(Path::polygon(&points), ON);
}

/// A triangle from the middle of the cell to the two corners of `edge`.
fn edge_triangle(m: &CellMetrics, canvas: &mut Canvas, edge: Edge) {
    let (width, height) = (f64::from(m.cell_width), f64::from(m.cell_height));
    let middle = (height / 2.0).round();
    let center = (width / 2.0).round();
    let (a, b) = match edge {
        Edge::Top => ((width, 0.0), (0.0, 0.0)),
        Edge::Left => ((0.0, 0.0), (0.0, height)),
        Edge::Bottom => ((0.0, height), (width, height)),
        Edge::Right => ((width, height), (width, 0.0)),
    };
    canvas.triangle([(center, middle), a, b], ON);
}

/// A mixed block glyph's parts.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Part {
    Block(Alignment, f64, f64),
    Shaded(Alignment, f64, f64, Shade),
    Full(Shade),
    /// Horizontal eighth rows.
    Rows([usize; 4]),
    Checker(usize),
    /// 🮗 the second and fourth quarter rows.
    Bands,
}

const UPPER: Alignment = Alignment::UPPER;
const LOWER: Alignment = Alignment::LOWER;
const LEFT: Alignment = Alignment::LEFT;
const RIGHT: Alignment = Alignment::RIGHT;

/// U+1FB7C + index: each glyph's parts, drawn in order (U+1FB93 is unassigned, blank).
const MIXED: [&[Part]; 0x1c] = [
    &[Part::Block(LEFT, ONE_EIGHTH, 1.0), Part::Block(LOWER, 1.0, ONE_EIGHTH)],
    &[Part::Block(LEFT, ONE_EIGHTH, 1.0), Part::Block(UPPER, 1.0, ONE_EIGHTH)],
    &[Part::Block(RIGHT, ONE_EIGHTH, 1.0), Part::Block(UPPER, 1.0, ONE_EIGHTH)],
    &[Part::Block(RIGHT, ONE_EIGHTH, 1.0), Part::Block(LOWER, 1.0, ONE_EIGHTH)],
    &[Part::Block(UPPER, 1.0, ONE_EIGHTH), Part::Block(LOWER, 1.0, ONE_EIGHTH)],
    &[Part::Rows([0, 2, 4, 7])],
    &[Part::Block(UPPER, 1.0, ONE_QUARTER)],
    &[Part::Block(UPPER, 1.0, THREE_EIGHTHS)],
    &[Part::Block(UPPER, 1.0, FIVE_EIGHTHS)],
    &[Part::Block(UPPER, 1.0, THREE_QUARTERS)],
    &[Part::Block(UPPER, 1.0, SEVEN_EIGHTHS)],
    &[Part::Block(RIGHT, ONE_QUARTER, 1.0)],
    &[Part::Block(RIGHT, THREE_EIGHTHS, 1.0)],
    &[Part::Block(RIGHT, FIVE_EIGHTHS, 1.0)],
    &[Part::Block(RIGHT, THREE_QUARTERS, 1.0)],
    &[Part::Block(RIGHT, SEVEN_EIGHTHS, 1.0)],
    &[Part::Shaded(LEFT, HALF, 1.0, Shade::Medium)],
    &[Part::Shaded(RIGHT, HALF, 1.0, Shade::Medium)],
    &[Part::Shaded(UPPER, 1.0, HALF, Shade::Medium)],
    &[Part::Shaded(LOWER, 1.0, HALF, Shade::Medium)],
    &[Part::Full(Shade::Medium)],
    &[Part::Full(Shade::Medium), Part::Block(UPPER, 1.0, HALF)],
    &[Part::Full(Shade::Medium), Part::Block(LOWER, 1.0, HALF)],
    &[],
    &[Part::Full(Shade::Medium), Part::Block(RIGHT, HALF, 1.0)],
    &[Part::Checker(0)],
    &[Part::Checker(1)],
    &[Part::Bands],
];

fn mixed(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    for part in MIXED[index(cp, 0x1fb7c)] {
        match *part {
            Part::Block(alignment, width, height) => block(m, canvas, alignment, (width, height)),
            Part::Shaded(alignment, width, height, shade) => block_shade(m, canvas, alignment, (width, height), shade),
            Part::Full(shade) => full_block_shade(m, canvas, shade),
            Part::Rows(rows) => {
                for row in rows {
                    horizontal_eighth(m, canvas, row);
                }
            }
            Part::Checker(parity) => checkerboard(m, canvas, parity),
            Part::Bands => {
                let (width, height) = (int(canvas.width()), int(canvas.height()));
                canvas.box_fill((0, height / 4), (width, 2 * height / 4), ON);
                canvas.box_fill((0, 3 * height / 4), (width, height), ON);
            }
        }
    }
}

/// A 4-column checkerboard with square-ish cells, `parity` choosing which squares are filled.
fn checkerboard(m: &CellMetrics, canvas: &mut Canvas, parity: usize) {
    let columns = 4_u32;
    let rows = u32::try_from(round(4.0 * f64::from(m.cell_height) / f64::from(m.cell_width))).unwrap_or(0);
    for x in 0..columns {
        let (x0, x1) = (m.cell_width * x / columns, m.cell_width * (x + 1) / columns);
        for y in 0..rows {
            let (y0, y1) = (m.cell_height * y / rows, m.cell_height * (y + 1) / rows);
            if usize::try_from((x + y) % 2).ok() == Some(parity) {
                canvas.rect((int(x0), int(y0)), (int(x1 - x0), int(y1 - y0)), ON);
            }
        }
    }
}

/// 🮘 / 🮙 parallel diagonal lines filling the cell, falling (upper left to lower right) or rising.
fn hatch(m: &CellMetrics, canvas: &mut Canvas, rising: bool) {
    canvas.clip_to_cell();
    let thick_px = Thickness::Light.height(m.box_thickness);
    let lines = m.cell_width / (2 * thick_px);
    let (width, height) = (f64::from(m.cell_width), f64::from(m.cell_height));
    let stride = (width / f64::from(lines)).round();
    for step in -int(lines)..=int(lines) {
        let offset = f64::from(step) * stride;
        let (top, bottom) = if rising {
            (width + offset, offset)
        } else {
            (offset, width + offset)
        };
        canvas.line((top, 0.0), (bottom, height), f64::from(thick_px), ON);
    }
}

/// The corners U+1FBA0 + index joins with lines from the middles of the sides (top-left, top-right, bottom-left,
/// bottom-right as bits 0–3).
const CORNER_DIAGONALS: [u8; 0xf] = [
    0b0001, 0b0010, 0b0100, 0b1000, 0b0101, 0b1010, 0b1100, 0b0011, 0b1001, 0b0110, 0b1110, 0b1101, 0b1011, 0b0111,
    0b1111,
];

/// Lines between the middles of the sides around each corner in `corners`.
fn corner_diagonals(m: &CellMetrics, canvas: &mut Canvas, corners: Quads) {
    let thick = f64::from(Thickness::Light.height(m.box_thickness));
    let (width, height) = (f64::from(m.cell_width), f64::from(m.cell_height));
    let cx = f64::from(m.cell_width / 2 + m.cell_width % 2);
    let cy = f64::from(m.cell_height / 2 + m.cell_height % 2);
    let lines = [
        ((cx, 0.0), (0.0, cy)),
        ((cx, 0.0), (width, cy)),
        ((cx, height), (0.0, cy)),
        ((cx, height), (width, cy)),
    ];
    for (index, (from, to)) in lines.into_iter().enumerate() {
        if corners.has(index) {
            canvas.line(from, to, thick, ON);
        }
    }
}

type Diagonal = Option<(Alignment, Alignment)>;

const fn one(from: Alignment, to: Alignment) -> [Diagonal; 2] {
    [Some((from, to)), None]
}

const fn two(a: Alignment, b: Alignment, c: Alignment) -> [Diagonal; 2] {
    [Some((a, b)), Some((b, c))]
}

/// U+1FBD0 + index: the one or two lines between anchor points of the cell.
const CELL_DIAGONALS: [[Diagonal; 2]; 0x10] = {
    use Alignment as A;
    [
        one(A::RIGHT, A::LOWER_LEFT),
        one(A::UPPER_RIGHT, A::LEFT),
        one(A::UPPER_LEFT, A::RIGHT),
        one(A::LEFT, A::LOWER_RIGHT),
        one(A::UPPER_LEFT, A::LOWER),
        one(A::UPPER, A::LOWER_RIGHT),
        one(A::UPPER_RIGHT, A::LOWER),
        one(A::UPPER, A::LOWER_LEFT),
        two(A::UPPER_LEFT, A::CENTER, A::UPPER_RIGHT),
        two(A::UPPER_RIGHT, A::CENTER, A::LOWER_RIGHT),
        two(A::LOWER_LEFT, A::CENTER, A::LOWER_RIGHT),
        two(A::UPPER_LEFT, A::CENTER, A::LOWER_LEFT),
        two(A::UPPER_LEFT, A::LOWER, A::UPPER_RIGHT),
        two(A::UPPER_RIGHT, A::LEFT, A::LOWER_RIGHT),
        two(A::LOWER_LEFT, A::UPPER, A::LOWER_RIGHT),
        two(A::UPPER_LEFT, A::RIGHT, A::LOWER_LEFT),
    ]
};

fn cell_diagonal(m: &CellMetrics, canvas: &mut Canvas, from: Alignment, to: Alignment) {
    let (width, height) = (f64::from(m.cell_width), f64::from(m.cell_height));
    let thick = f64::from(Thickness::Light.height(m.box_thickness));
    canvas.line(from.point(width, height), to.point(width, height), thick, ON);
}

/// U+1FBE0–U+1FBEF: circles centered on an anchor (outlined, then filled), and half-size blocks.
fn circles(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    use Alignment as A;
    let anchors = [A::UPPER, A::RIGHT, A::LOWER, A::LEFT];
    match index(cp, 0x1fbe0) {
        n @ 0..=3 => circle(m, canvas, anchors[n], false),
        n @ 4..=7 => block(m, canvas, [A::UPPER, A::LOWER, A::LEFT, A::RIGHT][n - 4], (0.5, 0.5)),
        n @ 8..=11 => circle(m, canvas, anchors[n - 8], true),
        n => {
            let corner = [A::UPPER_RIGHT, A::LOWER_LEFT, A::LOWER_RIGHT, A::UPPER_LEFT];
            if let Some(position) = corner.get(n - 12) {
                circle(m, canvas, *position, true);
            }
        }
    }
}

/// A circle of half the cell's smaller side around the anchor `position`, filled or a light outline, kept inside
/// the cell; the supplement's half circles use it too.
pub(super) fn circle(m: &CellMetrics, canvas: &mut Canvas, position: Alignment, filled: bool) {
    canvas.clip_to_cell();
    let (width, height) = (f64::from(m.cell_width), f64::from(m.cell_height));
    let center = position.point(width, height);
    let r = 0.5 * width.min(height);
    let thick = f64::from(Thickness::Light.height(m.box_thickness));
    let mut path = Path::new();
    if filled {
        path.circle(center, r);
        canvas.fill(path, ON);
    } else {
        path.circle(center, r - thick / 2.0);
        canvas.stroke(path, thick, ON);
    }
}
