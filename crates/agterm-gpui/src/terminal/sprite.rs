//! Procedural sprites: the box drawing, block, braille, powerline, branch, geometric shape and legacy computing
//! characters Ghostty draws itself instead of taking them from a font, so they meet their neighbours without gaps at
//! any cell size (plan > Task 15). The element routes these code points away from the font.
//!
//! Ported from qwertty-term (<https://github.com/joshka/qwertty-term>, MIT, Copyright (c) 2026 Josh `McKinney`):
//! `qwertty-term-sprite`, a port of Ghostty's sprite font (`src/font/sprite/`, MIT, Copyright (c) 2024 Mitchell
//! Hashimoto, Ghostty contributors), brought to the Ghostty of `LIBGHOSTTY_VT_REV` (build/pins.env) where Ghostty
//! changed since (the outlined powerline half circles' square ends). Adapted: each family's characters are a table
//! instead of a match per code point, sprites are drawn for [`CellMetrics`], and the cursor and decoration sprites stay
//! in the element (Task 13). The goldens in `tests/sprite_golden.rs` are Ghostty's own at that revision.
//!
//! A sprite is rasterized on the CPU into an 8-bit coverage mask as Ghostty's canvas does it (paths through
//! tiny-skia where Ghostty uses z2d, rectangles and pixels written straight into the mask), on a canvas padded by a
//! quarter of the cell, and trimmed. The element paints the mask with `Window::paint_alpha_mask` (GPUI patch 0003) in
//! the cell's foreground color, one texel per device pixel, as Ghostty's renderer draws a sprite glyph from its atlas.

mod block;
mod box_drawing;
mod braille;
mod branch;
mod canvas;
mod common;
mod geometric;
mod legacy;
mod legacy_supplement;
mod powerline;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub use canvas::Mask;

use super::metrics::CellMetrics;
use canvas::Canvas;

type Draw = fn(u32, &mut Canvas, &CellMetrics);

/// The code points Ghostty's sprite face draws at `LIBGHOSTTY_VT_REV` (its `draw<MIN>_<MAX>` functions, sorted and
/// disjoint), and the family that draws each range. Code points between the ranges come from the font.
const RANGES: &[(u32, u32, Draw)] = &[
    (0x2500, 0x257f, box_drawing::draw),
    (0x2580, 0x259f, block::draw),
    (0x25e2, 0x25e5, geometric::draw),
    (0x25f8, 0x25fa, geometric::draw),
    (0x25ff, 0x25ff, geometric::draw),
    (0x2800, 0x28ff, braille::draw),
    (0xe0b0, 0xe0bf, powerline::draw),
    (0xe0d2, 0xe0d2, powerline::draw),
    (0xe0d4, 0xe0d4, powerline::draw),
    (0xf5d0, 0xf60d, branch::draw),
    (0x1cc1b, 0x1cc1e, legacy_supplement::draw),
    (0x1cc21, 0x1cc2f, legacy_supplement::draw),
    (0x1cc30, 0x1cc3f, legacy_supplement::draw),
    (0x1cd00, 0x1cde5, legacy_supplement::draw),
    (0x1ce00, 0x1ce01, legacy_supplement::draw),
    (0x1ce0b, 0x1ce0c, legacy_supplement::draw),
    (0x1ce16, 0x1ce19, legacy_supplement::draw),
    (0x1ce51, 0x1ce8f, legacy_supplement::draw),
    (0x1ce90, 0x1ceaf, legacy_supplement::draw),
    (0x1fb00, 0x1fbaf, legacy::draw),
    (0x1fbbd, 0x1fbbf, legacy::draw),
    (0x1fbce, 0x1fbef, legacy::draw),
];

fn draw_fn(codepoint: u32) -> Option<Draw> {
    let index = RANGES.partition_point(|(_, max, _)| *max < codepoint);
    RANGES
        .get(index)
        .filter(|(min, _, _)| *min <= codepoint)
        .map(|(_, _, draw)| *draw)
}

/// Whether the sprites draw `codepoint` (Ghostty's `hasCodepoint`).
#[must_use]
pub fn is_sprite(codepoint: u32) -> bool {
    draw_fn(codepoint).is_some()
}

/// The sprite of a cell's text: its one code point, when the sprites draw it.
#[must_use]
pub fn of_text(text: &str) -> Option<u32> {
    let mut chars = text.chars();
    let first = u32::from(chars.next()?);
    (chars.next().is_none() && is_sprite(first)).then_some(first)
}

/// Rasterizes `codepoint` for cells of `metrics`, `cells` wide (2 for a wide character: the canvas doubles, the
/// metrics stay one cell's, as in Ghostty's `renderGlyph`). `None` when the sprites do not draw it.
#[must_use]
pub fn rasterize(codepoint: u32, metrics: &CellMetrics, cells: u32) -> Option<Mask> {
    let draw = draw_fn(codepoint)?;
    let mut canvas = Canvas::new(metrics.cell_width * cells.max(1), metrics.cell_height);
    draw(codepoint, &mut canvas, metrics);
    Some(canvas.into_mask())
}

/// What the element paints for a sprite: its mask and the id the GPUI atlas keeps it under.
#[derive(Debug)]
pub struct Sprite {
    pub id: u64,
    pub mask: Mask,
}

type Key = (u32, u32, CellMetrics);

/// The sprites rasterized so far on this thread, by code point, width in cells and metrics, each with an atlas id of
/// its own. It grows as GPUI's glyph atlas does, one entry per sprite drawn per cell size, and never forgets one: the
/// atlas keeps every tile under its id, so an entry dropped here would strand its tile and upload the mask again.
#[derive(Default)]
struct Cache {
    sprites: HashMap<Key, Rc<Sprite>>,
    next_id: u64,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// The sprite of `codepoint` for cells of `metrics`, `cells` wide, rasterized once per thread. `None` when the sprites
/// do not draw it or it covers nothing.
#[must_use]
pub fn sprite(codepoint: u32, metrics: &CellMetrics, cells: u32) -> Option<Rc<Sprite>> {
    let key = (codepoint, cells, *metrics);
    CACHE.with_borrow_mut(|cache| {
        if let Some(sprite) = cache.sprites.get(&key) {
            return Some(Rc::clone(sprite));
        }
        let mask = rasterize(codepoint, metrics, cells)?;
        if mask.width == 0 || mask.height == 0 {
            return None;
        }
        cache.next_id += 1;
        let sprite = Rc::new(Sprite {
            id: cache.next_id,
            mask,
        });
        cache.sprites.insert(key, Rc::clone(&sprite));
        Some(sprite)
    })
}
