//! Branch Drawing Characters, U+F5D0–U+F60D (Ghostty's `draw/branch.zig`, through qwertty-term's `branch.rs`): the
//! git graph glyphs, lines and arcs aligned with box drawing, fading lines, and nodes (a circle, filled or hollow,
//! with stubs to some edges).

use super::super::metrics::CellMetrics;
use super::box_drawing::arc;
use super::canvas::{Canvas, Path};
use super::common::{Corner, Edge, ON, Thickness, byte, ceil, floor, hline_middle, int, vline_middle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Straight {
    None,
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glyph {
    /// A straight line through the middle and up to two arcs.
    Lines(Straight, Option<Corner>, Option<Corner>),
    /// A line fading out towards an edge.
    Fade(Edge),
    /// A node: stubs up, right, down, left as bits 0–3, and whether it is filled.
    Node(u8, bool),
}

const fn lines(straight: Straight, a: Option<Corner>, b: Option<Corner>) -> Glyph {
    Glyph::Lines(straight, a, b)
}

const NO: Option<Corner> = None;
const TL: Option<Corner> = Some(Corner::Tl);
const TR: Option<Corner> = Some(Corner::Tr);
const BL: Option<Corner> = Some(Corner::Bl);
const BR: Option<Corner> = Some(Corner::Br);
const H: Straight = Straight::Horizontal;
const V: Straight = Straight::Vertical;
const X: Straight = Straight::None;
const UP: u8 = 1;
const RIGHT: u8 = 2;
const DOWN: u8 = 4;
const LEFT: u8 = 8;

/// U+F5D0 + index, as in Ghostty's `drawF5D0_F60D`.
const GLYPHS: [Glyph; 0x3e] = [
    lines(H, NO, NO),
    lines(V, NO, NO),
    Glyph::Fade(Edge::Right),
    Glyph::Fade(Edge::Left),
    Glyph::Fade(Edge::Bottom),
    Glyph::Fade(Edge::Top),
    lines(X, BR, NO),
    lines(X, BL, NO),
    lines(X, TR, NO),
    lines(X, TL, NO),
    lines(V, TR, NO),
    lines(V, BR, NO),
    lines(X, TR, BR),
    lines(V, TL, NO),
    lines(V, BL, NO),
    lines(X, TL, BL),
    // U+F5E0
    lines(H, BL, NO),
    lines(H, BR, NO),
    lines(X, BR, BL),
    lines(H, TL, NO),
    lines(H, TR, NO),
    lines(X, TR, TL),
    lines(V, TL, TR),
    lines(V, BL, BR),
    lines(H, BL, TL),
    lines(H, TR, BR),
    lines(V, TL, BR),
    lines(V, TR, BL),
    lines(H, TL, BR),
    lines(H, TR, BL),
    Glyph::Node(0, true),
    Glyph::Node(0, false),
    // U+F5F0
    Glyph::Node(RIGHT, true),
    Glyph::Node(RIGHT, false),
    Glyph::Node(LEFT, true),
    Glyph::Node(LEFT, false),
    Glyph::Node(RIGHT | LEFT, true),
    Glyph::Node(RIGHT | LEFT, false),
    Glyph::Node(DOWN, true),
    Glyph::Node(DOWN, false),
    Glyph::Node(UP, true),
    Glyph::Node(UP, false),
    Glyph::Node(UP | DOWN, true),
    Glyph::Node(UP | DOWN, false),
    Glyph::Node(RIGHT | DOWN, true),
    Glyph::Node(RIGHT | DOWN, false),
    Glyph::Node(DOWN | LEFT, true),
    Glyph::Node(DOWN | LEFT, false),
    // U+F600
    Glyph::Node(UP | RIGHT, true),
    Glyph::Node(UP | RIGHT, false),
    Glyph::Node(UP | LEFT, true),
    Glyph::Node(UP | LEFT, false),
    Glyph::Node(UP | RIGHT | DOWN, true),
    Glyph::Node(UP | RIGHT | DOWN, false),
    Glyph::Node(UP | DOWN | LEFT, true),
    Glyph::Node(UP | DOWN | LEFT, false),
    Glyph::Node(RIGHT | DOWN | LEFT, true),
    Glyph::Node(RIGHT | DOWN | LEFT, false),
    Glyph::Node(UP | RIGHT | LEFT, true),
    Glyph::Node(UP | RIGHT | LEFT, false),
    Glyph::Node(UP | RIGHT | DOWN | LEFT, true),
    Glyph::Node(UP | RIGHT | DOWN | LEFT, false),
];

pub(super) fn draw(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    let Some(glyph) = cp
        .checked_sub(0xf5d0)
        .and_then(|index| GLYPHS.get(usize::try_from(index).ok()?))
    else {
        return;
    };
    match *glyph {
        Glyph::Lines(straight, a, b) => {
            match straight {
                Straight::None => {}
                Straight::Horizontal => hline_middle(m, canvas, Thickness::Light),
                Straight::Vertical => vline_middle(m, canvas, Thickness::Light),
            }
            for corner in [a, b].into_iter().flatten() {
                arc(m, canvas, corner, Thickness::Light);
            }
        }
        Glyph::Fade(edge) => fading_line(m, canvas, edge),
        Glyph::Node(stubs, filled) => node(m, canvas, stubs, filled),
    }
}

/// A circle centered where box drawing's lines cross, as large as the cell allows, with stubs to the edges in
/// `stubs`; filled, or a light outline.
fn node(m: &CellMetrics, canvas: &mut Canvas, stubs: u8, filled: bool) {
    let thick_px = Thickness::Light.height(m.box_thickness);
    let (width, height) = (f64::from(m.cell_width), f64::from(m.cell_height));
    let thick = f64::from(thick_px);
    let h_top = m.cell_height.saturating_sub(thick_px) / 2;
    let v_left = m.cell_width.saturating_sub(thick_px) / 2;
    let (h_top, h_bottom) = (int(h_top), int(h_top + thick_px));
    let (v_left, v_right) = (int(v_left), int(v_left + thick_px));
    let cx = f64::from(v_left) + thick / 2.0;
    let cy = f64::from(h_top) + thick / 2.0;
    let r = cx.min(cy).min((width - cx).min(height - cy));
    let (cell_w, cell_h) = (int(m.cell_width), int(m.cell_height));
    if stubs & UP != 0 {
        canvas.box_fill((v_left, 0), (v_right, ceil(cy - r + thick / 2.0)), ON);
    }
    if stubs & RIGHT != 0 {
        canvas.box_fill((floor(cx + r - thick / 2.0), h_top), (cell_w, h_bottom), ON);
    }
    if stubs & DOWN != 0 {
        canvas.box_fill((v_left, floor(cy + r - thick / 2.0)), (v_right, cell_h), ON);
    }
    if stubs & LEFT != 0 {
        canvas.box_fill((0, h_top), (ceil(cx - r + thick / 2.0), h_bottom), ON);
    }
    let mut path = Path::new();
    if filled {
        path.circle((cx, cy), r);
        canvas.fill(path, ON);
    } else {
        path.circle((cx, cy), r - thick / 2.0);
        canvas.stroke(path, thick, ON);
    }
}

/// A light line through the middle that fades from full coverage to none at `to`.
fn fading_line(m: &CellMetrics, canvas: &mut Canvas, to: Edge) {
    let thick_px = Thickness::Light.height(m.box_thickness);
    let h_top = m.cell_height.saturating_sub(thick_px) / 2;
    let v_left = m.cell_width.saturating_sub(thick_px) / 2;
    let (length, across) = match to {
        Edge::Top | Edge::Bottom => (m.cell_height, (v_left, v_left + thick_px)),
        Edge::Left | Edge::Right => (m.cell_width, (h_top, h_top + thick_px)),
    };
    let (mut color, step) = match to {
        Edge::Top | Edge::Left => (0.0, 255.0 / f64::from(length)),
        Edge::Bottom | Edge::Right => (255.0, -255.0 / f64::from(length)),
    };
    for along in 0..length {
        for side in across.0..across.1 {
            let (x, y) = match to {
                Edge::Top | Edge::Bottom => (side, along),
                Edge::Left | Edge::Right => (along, side),
            };
            canvas.pixel(int(x), int(y), byte(color));
        }
        color += step;
    }
}
