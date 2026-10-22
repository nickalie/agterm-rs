//! Powerline and Powerline Extra, the geometric glyphs of U+E0B0–U+E0D4 (Ghostty's `draw/powerline.zig`, through
//! qwertty-term's `powerline.rs`): arrows, chevrons, half circles, corner triangles and trapezoids, drawn to the
//! cell's edges so a segment meets its neighbour without a seam. The stylized glyphs (flames, pixels, waves) are left
//! to the font, as in Ghostty.

use super::super::metrics::CellMetrics;
use super::box_drawing::{diagonal_falling, diagonal_rising};
use super::canvas::{Canvas, Path};
use super::common::{ON, Thickness};

pub(super) fn draw(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    let (width, height) = (f64::from(canvas.width()), f64::from(canvas.height()));
    match cp {
        0xe0b0 => canvas.triangle([(0.0, 0.0), (width, height / 2.0), (0.0, height)], ON),
        0xe0b1 | 0xe0b3 => chevron(canvas, m),
        0xe0b2 => canvas.triangle([(width, 0.0), (0.0, height / 2.0), (width, height)], ON),
        0xe0b4 | 0xe0b6 => half_circle(canvas),
        0xe0b5 | 0xe0b7 => half_circle_outline(canvas, m),
        0xe0b8 => canvas.triangle([(0.0, 0.0), (width, height), (0.0, height)], ON),
        0xe0b9 | 0xe0bf => diagonal_falling(m, canvas),
        0xe0ba => canvas.triangle([(width, 0.0), (width, height), (0.0, height)], ON),
        0xe0bb | 0xe0bd => diagonal_rising(m, canvas),
        0xe0bc => canvas.triangle([(0.0, 0.0), (width, 0.0), (0.0, height)], ON),
        0xe0be => canvas.triangle([(0.0, 0.0), (width, 0.0), (width, height)], ON),
        0xe0d2 | 0xe0d4 => trapezoids(canvas, m),
        _ => {}
    }
    // the left-pointing glyphs mirror the right-pointing ones
    if matches!(cp, 0xe0b3 | 0xe0b6 | 0xe0b7 | 0xe0d4) {
        canvas.flip_horizontal();
    }
}

/// U+E0B1, a right-pointing chevron, a light line.
fn chevron(canvas: &mut Canvas, m: &CellMetrics) {
    let (width, height) = (f64::from(canvas.width()), f64::from(canvas.height()));
    let mut path = Path::new();
    path.move_to((0.0, 0.0));
    path.line_to((width, height / 2.0));
    path.line_to((0.0, height));
    canvas.stroke(path, f64::from(Thickness::Light.height(m.box_thickness)), ON);
}

/// The control point offset that approximates a quarter circle with a cubic Bézier.
fn arc_coefficient() -> f64 {
    (std::f64::consts::SQRT_2 - 1.0) * 4.0 / 3.0
}

/// U+E0B4, a right half circle (an ellipse when the cell is too narrow), filled.
fn half_circle(canvas: &mut Canvas) {
    let (width, height) = (f64::from(canvas.width()), f64::from(canvas.height()));
    let c = arc_coefficient();
    let r = width.min(height / 2.0);
    let mut path = Path::new();
    path.move_to((0.0, 0.0));
    path.cubic_to((r * c, 0.0), (r, r - r * c), (r, r));
    path.line_to((r, height - r));
    path.cubic_to((r, height - r + r * c), (r * c, height), (0.0, height));
    path.close();
    canvas.fill(path, ON);
}

/// U+E0B5, the outline of [`half_circle`], inside it. The short straight runs at both ends keep the inset ends square.
fn half_circle_outline(canvas: &mut Canvas, m: &CellMetrics) {
    let (width, height) = (f64::from(canvas.width()), f64::from(canvas.height()));
    let c = arc_coefficient();
    let r = width.min(height / 2.0);
    let mut path = Path::new();
    path.move_to((0.0, 0.0));
    path.line_to((1.0, 0.0));
    path.cubic_to((r * c, 0.0), (r, r - r * c), (r, r));
    path.line_to((r, height - r));
    path.cubic_to((r, height - r + r * c), (r * c, height), (1.0, height));
    path.line_to((0.0, height));
    canvas.inner_stroke(path, f64::from(m.box_thickness), ON);
}

/// U+E0D2, two trapezoids around a box-thickness gap at mid-height.
fn trapezoids(canvas: &mut Canvas, m: &CellMetrics) {
    let (width, height) = (f64::from(canvas.width()), f64::from(canvas.height()));
    let half_thick = f64::from(m.box_thickness) / 2.0;
    let (top, bottom) = (height / 2.0 - half_thick, height / 2.0 + half_thick);
    canvas.fill(
        Path::polygon(&[(0.0, 0.0), (width, 0.0), (width / 2.0, top), (0.0, top)]),
        ON,
    );
    canvas.fill(
        Path::polygon(&[(0.0, height), (width, height), (width / 2.0, bottom), (0.0, bottom)]),
        ON,
    );
}
