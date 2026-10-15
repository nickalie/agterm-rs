//! The sprite canvas: an 8-bit coverage buffer around one cell (Ghostty's `src/font/sprite/canvas.zig`, through
//! qwertty-term's `canvas.rs`). The cell has a quarter of its size of padding on every side, so a figure may reach
//! past it (diagonals, decorations); coordinates are relative to the cell's top-left corner.
//!
//! Rectangles and pixels are written straight into the buffer, replacing what is there, as Ghostty's canvas does.
//! Paths are filled and stroked with tiny-skia where Ghostty uses z2d, in opaque white on a scratch pixmap whose
//! coverage is then laid over the buffer (`src_over`, scaled by the color's alpha). Ghostty's inner stroke (a stroke
//! inset by half its width, z2d's path offset) is a stroke of twice the width kept only inside the path's fill.

use tiny_skia::{FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

use super::common::{byte, int, single};

/// A path in cell coordinates, built in `f64` like Ghostty's.
#[derive(Default)]
pub(super) struct Path {
    builder: PathBuilder,
}

impl Path {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn move_to(&mut self, (x, y): (f64, f64)) {
        self.builder.move_to(single(x), single(y));
    }

    pub(super) fn line_to(&mut self, (x, y): (f64, f64)) {
        self.builder.line_to(single(x), single(y));
    }

    /// A cubic Bézier through the control points `c1` and `c2` to `to`.
    pub(super) fn cubic_to(&mut self, c1: (f64, f64), c2: (f64, f64), to: (f64, f64)) {
        self.builder.cubic_to(
            single(c1.0),
            single(c1.1),
            single(c2.0),
            single(c2.1),
            single(to.0),
            single(to.1),
        );
    }

    /// A whole circle around `center`.
    pub(super) fn circle(&mut self, (x, y): (f64, f64), radius: f64) {
        self.builder.push_circle(single(x), single(y), single(radius));
    }

    pub(super) fn close(&mut self) {
        self.builder.close();
    }

    /// A closed polygon through `points`.
    pub(super) fn polygon(points: &[(f64, f64)]) -> Self {
        let mut path = Self::new();
        for (index, point) in points.iter().enumerate() {
            if index == 0 {
                path.move_to(*point);
            } else {
                path.line_to(*point);
            }
        }
        path.close();
        path
    }
}

/// A rasterized sprite: coverage trimmed to its non-empty box, and where that box sits relative to the cell's
/// top-left corner (device pixels; negative is left of or above the cell).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    /// Row-major, `width * height` bytes, 0 transparent.
    pub alpha: Vec<u8>,
}

impl Mask {
    /// The coverage at `(x, y)` of the mask, 0 outside it.
    #[must_use]
    pub fn at(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let index = usize::try_from(y * self.width + x).unwrap_or(usize::MAX);
        self.alpha.get(index).copied().unwrap_or(0)
    }
}

/// The canvas a sprite is drawn on.
pub(super) struct Canvas {
    /// The cell (or the cells, for a wide character) the sprite is drawn for.
    width: u32,
    height: u32,
    padding_x: u32,
    padding_y: u32,
    /// The whole buffer, padding included.
    buf_width: u32,
    buf_height: u32,
    buf: Vec<u8>,
    /// Margins excluded from the mask: set to the padding by figures that must not leave the cell, grown over
    /// empty rows and columns when the mask is taken.
    clip_top: u32,
    clip_left: u32,
    clip_right: u32,
    clip_bottom: u32,
}

impl Canvas {
    /// A canvas for a `width` x `height` cell with a quarter of each as padding (Ghostty's `renderGlyph`).
    pub(super) fn new(width: u32, height: u32) -> Self {
        let (padding_x, padding_y) = (width / 4, height / 4);
        let buf_width = width + 2 * padding_x;
        let buf_height = height + 2 * padding_y;
        Self {
            width,
            height,
            padding_x,
            padding_y,
            buf_width,
            buf_height,
            buf: vec![0; usize::try_from(buf_width * buf_height).unwrap_or(0)],
            clip_top: 0,
            clip_left: 0,
            clip_right: 0,
            clip_bottom: 0,
        }
    }

    /// The width the sprite is drawn at (a wide character's two cells).
    pub(super) fn width(&self) -> u32 {
        self.width
    }

    pub(super) fn height(&self) -> u32 {
        self.height
    }

    /// Keeps the mask inside the cell: the padding is clipped away.
    pub(super) fn clip_to_cell(&mut self) {
        self.clip_left = self.padding_x;
        self.clip_right = self.padding_x;
        self.clip_top = self.padding_y;
        self.clip_bottom = self.padding_y;
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let x = u32::try_from(x + int(self.padding_x)).ok()?;
        let y = u32::try_from(y + int(self.padding_y)).ok()?;
        (x < self.buf_width && y < self.buf_height).then(|| usize::try_from(y * self.buf_width + x).ok())?
    }

    /// Sets one pixel to `alpha`, whatever was there.
    pub(super) fn pixel(&mut self, x: i32, y: i32, alpha: u8) {
        if let Some(index) = self.index(x, y) {
            self.buf[index] = alpha;
        }
    }

    /// Sets the pixels of the rectangle at `(x, y)` of `width` x `height` to `alpha`.
    pub(super) fn rect(&mut self, (x, y): (i32, i32), (width, height): (i32, i32), alpha: u8) {
        for row in y..y + height {
            for col in x..x + width {
                self.pixel(col, row, alpha);
            }
        }
    }

    /// Sets the pixels between two corners (in either order) to `alpha`.
    pub(super) fn box_fill(&mut self, (x0, y0): (i32, i32), (x1, y1): (i32, i32), alpha: u8) {
        let (left, right) = (x0.min(x1), x0.max(x1));
        let (top, bottom) = (y0.min(y1), y0.max(y1));
        self.rect((left, top), (right - left, bottom - top), alpha);
    }

    /// Fills a triangle.
    pub(super) fn triangle(&mut self, points: [(f64, f64); 3], alpha: u8) {
        self.fill(Path::polygon(&points), alpha);
    }

    /// Strokes a straight line with butt caps.
    pub(super) fn line(&mut self, from: (f64, f64), to: (f64, f64), thickness: f64, alpha: u8) {
        let mut path = Path::new();
        path.move_to(from);
        path.line_to(to);
        self.stroke(path, thickness, alpha);
    }

    /// Fills `path` (non-zero winding; an open path is closed).
    pub(super) fn fill(&mut self, path: Path, alpha: u8) {
        let Some(path) = path.builder.finish() else {
            return;
        };
        let Some(mut pixmap) = self.pixmap() else {
            return;
        };
        pixmap.fill_path(&path, &white(), FillRule::Winding, self.transform(), None);
        self.lay(&pixmap, None, alpha);
    }

    /// Strokes `path` with butt caps.
    pub(super) fn stroke(&mut self, path: Path, thickness: f64, alpha: u8) {
        let Some(path) = path.builder.finish() else {
            return;
        };
        let Some(mut pixmap) = self.pixmap() else {
            return;
        };
        pixmap.stroke_path(&path, &white(), &butt(thickness), self.transform(), None);
        self.lay(&pixmap, None, alpha);
    }

    /// Strokes `path` inset by half the thickness: the stroke stays inside the shape the path outlines.
    pub(super) fn inner_stroke(&mut self, path: Path, thickness: f64, alpha: u8) {
        let Some(path) = path.builder.finish() else {
            return;
        };
        let (Some(mut inside), Some(mut stroke)) = (self.pixmap(), self.pixmap()) else {
            return;
        };
        inside.fill_path(&path, &white(), FillRule::Winding, self.transform(), None);
        stroke.stroke_path(&path, &white(), &butt(thickness * 2.0), self.transform(), None);
        self.lay(&stroke, Some(&inside), alpha);
    }

    fn pixmap(&self) -> Option<Pixmap> {
        Pixmap::new(self.buf_width, self.buf_height)
    }

    fn transform(&self) -> Transform {
        #[expect(clippy::cast_precision_loss, reason = "paddings are a few dozen pixels")]
        let (x, y) = (self.padding_x as f32, self.padding_y as f32);
        Transform::from_translate(x, y)
    }

    /// Lays the coverage of `pixmap` (times that of `within`) over the buffer, scaled by `alpha`.
    fn lay(&mut self, pixmap: &Pixmap, within: Option<&Pixmap>, alpha: u8) {
        let scale = f64::from(alpha) / 255.0;
        let inside = within.map(Pixmap::data);
        let (pixels, _) = pixmap.data().as_chunks::<4>();
        for (index, (dst, src)) in self.buf.iter_mut().zip(pixels).enumerate() {
            let mut coverage = f64::from(src[3]) / 255.0;
            if let Some(inside) = inside {
                coverage *= f64::from(inside[index * 4 + 3]) / 255.0;
            }
            if coverage <= 0.0 {
                continue;
            }
            *dst = src_over(*dst, byte(255.0 * coverage * scale));
        }
    }

    /// Every pixel `v` becomes `255 - v`.
    pub(super) fn invert(&mut self) {
        for value in &mut self.buf {
            *value = 255 - *value;
        }
    }

    /// Mirrors the buffer left to right.
    pub(super) fn flip_horizontal(&mut self) {
        let width = usize::try_from(self.buf_width).unwrap_or(0);
        if width > 0 {
            for row in self.buf.chunks_exact_mut(width) {
                row.reverse();
            }
        }
        std::mem::swap(&mut self.clip_left, &mut self.clip_right);
    }

    /// The mask: the buffer within the clip margins, trimmed of empty rows and columns, placed relative to the cell
    /// (Ghostty's `writeAtlas` and the offsets of `renderGlyph`).
    pub(super) fn into_mask(mut self) -> Mask {
        self.trim();
        let width = self.buf_width.saturating_sub(self.clip_left + self.clip_right);
        let height = self.buf_height.saturating_sub(self.clip_top + self.clip_bottom);
        let mut alpha = Vec::with_capacity(usize::try_from(width * height).unwrap_or(0));
        for y in self.clip_top..self.clip_top + height {
            let start = usize::try_from(y * self.buf_width + self.clip_left).unwrap_or(0);
            let end = start + usize::try_from(width).unwrap_or(0);
            alpha.extend_from_slice(&self.buf[start..end]);
        }
        Mask {
            width,
            height,
            left: int(self.clip_left) - int(self.padding_x),
            top: int(self.clip_top) - int(self.padding_y),
            alpha,
        }
    }

    fn row_empty(&self, y: u32) -> bool {
        (self.clip_left..self.buf_width - self.clip_right).all(|x| self.value(x, y) == 0)
    }

    fn column_empty(&self, x: u32) -> bool {
        (self.clip_top..self.buf_height - self.clip_bottom).all(|y| self.value(x, y) == 0)
    }

    fn value(&self, x: u32, y: u32) -> u8 {
        usize::try_from(y * self.buf_width + x)
            .ok()
            .and_then(|index| self.buf.get(index).copied())
            .unwrap_or(0)
    }

    /// Grows the clip margins over empty rows and columns (Ghostty's `trim`).
    fn trim(&mut self) {
        while self.clip_top < self.buf_height.saturating_sub(self.clip_bottom) && self.row_empty(self.clip_top) {
            self.clip_top += 1;
        }
        while self.clip_bottom < self.buf_height.saturating_sub(self.clip_top)
            && self.row_empty(self.buf_height - self.clip_bottom - 1)
        {
            self.clip_bottom += 1;
        }
        while self.clip_left < self.buf_width.saturating_sub(self.clip_right) && self.column_empty(self.clip_left) {
            self.clip_left += 1;
        }
        while self.clip_right < self.buf_width.saturating_sub(self.clip_left)
            && self.column_empty(self.buf_width - self.clip_right - 1)
        {
            self.clip_right += 1;
        }
    }
}

fn white() -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, 255);
    paint.anti_alias = true;
    paint
}

fn butt(width: f64) -> Stroke {
    Stroke {
        width: single(width),
        line_cap: LineCap::Butt,
        ..Stroke::default()
    }
}

/// `src` over `dst`, both coverages.
fn src_over(dst: u8, src: u8) -> u8 {
    match src {
        255 => 255,
        0 => dst,
        _ => {
            let (src, dst) = (f64::from(src) / 255.0, f64::from(dst) / 255.0);
            byte((src + dst * (1.0 - src)) * 255.0)
        }
    }
}
