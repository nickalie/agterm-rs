//! The inactive-pane and backdrop washes (ported from the GTK build's `mute.rs` tests) and the backdrop's flattening.

use ngterm_core::model::{BackgroundWatermark, OverlayPane, SplitAxis, WatermarkKind};

use super::*;
use crate::split::geometry;
use crate::split::tests::{pane_overlay, program, session, split};

const THEME: Rgb = Rgb::new(0x10, 0x20, 0x30);

fn color_mark(hex: &str) -> BackgroundWatermark {
    BackgroundWatermark {
        kind: WatermarkKind::Color,
        image_path: None,
        text: None,
        color_hex: Some(hex.to_owned()),
        opacity: None,
        fit: None,
        position: None,
        repeats: None,
    }
}

fn muted_of(session: &Session, opacity: f64) -> Option<Muted> {
    muted(session, Some(&PanePlan::layout(session, None)), opacity, THEME)
}

#[test]
fn the_pane_wash_blends_toward_the_background_the_pane_renders() {
    let mut shown = session(Some(split(true, true)), false);
    assert_eq!(
        muted_of(&shown, 0.4),
        Some(Muted {
            slot: Slot::Primary,
            overlay: false,
            color: THEME,
            opacity: 0.4,
        }),
        "the theme background without a solid color"
    );
    shown.background_watermark = Some(color_mark("#201414"));
    assert_eq!(
        muted_of(&shown, 0.4).map(|muted| muted.color),
        Some(Rgb::new(0x20, 0x14, 0x14))
    );
    shown.pane_backgrounds.left = Some(color_mark("#0a0b0c"));
    assert_eq!(
        muted_of(&shown, 0.4).map(|muted| muted.color),
        Some(Rgb::new(0x0a, 0x0b, 0x0c)),
        "a pane override wins over the session default"
    );
    assert_eq!(muted_of(&shown, 0.0), None, "strength 0 renders nothing");
    assert_eq!(
        muted(&shown, None, 0.4, THEME),
        None,
        "nothing off screen or under the dashboard"
    );
}

#[test]
fn the_wash_follows_the_split_focus() {
    let mut shown = session(Some(split(true, true)), false);
    assert_eq!(muted_of(&shown, 0.4).map(|muted| muted.slot), Some(Slot::Primary));
    shown.split.focused = false;
    assert_eq!(
        muted_of(&shown, 0.4).map(|muted| muted.slot),
        Some(Slot::Split),
        "the focus moved: the other pane fades"
    );
    shown.split.shown = false;
    assert_eq!(muted_of(&shown, 0.4), None, "a hidden split shows one pane, unmuted");
}

#[test]
fn an_overlay_wash_blends_toward_the_overlays_own_background() {
    let mut overlaid = session(Some(split(true, true)), false);
    overlaid.pane_backgrounds.left = Some(color_mark("#201414"));
    pane_overlay(&mut overlaid, OverlayPane::Left, 1);
    let wash = muted_of(&overlaid, 0.4).expect("a wash");
    assert!(wash.overlay && wash.slot == Slot::Primary);
    assert_eq!(wash.color, THEME, "the overlay never inherits the pane's color");
    let set = |hex: &str, session: &mut Session| {
        if let Some(overlay) = session.pane_overlays.slot_mut(OverlayPane::Left).overlay.as_mut() {
            overlay.background_color = Some(hex.to_owned());
        }
    };
    set("#0a0b0c", &mut overlaid);
    assert_eq!(
        muted_of(&overlaid, 0.4).map(|muted| muted.color),
        Some(Rgb::new(0x0a, 0x0b, 0x0c))
    );
    set("teal", &mut overlaid);
    assert_eq!(
        muted_of(&overlaid, 0.4).map(|muted| muted.color),
        Some(THEME),
        "a color the renderer refuses leaves the theme"
    );
    let washes = Washes {
        pane: muted_of(&overlaid, 0.4),
        backdrop: None,
    };
    assert_eq!(washes.wash(Slot::Primary, true), Some((THEME, 0.4)));
    assert_eq!(
        washes.wash(Slot::Primary, false),
        None,
        "over the overlay, not the terminal"
    );
}

#[test]
fn the_backdrop_washes_each_laid_out_pane_over_the_session_default() {
    let mut floating = session(Some(split(true, true)), false);
    floating.background_watermark = Some(color_mark("#201414"));
    floating.pane_backgrounds.right = Some(color_mark("#0a0b0c"));
    assert_eq!(
        backdrop(&floating, Some(&PanePlan::layout(&floating, None)), THEME),
        None,
        "no program, no backdrop"
    );
    program(&mut floating, Some(60));
    let plan = PanePlan::layout(&floating, None);
    assert_eq!(
        backdrop(&floating, Some(&plan), THEME),
        Some(vec![
            (None, Rgb::new(0x20, 0x14, 0x14)),
            (Some(Slot::Primary), Rgb::new(0x20, 0x14, 0x14)),
            (Some(Slot::Split), Rgb::new(0x0a, 0x0b, 0x0c)),
        ])
    );
    assert_eq!(backdrop(&floating, None, THEME), None, "nothing off screen");
}

/// The color covering (`x`, `y`) in `flat`, which must cover every pixel at most once.
fn at(flat: &[(Rect, Rgb)], x: i32, y: i32) -> Option<Rgb> {
    let covering: Vec<Rgb> = flat
        .iter()
        .filter(|(rect, _)| x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height)
        .map(|(_, color)| *color)
        .collect();
    assert!(covering.len() <= 1, "({x}, {y}) is painted {} times", covering.len());
    covering.first().copied()
}

#[test]
fn a_flattened_backdrop_paints_each_pixel_once_in_the_color_on_top() {
    let (back, left, right) = (Rgb::new(1, 1, 1), Rgb::new(2, 2, 2), Rgb::new(3, 3, 3));
    let regions = vec![(None, back), (Some(Slot::Primary), left), (Some(Slot::Split), right)];
    let laid = geometry(Panes::Both, SplitAxis::LeftRight, 0.5, 801, 60);
    let flat = flatten(&regions, Panes::Both, &laid, 801, 60);
    for y in [0, 30, 59] {
        assert_eq!(at(&flat, 0, y), Some(left));
        assert_eq!(at(&flat, 399, y), Some(left));
        assert_eq!(at(&flat, 400, y), Some(back), "the separator keeps the session default");
        assert_eq!(at(&flat, 401, y), Some(right));
        assert_eq!(at(&flat, 800, y), Some(right));
    }
    let alone = geometry(Panes::Alone(Slot::Primary), SplitAxis::LeftRight, 0.5, 80, 60);
    let flat = flatten(&regions, Panes::Alone(Slot::Primary), &alone, 80, 60);
    assert_eq!(at(&flat, 40, 30), Some(left), "a pane not laid out is skipped");
    assert_eq!(flat.len(), 1, "the default lies wholly under the lone pane");
}

#[test]
fn subtracting_a_rectangle_leaves_disjoint_pieces_around_it() {
    let page = Rect::full(10, 10);
    let hole = Rect {
        x: 3,
        y: 3,
        width: 4,
        height: 4,
    };
    let pieces = subtract(page, hole);
    let area: i32 = pieces.iter().map(|piece| piece.width * piece.height).sum();
    assert_eq!(area, 100 - 16);
    let flat: Vec<(Rect, Rgb)> = pieces.into_iter().map(|piece| (piece, THEME)).collect();
    assert_eq!(at(&flat, 5, 5), None, "the hole stays a hole");
    assert_eq!(at(&flat, 0, 0), Some(THEME));
    assert_eq!(
        subtract(page, Rect { x: 20, ..hole }),
        vec![page],
        "a cut outside leaves it whole"
    );
}
