//! The small vector painter the sidebar's glyphs are traced with: cairo's path model (a current point, `arc`
//! continuing from it, `new_sub_path`, `close_path`) over tiny-skia, inking into an 8-bit coverage mask. A knock-out
//! (`clear_*`) removes coverage, as cairo's `CLEAR` operator inside a pushed group does in the GTK build, so a cut-out
//! is real transparency whatever lies beneath once the mask is painted.

use std::f64::consts::{FRAC_PI_2, TAU};

use tiny_skia::{BlendMode, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

/// An `f64` design coordinate as tiny-skia's `f32`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "glyph coordinates are a few dozen units, far inside f32's range"
)]
fn single(value: f64) -> f32 {
    value as f32
}

/// A path traced in design units, with cairo's current point.
#[derive(Default)]
pub struct Trace {
    builder: PathBuilder,
    /// The current point, `None` at the start of a sub-path (cairo's `new_sub_path`).
    current: Option<(f64, f64)>,
    /// Where the open sub-path started: the current point once it is closed.
    start: Option<(f64, f64)>,
}

impl Trace {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(&mut self, (x, y): (f64, f64)) {
        self.builder.move_to(single(x), single(y));
        self.current = Some((x, y));
        self.start = Some((x, y));
    }

    pub fn line_to(&mut self, point: (f64, f64)) {
        if self.current.is_none() {
            self.move_to(point);
            return;
        }
        self.builder.line_to(single(point.0), single(point.1));
        self.current = Some(point);
    }

    /// A cubic Bézier through the control points `c1` and `c2` to `to`.
    pub fn curve_to(&mut self, c1: (f64, f64), c2: (f64, f64), to: (f64, f64)) {
        if self.current.is_none() {
            self.move_to(c1);
        }
        self.builder.cubic_to(
            single(c1.0),
            single(c1.1),
            single(c2.0),
            single(c2.1),
            single(to.0),
            single(to.1),
        );
        self.current = Some(to);
    }

    /// Ends the current point: the next segment starts a sub-path of its own (cairo's `new_sub_path`).
    pub fn new_sub_path(&mut self) {
        self.current = None;
    }

    pub fn close(&mut self) {
        self.builder.close();
        self.current = self.start;
    }

    /// Cairo's `arc`: the circle `(x, y, radius)` from angle `from` clockwise (in screen coordinates) to `to`, joined
    /// to the current point by a line; `to` below `from` goes round once more.
    pub fn arc(&mut self, (x, y, radius): (f64, f64, f64), from: f64, to: f64) {
        let mut to = to;
        while to < from {
            to += TAU;
        }
        let point = |angle: f64| (x + radius * angle.cos(), y + radius * angle.sin());
        self.line_to(point(from));
        let sweep = to - from;
        if sweep <= 0.0 {
            return;
        }
        let pieces = (sweep / FRAC_PI_2).ceil().max(1.0);
        let step = sweep / pieces;
        // the control-point distance of a cubic approximating an arc of `step` radians
        let reach = radius * 4.0 / 3.0 * (step / 4.0).tan();
        let mut angle = from;
        while angle < to - step / 2.0 {
            let next = angle + step;
            let (start, end) = (point(angle), point(next));
            let c1 = (start.0 - reach * angle.sin(), start.1 + reach * angle.cos());
            let c2 = (end.0 + reach * next.sin(), end.1 - reach * next.cos());
            self.curve_to(c1, c2, end);
            angle = next;
        }
    }

    /// The rectangle `(left, top, width, height)` with corners of `radius`, as a new closed sub-path.
    pub fn rounded_rect(&mut self, (left, top, width, height): (f64, f64, f64, f64), radius: f64) {
        let radius = radius.clamp(0.0, width.min(height) / 2.0);
        let (right, bottom) = (left + width, top + height);
        self.new_sub_path();
        self.arc((right - radius, top + radius, radius), -FRAC_PI_2, 0.0);
        self.arc((right - radius, bottom - radius, radius), 0.0, FRAC_PI_2);
        self.arc((left + radius, bottom - radius, radius), FRAC_PI_2, 2.0 * FRAC_PI_2);
        self.arc((left + radius, top + radius, radius), 2.0 * FRAC_PI_2, 3.0 * FRAC_PI_2);
        self.close();
    }

    /// An open polyline through `points`, as a new sub-path.
    pub fn polyline(&mut self, points: &[(f64, f64)]) {
        self.new_sub_path();
        for &point in points {
            self.line_to(point);
        }
    }

    /// A closed polygon through `points`, as a new sub-path.
    pub fn polygon(&mut self, points: &[(f64, f64)]) {
        self.polyline(points);
        self.close();
    }

    fn finish(self) -> Option<Path> {
        self.builder.finish()
    }
}

/// A square coverage canvas of `side` pixels the 16-unit design is scaled onto.
pub struct Ink {
    pixmap: Option<Pixmap>,
    transform: Transform,
    side: u32,
}

impl Ink {
    /// A blank canvas `side` pixels square, `design` units mapped onto it.
    #[must_use]
    pub fn new(side: u32, design: f64) -> Self {
        let scale = single(f64::from(side) / design);
        Self {
            pixmap: Pixmap::new(side.max(1), side.max(1)),
            transform: Transform::from_scale(scale, scale),
            side,
        }
    }

    /// A canvas `slot + 2 * margin` pixels square, the design mapped onto the `slot` square inside the empty margin:
    /// what the glyphs' bounds are checked on.
    #[cfg(test)]
    #[must_use]
    pub fn framed(slot: u32, margin: u32, design: f64) -> Self {
        let side = slot + 2 * margin;
        let scale = single(f64::from(slot) / design);
        let offset = single(f64::from(margin));
        Self {
            pixmap: Pixmap::new(side, side),
            transform: Transform::from_row(scale, 0.0, 0.0, scale, offset, offset),
            side,
        }
    }

    fn paint(clear: bool) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color_rgba8(255, 255, 255, 255);
        paint.anti_alias = true;
        if clear {
            paint.blend_mode = BlendMode::Clear;
        }
        paint
    }

    fn stroke_with(&mut self, path: &Path, width: f64, clear: bool) {
        let stroke = Stroke {
            width: single(width),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        let transform = self.transform;
        if let Some(pixmap) = self.pixmap.as_mut() {
            pixmap.stroke_path(path, &Self::paint(clear), &stroke, transform, None);
        }
    }

    fn fill_with(&mut self, path: &Path, clear: bool) {
        let transform = self.transform;
        if let Some(pixmap) = self.pixmap.as_mut() {
            pixmap.fill_path(path, &Self::paint(clear), FillRule::Winding, transform, None);
        }
    }

    /// Inks `trace`'s outline at `width` design units, round caps and joins.
    pub fn stroke(&mut self, trace: Trace, width: f64) {
        if let Some(path) = trace.finish() {
            self.stroke_with(&path, width, false);
        }
    }

    /// Inks `trace`'s inside.
    pub fn fill(&mut self, trace: Trace) {
        if let Some(path) = trace.finish() {
            self.fill_with(&path, false);
        }
    }

    /// Clears the coverage under `trace`'s outline at `width`, then inks the outline at `inked` (a slash set off by a
    /// halo: cairo's `stroke_preserve` under `CLEAR`, then `stroke`). `inked` `None` only clears.
    pub fn clear_stroke(&mut self, trace: Trace, width: f64, inked: Option<f64>) {
        let Some(path) = trace.finish() else { return };
        self.stroke_with(&path, width, true);
        if let Some(inked) = inked {
            self.stroke_with(&path, inked, false);
        }
    }

    /// Clears the coverage under `trace`'s inside.
    pub fn clear_fill(&mut self, trace: Trace) {
        if let Some(path) = trace.finish() {
            self.fill_with(&path, true);
        }
    }

    /// The canvas's coverage, row-major, one byte per pixel.
    #[must_use]
    pub fn coverage(&self) -> Vec<u8> {
        let length = usize::try_from(self.side * self.side).unwrap_or(0);
        self.pixmap.as_ref().map_or_else(
            || vec![0; length],
            |pixmap| pixmap.pixels().iter().map(|pixel| pixel.alpha()).collect(),
        )
    }
}
