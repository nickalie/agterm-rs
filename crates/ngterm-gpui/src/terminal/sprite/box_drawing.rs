//! Box Drawing, U+2500–U+257F (Ghostty's `draw/box.zig`, through qwertty-term's `box_drawing.rs`): a table of what
//! each character is, the intersection renderer [`lines`] and the [`arc`] the branch glyphs share.

use super::super::metrics::CellMetrics;
use super::canvas::{Canvas, Path};
use super::common::{Corner, ON, Thickness, hline, hline_middle, int, vline, vline_middle};

/// The style of one arm of an intersection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Style {
    #[default]
    None,
    Light,
    Heavy,
    Double,
}

/// The four arms of an intersection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Lines {
    pub up: Style,
    pub right: Style,
    pub down: Style,
    pub left: Style,
}

/// The gap between dashes: at least four pixels (and a light line), or a line's thickness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gap {
    Four,
    Line(Thickness),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Diagonal {
    /// ╱
    Rising,
    /// ╲
    Falling,
    /// ╳
    Cross,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glyph {
    Lines(Lines),
    Dash {
        count: u8,
        vertical: bool,
        thickness: Thickness,
        gap: Gap,
    },
    Arc(Corner),
    Diagonal(Diagonal),
}

const N: Style = Style::None;
const L: Style = Style::Light;
const H: Style = Style::Heavy;
const D: Style = Style::Double;
const LIGHT: Thickness = Thickness::Light;
const HEAVY: Thickness = Thickness::Heavy;

/// Arms up, right, down, left.
pub(super) const fn arms(up: Style, right: Style, down: Style, left: Style) -> Lines {
    Lines { up, right, down, left }
}

const fn lines(up: Style, right: Style, down: Style, left: Style) -> Glyph {
    Glyph::Lines(arms(up, right, down, left))
}

const fn dash(count: u8, vertical: bool, thickness: Thickness, gap: Gap) -> Glyph {
    Glyph::Dash {
        count,
        vertical,
        thickness,
        gap,
    }
}

/// U+2500 + index.
const GLYPHS: [Glyph; 0x80] = [
    lines(N, L, N, L),                       // U+2500
    lines(N, H, N, H),                       // U+2501
    lines(L, N, L, N),                       // U+2502
    lines(H, N, H, N),                       // U+2503
    dash(3, false, LIGHT, Gap::Four),        // U+2504
    dash(3, false, HEAVY, Gap::Four),        // U+2505
    dash(3, true, LIGHT, Gap::Four),         // U+2506
    dash(3, true, HEAVY, Gap::Four),         // U+2507
    dash(4, false, LIGHT, Gap::Four),        // U+2508
    dash(4, false, HEAVY, Gap::Four),        // U+2509
    dash(4, true, LIGHT, Gap::Four),         // U+250A
    dash(4, true, HEAVY, Gap::Four),         // U+250B
    lines(N, L, L, N),                       // U+250C
    lines(N, H, L, N),                       // U+250D
    lines(N, L, H, N),                       // U+250E
    lines(N, H, H, N),                       // U+250F
    lines(N, N, L, L),                       // U+2510
    lines(N, N, L, H),                       // U+2511
    lines(N, N, H, L),                       // U+2512
    lines(N, N, H, H),                       // U+2513
    lines(L, L, N, N),                       // U+2514
    lines(L, H, N, N),                       // U+2515
    lines(H, L, N, N),                       // U+2516
    lines(H, H, N, N),                       // U+2517
    lines(L, N, N, L),                       // U+2518
    lines(L, N, N, H),                       // U+2519
    lines(H, N, N, L),                       // U+251A
    lines(H, N, N, H),                       // U+251B
    lines(L, L, L, N),                       // U+251C
    lines(L, H, L, N),                       // U+251D
    lines(H, L, L, N),                       // U+251E
    lines(L, L, H, N),                       // U+251F
    lines(H, L, H, N),                       // U+2520
    lines(H, H, L, N),                       // U+2521
    lines(L, H, H, N),                       // U+2522
    lines(H, H, H, N),                       // U+2523
    lines(L, N, L, L),                       // U+2524
    lines(L, N, L, H),                       // U+2525
    lines(H, N, L, L),                       // U+2526
    lines(L, N, H, L),                       // U+2527
    lines(H, N, H, L),                       // U+2528
    lines(H, N, L, H),                       // U+2529
    lines(L, N, H, H),                       // U+252A
    lines(H, N, H, H),                       // U+252B
    lines(N, L, L, L),                       // U+252C
    lines(N, L, L, H),                       // U+252D
    lines(N, H, L, L),                       // U+252E
    lines(N, H, L, H),                       // U+252F
    lines(N, L, H, L),                       // U+2530
    lines(N, L, H, H),                       // U+2531
    lines(N, H, H, L),                       // U+2532
    lines(N, H, H, H),                       // U+2533
    lines(L, L, N, L),                       // U+2534
    lines(L, L, N, H),                       // U+2535
    lines(L, H, N, L),                       // U+2536
    lines(L, H, N, H),                       // U+2537
    lines(H, L, N, L),                       // U+2538
    lines(H, L, N, H),                       // U+2539
    lines(H, H, N, L),                       // U+253A
    lines(H, H, N, H),                       // U+253B
    lines(L, L, L, L),                       // U+253C
    lines(L, L, L, H),                       // U+253D
    lines(L, H, L, L),                       // U+253E
    lines(L, H, L, H),                       // U+253F
    lines(H, L, L, L),                       // U+2540
    lines(L, L, H, L),                       // U+2541
    lines(H, L, H, L),                       // U+2542
    lines(H, L, L, H),                       // U+2543
    lines(H, H, L, L),                       // U+2544
    lines(L, L, H, H),                       // U+2545
    lines(L, H, H, L),                       // U+2546
    lines(H, H, L, H),                       // U+2547
    lines(L, H, H, H),                       // U+2548
    lines(H, L, H, H),                       // U+2549
    lines(H, H, H, L),                       // U+254A
    lines(H, H, H, H),                       // U+254B
    dash(2, false, LIGHT, Gap::Line(LIGHT)), // U+254C
    dash(2, false, HEAVY, Gap::Line(HEAVY)), // U+254D
    dash(2, true, LIGHT, Gap::Line(HEAVY)),  // U+254E
    dash(2, true, HEAVY, Gap::Line(HEAVY)),  // U+254F
    lines(N, D, N, D),                       // U+2550
    lines(D, N, D, N),                       // U+2551
    lines(N, D, L, N),                       // U+2552
    lines(N, L, D, N),                       // U+2553
    lines(N, D, D, N),                       // U+2554
    lines(N, N, L, D),                       // U+2555
    lines(N, N, D, L),                       // U+2556
    lines(N, N, D, D),                       // U+2557
    lines(L, D, N, N),                       // U+2558
    lines(D, L, N, N),                       // U+2559
    lines(D, D, N, N),                       // U+255A
    lines(L, N, N, D),                       // U+255B
    lines(D, N, N, L),                       // U+255C
    lines(D, N, N, D),                       // U+255D
    lines(L, D, L, N),                       // U+255E
    lines(D, L, D, N),                       // U+255F
    lines(D, D, D, N),                       // U+2560
    lines(L, N, L, D),                       // U+2561
    lines(D, N, D, L),                       // U+2562
    lines(D, N, D, D),                       // U+2563
    lines(N, D, L, D),                       // U+2564
    lines(N, L, D, L),                       // U+2565
    lines(N, D, D, D),                       // U+2566
    lines(L, D, N, D),                       // U+2567
    lines(D, L, N, L),                       // U+2568
    lines(D, D, N, D),                       // U+2569
    lines(L, D, L, D),                       // U+256A
    lines(D, L, D, L),                       // U+256B
    lines(D, D, D, D),                       // U+256C
    Glyph::Arc(Corner::Br),                  // U+256D
    Glyph::Arc(Corner::Bl),                  // U+256E
    Glyph::Arc(Corner::Tl),                  // U+256F
    Glyph::Arc(Corner::Tr),                  // U+2570
    Glyph::Diagonal(Diagonal::Rising),       // U+2571
    Glyph::Diagonal(Diagonal::Falling),      // U+2572
    Glyph::Diagonal(Diagonal::Cross),        // U+2573
    lines(N, N, N, L),                       // U+2574
    lines(L, N, N, N),                       // U+2575
    lines(N, L, N, N),                       // U+2576
    lines(N, N, L, N),                       // U+2577
    lines(N, N, N, H),                       // U+2578
    lines(H, N, N, N),                       // U+2579
    lines(N, H, N, N),                       // U+257A
    lines(N, N, H, N),                       // U+257B
    lines(N, H, N, L),                       // U+257C
    lines(L, N, H, N),                       // U+257D
    lines(N, L, N, H),                       // U+257E
    lines(H, N, L, N),                       // U+257F
];

pub(super) fn draw(cp: u32, canvas: &mut Canvas, m: &CellMetrics) {
    let Some(glyph) = cp
        .checked_sub(0x2500)
        .and_then(|index| GLYPHS.get(usize::try_from(index).ok()?))
    else {
        return;
    };
    match *glyph {
        Glyph::Lines(arms) => lines_char(m, canvas, arms),
        Glyph::Dash {
            count,
            vertical,
            thickness,
            gap,
        } => {
            let gap = match gap {
                Gap::Four => LIGHT.height(m.box_thickness).max(4),
                Gap::Line(thickness) => thickness.height(m.box_thickness),
            };
            let thick = thickness.height(m.box_thickness);
            if vertical {
                dash_vertical(m, canvas, count, thick, gap);
            } else {
                dash_horizontal(m, canvas, count, thick, gap);
            }
        }
        Glyph::Arc(corner) => arc(m, canvas, corner, LIGHT),
        Glyph::Diagonal(Diagonal::Rising) => diagonal_rising(m, canvas),
        Glyph::Diagonal(Diagonal::Falling) => diagonal_falling(m, canvas),
        Glyph::Diagonal(Diagonal::Cross) => diagonal_cross(m, canvas),
    }
}

/// Where the strokes of an intersection sit: the light, heavy and double strokes of each axis.
struct Strokes {
    /// Horizontal strokes: top and bottom.
    h_light: (u32, u32),
    h_heavy: (u32, u32),
    /// The outer edges of a double stroke (the inner ones are the light stroke's).
    h_double: (u32, u32),
    /// Vertical strokes: left and right.
    v_light: (u32, u32),
    v_heavy: (u32, u32),
    v_double: (u32, u32),
}

impl Strokes {
    fn new(m: &CellMetrics) -> Self {
        let light = LIGHT.height(m.box_thickness);
        let heavy = HEAVY.height(m.box_thickness);
        let centered = |size: u32, thick: u32| {
            let start = size.saturating_sub(thick) / 2;
            (start, start + thick)
        };
        let h_light = centered(m.cell_height, light);
        let v_light = centered(m.cell_width, light);
        Self {
            h_light,
            h_heavy: centered(m.cell_height, heavy),
            h_double: (h_light.0.saturating_sub(light), h_light.1 + light),
            v_light,
            v_heavy: centered(m.cell_width, heavy),
            v_double: (v_light.0.saturating_sub(light), v_light.1 + light),
        }
    }
}

/// Where the arms along one axis stop at the crossing, given the arms across it (`across`) and the two along it
/// (`along`, the one whose end is asked first): the near and far end of a light, heavy or double crossing stroke
/// (Ghostty's `up_bottom`, `down_top`, `left_right`, `right_left`, whose precedence this keeps).
fn reach(across: (Style, Style), along: (Style, Style), light: (u32, u32), heavy: u32, double: u32) -> u32 {
    let (a, b) = across;
    if a == H || b == H {
        heavy
    } else if a != b || along.0 == along.1 {
        if a == D || b == D { double } else { light.1 }
    } else if a == N && b == N {
        light.1
    } else {
        light.0
    }
}

/// Draws an intersection: each arm to the crossing, meeting the others without gaps or overlaps at any cell size.
pub(super) fn lines_char(m: &CellMetrics, canvas: &mut Canvas, lines: Lines) {
    let s = Strokes::new(m);
    let flip = |(start, end): (u32, u32)| (end, start);
    let up_bottom = reach(
        (lines.left, lines.right),
        (lines.down, lines.up),
        s.h_light,
        s.h_heavy.1,
        s.h_double.1,
    );
    let down_top = reach(
        (lines.left, lines.right),
        (lines.up, lines.down),
        flip(s.h_light),
        s.h_heavy.0,
        s.h_double.0,
    );
    let left_right = reach(
        (lines.up, lines.down),
        (lines.left, lines.right),
        s.v_light,
        s.v_heavy.1,
        s.v_double.1,
    );
    let right_left = reach(
        (lines.up, lines.down),
        (lines.right, lines.left),
        flip(s.v_light),
        s.v_heavy.0,
        s.v_double.0,
    );
    let arm = |style, sides, from_start, reach, inner| Arm {
        style,
        sides,
        from_start,
        reach,
        inner,
    };
    let (across_v, across_h) = ((lines.left, lines.right), (lines.up, lines.down));
    let up = arm(lines.up, across_v, true, up_bottom, s.h_light.0);
    let down = arm(lines.down, across_v, false, down_top, s.h_light.1);
    let right = arm(lines.right, across_h, false, right_left, s.v_light.1);
    let left = arm(lines.left, across_h, true, left_right, s.v_light.0);
    vertical_arm(canvas, &s, &up, m.cell_height);
    vertical_arm(canvas, &s, &down, m.cell_height);
    horizontal_arm(canvas, &s, &right, m.cell_width);
    horizontal_arm(canvas, &s, &left, m.cell_width);
}

fn fill(canvas: &mut Canvas, (x0, y0): (u32, u32), (x1, y1): (u32, u32)) {
    canvas.box_fill((int(x0), int(y0)), (int(x1), int(y1)), ON);
}

/// One arm of an intersection: its style, the arms either side of it (left then right of a vertical arm, up then
/// down of a horizontal one), where it reaches at the crossing, and the inner edge of the light stroke across it,
/// where a half of a double arm stops when the arm on its side is double too.
struct Arm {
    style: Style,
    sides: (Style, Style),
    /// Up and left arms run from the cell's edge at 0 to `reach`; down and right ones from `reach` to the far edge.
    from_start: bool,
    reach: u32,
    inner: u32,
}

impl Arm {
    fn span(&self, far: u32) -> (u32, u32) {
        if self.from_start {
            (0, self.reach)
        } else {
            (self.reach, far)
        }
    }

    /// Where the half of a double arm on `side` ends at the crossing.
    fn end(&self, side: Style) -> u32 {
        if side == D { self.inner } else { self.reach }
    }
}

/// The up or down arm.
fn vertical_arm(canvas: &mut Canvas, s: &Strokes, arm: &Arm, height: u32) {
    let (top, bottom) = arm.span(height);
    let (left, right) = (arm.end(arm.sides.0), arm.end(arm.sides.1));
    match arm.style {
        Style::None => {}
        Style::Light => fill(canvas, (s.v_light.0, top), (s.v_light.1, bottom)),
        Style::Heavy => fill(canvas, (s.v_heavy.0, top), (s.v_heavy.1, bottom)),
        Style::Double if arm.from_start => {
            fill(canvas, (s.v_double.0, 0), (s.v_light.0, left));
            fill(canvas, (s.v_light.1, 0), (s.v_double.1, right));
        }
        Style::Double => {
            fill(canvas, (s.v_double.0, left), (s.v_light.0, height));
            fill(canvas, (s.v_light.1, right), (s.v_double.1, height));
        }
    }
}

/// The left or right arm.
fn horizontal_arm(canvas: &mut Canvas, s: &Strokes, arm: &Arm, width: u32) {
    let (left, right) = arm.span(width);
    let (top, bottom) = (arm.end(arm.sides.0), arm.end(arm.sides.1));
    match arm.style {
        Style::None => {}
        Style::Light => fill(canvas, (left, s.h_light.0), (right, s.h_light.1)),
        Style::Heavy => fill(canvas, (left, s.h_heavy.0), (right, s.h_heavy.1)),
        Style::Double if arm.from_start => {
            fill(canvas, (0, s.h_double.0), (top, s.h_light.0));
            fill(canvas, (0, s.h_light.1), (bottom, s.h_double.1));
        }
        Style::Double => {
            fill(canvas, (top, s.h_double.0), (width, s.h_light.0));
            fill(canvas, (bottom, s.h_light.1), (width, s.h_double.1));
        }
    }
}

/// The overshoot past the corners that keeps a diagonal's slope (Ghostty's `slope_x`, `slope_y`).
fn slopes(m: &CellMetrics) -> (f64, f64, f64, f64) {
    let (width, height) = (f64::from(m.cell_width), f64::from(m.cell_height));
    (width, height, (width / height).min(1.0), (height / width).min(1.0))
}

/// ╱ corner to corner.
pub(super) fn diagonal_rising(m: &CellMetrics, canvas: &mut Canvas) {
    let (width, height, sx, sy) = slopes(m);
    let thick = f64::from(LIGHT.height(m.box_thickness));
    canvas.line((width + 0.5 * sx, -0.5 * sy), (-0.5 * sx, height + 0.5 * sy), thick, ON);
}

/// ╲ corner to corner.
pub(super) fn diagonal_falling(m: &CellMetrics, canvas: &mut Canvas) {
    let (width, height, sx, sy) = slopes(m);
    let thick = f64::from(LIGHT.height(m.box_thickness));
    canvas.line((-0.5 * sx, -0.5 * sy), (width + 0.5 * sx, height + 0.5 * sy), thick, ON);
}

/// ╳
pub(super) fn diagonal_cross(m: &CellMetrics, canvas: &mut Canvas) {
    diagonal_rising(m, canvas);
    diagonal_falling(m, canvas);
}

/// A rounded corner joining the middle of the cell's edges on the `corner` side, aligned with the straight lines.
pub(super) fn arc(m: &CellMetrics, canvas: &mut Canvas, corner: Corner, thickness: Thickness) {
    let thick_px = thickness.height(m.box_thickness);
    let (width, height) = (f64::from(m.cell_width), f64::from(m.cell_height));
    let thick = f64::from(thick_px);
    let cx = f64::from(m.cell_width.saturating_sub(thick_px) / 2) + thick / 2.0;
    let cy = f64::from(m.cell_height.saturating_sub(thick_px) / 2) + thick / 2.0;
    let r = width.min(height) / 2.0;
    // the middle control points sit this fraction of the radius from the center
    let s = 0.25;
    let (dx, x_end) = match corner {
        Corner::Tl | Corner::Bl => (-1.0, 0.0),
        Corner::Tr | Corner::Br => (1.0, width),
    };
    let (dy, y_start) = match corner {
        Corner::Tl | Corner::Tr => (-1.0, 0.0),
        Corner::Bl | Corner::Br => (1.0, height),
    };
    let mut path = Path::new();
    path.move_to((cx, y_start));
    path.line_to((cx, cy + dy * r));
    path.cubic_to((cx, cy + dy * s * r), (cx + dx * s * r, cy), (cx + dx * r, cy));
    path.line_to((x_end, cy));
    canvas.stroke(path, thick, ON);
}

/// Splits `size` into `count` dashes and as many gaps of at most `desired_gap` (never half the space): the dashes'
/// lengths, the leftover pixels going to the first dashes, and the gap.
fn dashes(size: u32, count: u8, desired_gap: u32) -> (i32, i32, i32) {
    let count = i32::from(count);
    let gap = int(desired_gap.min(size / (2 * u32::try_from(count).unwrap_or(1))));
    let total_dash = int(size) - count * gap;
    (total_dash.div_euclid(count), total_dash.rem_euclid(count), gap)
}

/// A dashed horizontal line that tiles into an even one: half a gap at each end.
fn dash_horizontal(m: &CellMetrics, canvas: &mut Canvas, count: u8, thick: u32, desired_gap: u32) {
    if m.cell_width < 2 * u32::from(count) {
        hline_middle(m, canvas, LIGHT);
        return;
    }
    let (dash, mut extra, gap) = dashes(m.cell_width, count, desired_gap);
    let y = int(m.cell_height.saturating_sub(thick) / 2);
    let mut x = gap.div_euclid(2);
    for _ in 0..count {
        let mut x1 = x + dash;
        if extra > 0 {
            extra -= 1;
            x1 += 1;
        }
        hline(canvas, (x, x1), y, thick);
        x = x1 + gap;
    }
}

/// A dashed vertical line that tiles into an even one: a whole gap at the bottom.
fn dash_vertical(m: &CellMetrics, canvas: &mut Canvas, count: u8, thick: u32, desired_gap: u32) {
    if m.cell_height < 2 * u32::from(count) {
        vline_middle(m, canvas, LIGHT);
        return;
    }
    let (dash, mut extra, gap) = dashes(m.cell_height, count, desired_gap);
    let x = int(m.cell_width.saturating_sub(thick) / 2);
    let mut y = 0;
    for _ in 0..count {
        let mut y1 = y + dash;
        if extra > 0 {
            extra -= 1;
            y1 += 1;
        }
        vline(canvas, (y, y1), x, thick);
        y = y1 + gap;
    }
}
