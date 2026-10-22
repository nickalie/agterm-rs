//! Braille Patterns, U+2800–U+28FF (Ghostty's `draw/braille.zig`, through qwertty-term's `braille.rs`): eight square
//! dots sized by Ghostty's five greedy passes (dot width, margins, spacing, margins again, dot width again), so they
//! stay crisp and evenly spread at any cell size.

use super::super::metrics::CellMetrics;
use super::canvas::Canvas;
use super::common::{ON, int};

/// The dots' layout in a cell.
struct Layout {
    /// The dots' side.
    dot: i32,
    /// The left edges of the two columns.
    x: [i32; 2],
    /// The top edges of the four rows.
    y: [i32; 4],
}

/// What is left of each axis while the passes grow the layout.
struct Budget {
    dot: i32,
    x_spacing: i32,
    y_spacing: i32,
    x_margin: i32,
    y_margin: i32,
    x_left: i32,
    y_left: i32,
}

impl Budget {
    fn new(width: i32, height: i32) -> Self {
        let dot = (width / 4).min(height / 8);
        let (x_spacing, y_spacing) = (width / 4, height / 8);
        let (x_margin, y_margin) = (x_spacing.div_euclid(2), y_spacing.div_euclid(2));
        Self {
            dot,
            x_spacing,
            y_spacing,
            x_margin,
            y_margin,
            x_left: width - 2 * x_margin - x_spacing - 2 * dot,
            y_left: height - 2 * y_margin - 3 * y_spacing - 4 * dot,
        }
    }

    /// One pixel more of dot, if both axes have room.
    fn grow_dot(&mut self) {
        if self.x_left >= 2 && self.y_left >= 4 {
            self.dot += 1;
            self.x_left -= 2;
            self.y_left -= 4;
        }
    }

    fn grow_margins(&mut self, only_from_zero: bool) {
        if self.x_left >= 2 && (!only_from_zero || self.x_margin == 0) {
            self.x_margin += 1;
            self.x_left -= 2;
        }
        if self.y_left >= 2 && (!only_from_zero || self.y_margin == 0) {
            self.y_margin += 1;
            self.y_left -= 2;
        }
    }

    fn grow_spacing(&mut self) {
        if self.x_left >= 1 {
            self.x_spacing += 1;
            self.x_left -= 1;
        }
        if self.y_left >= 3 {
            self.y_spacing += 1;
            self.y_left -= 3;
        }
    }
}

impl Layout {
    fn new(width: u32, height: u32) -> Self {
        let mut budget = Budget::new(int(width), int(height));
        // 1: a dot of at least a pixel
        if budget.dot == 0 {
            budget.grow_dot();
        }
        // 2: a margin of at least a pixel; 3: wider spacing; 4: wider margins; 5: a wider dot
        budget.grow_margins(true);
        budget.grow_spacing();
        budget.grow_margins(false);
        budget.grow_dot();
        let Budget {
            dot,
            x_spacing,
            y_spacing,
            x_margin,
            y_margin,
            ..
        } = budget;
        let row = |index: i32| y_margin + index * (dot + y_spacing);
        Self {
            dot,
            x: [x_margin, x_margin + dot + x_spacing],
            y: [row(0), row(1), row(2), row(3)],
        }
    }
}

/// Each bit of the code point's low byte, in order, and its dot's column and row.
const DOTS: [(usize, usize); 8] = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)];

pub(super) fn draw(cp: u32, canvas: &mut Canvas, _m: &CellMetrics) {
    let layout = Layout::new(canvas.width(), canvas.height());
    for (bit, (col, row)) in DOTS.into_iter().enumerate() {
        if cp & (1 << bit) != 0 {
            let (x, y) = (layout.x[col], layout.y[row]);
            canvas.box_fill((x, y), (x + layout.dot, y + layout.dot), ON);
        }
    }
}
