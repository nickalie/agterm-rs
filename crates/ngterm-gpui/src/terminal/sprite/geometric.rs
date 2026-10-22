//! Geometric Shapes, the corner triangles of U+25E2–U+25FF (Ghostty's `draw/geometric_shapes.zig`, through
//! qwertty-term's `geometric_shapes.rs`); [`corner_triangle_shade`] serves the legacy computing glyphs too.

use super::super::metrics::CellMetrics;
use super::canvas::{Canvas, Path};
use super::common::{Corner, ON, Shade, Thickness};

pub(super) fn draw(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    match cp {
        0x25e2 => corner_triangle_shade(m, canvas, Corner::Br, Shade::On),
        0x25e3 => corner_triangle_shade(m, canvas, Corner::Bl, Shade::On),
        0x25e4 => corner_triangle_shade(m, canvas, Corner::Tl, Shade::On),
        0x25e5 => corner_triangle_shade(m, canvas, Corner::Tr, Shade::On),
        0x25f8 => corner_triangle_outline(m, canvas, Corner::Tl),
        0x25f9 => corner_triangle_outline(m, canvas, Corner::Tr),
        0x25fa => corner_triangle_outline(m, canvas, Corner::Bl),
        0x25ff => corner_triangle_outline(m, canvas, Corner::Br),
        _ => {}
    }
}

/// The right triangle filling the `corner` half of the cell.
fn corner_points(m: &CellMetrics, corner: Corner) -> [(f64, f64); 3] {
    let (width, height) = (f64::from(m.cell_width), f64::from(m.cell_height));
    match corner {
        Corner::Tl => [(0.0, 0.0), (0.0, height), (width, 0.0)],
        Corner::Tr => [(0.0, 0.0), (width, height), (width, 0.0)],
        Corner::Bl => [(0.0, 0.0), (0.0, height), (width, height)],
        Corner::Br => [(0.0, height), (width, height), (width, 0.0)],
    }
}

/// The `corner` half of the cell in `shade`.
pub(super) fn corner_triangle_shade(m: &CellMetrics, canvas: &mut Canvas, corner: Corner, shade: Shade) {
    canvas.triangle(corner_points(m, corner), shade.alpha());
}

/// The outline of the `corner` half of the cell, inside it.
fn corner_triangle_outline(m: &CellMetrics, canvas: &mut Canvas, corner: Corner) {
    let thick = f64::from(Thickness::Light.height(m.box_thickness));
    canvas.inner_stroke(Path::polygon(&corner_points(m, corner)), thick, ON);
}
