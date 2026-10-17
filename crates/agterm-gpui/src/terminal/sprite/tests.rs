use std::rc::Rc;

use super::super::metrics::{CellMetrics, FaceMetrics};
use super::{RANGES, is_sprite, of_text, rasterize, sprite};

/// The `draw<MIN>_<MAX>` / `draw<CP>` functions of Ghostty's `src/font/sprite/draw/*.zig` at `LIBGHOSTTY_VT_REV`,
/// as `grep 'pub fn draw' src/font/sprite/draw/*.zig` lists them.
const GHOSTTY_DRAW_FNS: &[&str] = &[
    "draw1CC1B_1CC1E",
    "draw1CC21_1CC2F",
    "draw1CC30_1CC3F",
    "draw1CD00_1CDE5",
    "draw1CE00",
    "draw1CE01",
    "draw1CE0B",
    "draw1CE0C",
    "draw1CE16_1CE19",
    "draw1CE51_1CE8F",
    "draw1CE90_1CEAF",
    "draw1FB00_1FB3B",
    "draw1FB3C_1FB67",
    "draw1FB68_1FB6F",
    "draw1FB70_1FB75",
    "draw1FB76_1FB7B",
    "draw1FB7C_1FB97",
    "draw1FB98",
    "draw1FB99",
    "draw1FB9A_1FB9F",
    "draw1FBA0_1FBAE",
    "draw1FBAF",
    "draw1FBBD",
    "draw1FBBE",
    "draw1FBBF",
    "draw1FBCE",
    "draw1FBCF",
    "draw1FBD0_1FBDF",
    "draw1FBE0_1FBEF",
    "draw2500_257F",
    "draw2580_259F",
    "draw25E2_25E5",
    "draw25F8_25FA",
    "draw25FF",
    "draw2800_28FF",
    "drawE0B0",
    "drawE0B1",
    "drawE0B2",
    "drawE0B3",
    "drawE0B4",
    "drawE0B5",
    "drawE0B6",
    "drawE0B7",
    "drawE0B8",
    "drawE0B9",
    "drawE0BA",
    "drawE0BB",
    "drawE0BC",
    "drawE0BD",
    "drawE0BE",
    "drawE0BF",
    "drawE0D2",
    "drawE0D4",
    "drawF5D0_F60D",
];

fn ghostty_code_points() -> Vec<u32> {
    let mut points: Vec<u32> = GHOSTTY_DRAW_FNS
        .iter()
        .flat_map(|name| {
            let range = name.trim_start_matches("draw");
            let (min, max) = range.split_once('_').unwrap_or((range, range));
            let parse = |hex: &str| u32::from_str_radix(hex, 16).expect("a hex code point");
            parse(min)..=parse(max)
        })
        .collect();
    points.sort_unstable();
    points
}

fn metrics(width: u32, height: u32, thickness: u32) -> CellMetrics {
    CellMetrics::calc(&FaceMetrics {
        cell_width: f64::from(width),
        ascent: f64::from(height - height / 5),
        descent: -f64::from(height / 5),
        underline_thickness: Some(f64::from(thickness)),
        ..FaceMetrics::default()
    })
}

#[test]
fn the_sprites_draw_exactly_ghosttys_code_points() {
    let ours: Vec<u32> = (0..0x11_0000).filter(|cp| is_sprite(*cp)).collect();
    assert_eq!(ours, ghostty_code_points());
}

#[test]
fn the_ranges_are_sorted_and_disjoint() {
    for pair in RANGES.windows(2) {
        assert!(
            pair[0].0 <= pair[0].1 && pair[0].1 < pair[1].0,
            "{:X?} then {:X?}",
            pair[0].0,
            pair[1].0
        );
    }
}

#[test]
fn a_cell_is_a_sprite_only_for_one_sprite_code_point() {
    assert_eq!(of_text("─"), Some(0x2500));
    assert_eq!(of_text("\u{e0b0}"), Some(0xe0b0));
    assert_eq!(of_text("⣿"), Some(0x28ff));
    assert_eq!(of_text("a"), None);
    assert_eq!(of_text(""), None);
    assert_eq!(of_text("─\u{301}"), None, "a cluster keeps the font");
    assert_eq!(
        of_text("\u{e0c0}"),
        None,
        "the stylized powerline glyphs come from the font"
    );
    assert_eq!(
        of_text("\u{1fb93}"),
        Some(0x1fb93),
        "Ghostty's range covers the unassigned hole"
    );
}

#[test]
fn every_sprite_but_the_hole_covers_pixels_at_small_and_large_cells() {
    for m in [metrics(7, 14, 1), metrics(10, 21, 2), metrics(24, 48, 4)] {
        for cp in ghostty_code_points() {
            let mask = rasterize(cp, &m, 1).expect("a sprite");
            let blank = cp == 0x1fb93 || (cp == 0x2800);
            assert_eq!(mask.alpha.iter().all(|value| *value == 0), blank, "U+{cp:X} at {m:?}");
            assert_eq!(
                mask.alpha.len(),
                usize::try_from(mask.width * mask.height).expect("small")
            );
        }
    }
}

#[test]
fn lines_reach_the_cell_edges_and_stay_in_the_cell() {
    let m = metrics(10, 21, 2);
    let cross = rasterize(0x253c, &m, 1).expect("┼");
    assert_eq!((cross.left, cross.top), (0, 0), "┼ starts at the cell's corner");
    assert_eq!((cross.width, cross.height), (10, 21), "┼ spans the whole cell");
    let full = rasterize(0x2588, &m, 1).expect("█");
    assert!(full.alpha.iter().all(|value| *value == 0xff), "█ covers every pixel");
    assert_eq!((full.width, full.height), (10, 21));
    let light = rasterize(0x2502, &m, 1).expect("│");
    assert_eq!((light.width, light.left), (2, 4), "│ is the box thickness, centered");
    let heavy = rasterize(0x2503, &m, 1).expect("┃");
    assert_eq!((heavy.width, heavy.left), (4, 3), "┃ is twice the box thickness");
}

#[test]
fn a_wide_cell_doubles_the_canvas_for_the_figures_that_follow_it() {
    let m = metrics(10, 20, 1);
    let narrow = rasterize(0xe0b0, &m, 1).expect("");
    let wide = rasterize(0xe0b0, &m, 2).expect("");
    assert_eq!(narrow.width, 10);
    assert_eq!(wide.width, 20, "a powerline arrow fills both cells of a wide character");
    let braille = rasterize(0x28ff, &m, 2).expect("⣿");
    assert!(braille.width > rasterize(0x28ff, &m, 1).expect("⣿").width);
}

#[test]
fn the_cache_rasterizes_once_and_keeps_ids_apart() {
    let small = metrics(9, 17, 1);
    let large = metrics(18, 36, 4);
    let first = sprite(0x2500, &small, 1).expect("─");
    let again = sprite(0x2500, &small, 1).expect("─");
    assert!(Rc::ptr_eq(&first, &again), "one rasterization per key");
    let other_size = sprite(0x2500, &large, 1).expect("─");
    let other_glyph = sprite(0x2502, &small, 1).expect("│");
    let wide = sprite(0x2500, &small, 2).expect("─");
    let ids = [first.id, other_size.id, other_glyph.id, wide.id];
    for (index, id) in ids.iter().enumerate() {
        assert!(!ids[index + 1..].contains(id), "{ids:?}");
    }
    assert!(
        sprite(0x2800, &small, 1).is_none(),
        "an empty braille cell paints nothing"
    );
    assert!(sprite(u32::from('a'), &small, 1).is_none());
}
