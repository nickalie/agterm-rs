//! The panel's placement, config, painter and link modifier, ported from the GTK build's `hud.rs` tests. The panel on
//! its page (mounted, passive, replaced, closed, its Ctrl-click) is the deck's tests (`deck/tests.rs`).

use agterm_core::hud::HudSpec;
use agterm_core::model::HudState;

use super::*;

fn style(position: HudPosition, sticky: bool) -> PanelStyle {
    PanelStyle {
        width_fraction: 0.5,
        height_fraction: 0.1,
        height_points: Some(60.0),
        edge_margin: if sticky { 0.0 } else { 0.1 },
        position,
        frame: true,
    }
}

fn pane() -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: 1000,
        height: 600,
    }
}

#[test]
fn a_centered_panel_sits_in_the_middle() {
    let frame = style(HudPosition::Center, false).panel_frame(pane());
    assert_eq!(
        frame,
        Rect {
            x: 250,
            y: 270,
            width: 500,
            height: 60
        }
    );
}

#[test]
fn an_anchored_panel_holds_the_edge_margin_unless_sticky() {
    let frame = style(HudPosition::TopLeft, false).panel_frame(pane());
    // across: (1000 * (0.25 - 0.1)) = 150 off center; down: 600 * ((1 - 0.1) / 2 - 0.1) = 210 up
    assert_eq!((frame.x, frame.y), (100, 60));
    let sticky = style(HudPosition::BottomRight, true).panel_frame(pane());
    assert_eq!(
        (sticky.x + sticky.width, sticky.y + sticky.height),
        (1000, 600),
        "flush with the corner"
    );
}

#[test]
fn a_scoped_panel_is_placed_in_its_pane() {
    let right = Rect {
        x: 501,
        y: 0,
        width: 499,
        height: 600,
    };
    let frame = style(HudPosition::Center, false).panel_frame(right);
    assert!(frame.x > right.x && frame.x + frame.width < right.x + right.width);
}

#[test]
fn the_style_follows_the_slot() {
    let mut slot = OverlaySlot {
        active: true,
        size_percent: Some(40),
        ..OverlaySlot::default()
    };
    assert_eq!(PanelStyle::of(&slot), None, "no hud");
    let mut spec = HudSpec::new("m");
    spec.sticky = true;
    spec.frame = false;
    spec.position = HudPosition::BottomCenter;
    slot.hud = Some(HudState {
        spec,
        pane_identity: None,
        file: PathBuf::from("/tmp/x"),
        height_percent: 12,
        height_points: Some(48.0),
        font_size: None,
        resized_width_percent: None,
    });
    let style = PanelStyle::of(&slot).unwrap();
    assert!((style.width_fraction - 0.4).abs() < 1e-9);
    assert_eq!(style.height_points, Some(48.0));
    assert!(style.edge_margin.abs() < f64::EPSILON && !style.frame);
    assert_eq!(style.position, HudPosition::BottomCenter);
}

/// The pane upstream's sticky cases lay a caption out in.
const CAPTION_PANE: Rect = Rect {
    x: 200,
    y: 100,
    width: 1000,
    height: 800,
};

/// A 48-point caption's style at `position` and `size_percent` (upstream's `captionSession`).
fn caption(position: HudPosition, size_percent: i64, sticky: bool, frame: bool) -> PanelStyle {
    let mut spec = HudSpec::new("caption");
    spec.position = position;
    spec.sticky = sticky;
    spec.frame = frame;
    let slot = OverlaySlot {
        active: true,
        size_percent: Some(size_percent),
        hud: Some(HudState {
            spec,
            pane_identity: None,
            file: PathBuf::from("/tmp/x"),
            height_percent: 10,
            height_points: Some(48.0),
            font_size: None,
            resized_width_percent: None,
        }),
        ..OverlaySlot::default()
    };
    PanelStyle::of(&slot).unwrap()
}

#[test]
fn a_sticky_top_panel_sits_flush_and_spans_the_pane() {
    let panel = caption(HudPosition::TopCenter, 100, true, false).panel_frame(CAPTION_PANE);
    assert_eq!(
        panel,
        Rect {
            x: 200,
            y: 100,
            width: 1000,
            height: 48
        }
    );
}

#[test]
fn a_sticky_bottom_panel_sits_flush_at_the_bottom() {
    let panel = caption(HudPosition::BottomCenter, 100, true, false).panel_frame(CAPTION_PANE);
    assert_eq!(panel.y + panel.height, CAPTION_PANE.y + CAPTION_PANE.height);
    assert_eq!(panel.height, 48);
}

#[test]
fn a_sticky_corner_panel_touches_both_edges() {
    let panel = caption(HudPosition::BottomRight, 40, true, false).panel_frame(CAPTION_PANE);
    assert_eq!(panel.x + panel.width, CAPTION_PANE.x + CAPTION_PANE.width);
    assert_eq!(panel.y + panel.height, CAPTION_PANE.y + CAPTION_PANE.height);
    assert_eq!(panel.width, 400);
}

#[test]
fn a_sticky_side_panel_touches_its_edge_and_stays_vertically_centered() {
    let panel = caption(HudPosition::CenterLeft, 40, true, false).panel_frame(CAPTION_PANE);
    assert_eq!(panel.x, CAPTION_PANE.x);
    assert_eq!(
        2 * panel.y + panel.height,
        2 * CAPTION_PANE.y + CAPTION_PANE.height,
        "the same middle"
    );
}

#[test]
fn sticky_changes_nothing_at_center() {
    let sticky = caption(HudPosition::Center, 40, true, false);
    let loose = caption(HudPosition::Center, 40, false, false);
    assert_eq!(sticky.panel_frame(CAPTION_PANE), loose.panel_frame(CAPTION_PANE));
}

#[test]
fn a_panel_that_is_not_sticky_keeps_its_edge_margin() {
    let panel = caption(HudPosition::TopLeft, 40, false, false).panel_frame(CAPTION_PANE);
    assert_eq!((panel.x, panel.y), (CAPTION_PANE.x + 100, CAPTION_PANE.y + 80));
}

#[test]
fn the_height_in_points_wins_over_the_fraction_whatever_the_pane_height() {
    let style = caption(HudPosition::Center, 40, false, true);
    assert!(style.height_fraction > 0.0, "a fraction is there to lose to");
    for height in [500, 1000] {
        let pane = Rect { height, ..CAPTION_PANE };
        assert_eq!(style.panel_frame(pane).height, 48, "{height}");
    }
}

#[test]
fn the_page_places_a_scoped_panel_in_its_pane_and_nothing_while_it_is_not_laid_out() {
    use crate::split::{Panes, geometry};
    use agterm_core::model::SplitAxis;
    let placement = HudPlacement {
        style: caption(HudPosition::TopCenter, 100, true, false),
        scope: Some(Slot::Split),
    };
    let laid = geometry(Panes::Both, SplitAxis::LeftRight, 0.5, 1000, 600);
    let rect = crate::split::Place::Hud(placement);
    let page = |panes, geometry: &crate::split::Geometry| rect.rect(panes, geometry, SplitAxis::LeftRight, 1000, 600);
    let placed = page(Panes::Both, &laid);
    assert_eq!((placed.x, placed.y, placed.width), (laid.split.x, 0, laid.split.width));
    let alone = geometry(Panes::Alone(Slot::Primary), SplitAxis::LeftRight, 0.5, 1000, 600);
    assert!(
        page(Panes::Alone(Slot::Primary), &alone).is_empty(),
        "its pane is not laid out"
    );
    let whole = crate::split::Place::Hud(HudPlacement {
        scope: None,
        ..placement
    });
    assert_eq!(
        whole.rect(Panes::Both, &laid, SplitAxis::LeftRight, 1000, 600).width,
        1000
    );
}

#[test]
fn the_config_pins_the_padding_and_paints_the_background() {
    let plain = config_overlay(None);
    assert!(plain.contains("window-padding-x = 8\n") && plain.contains("window-padding-y = 6\n"));
    assert!(!plain.contains("background ="));
    let colored = config_overlay(Some("#102030"));
    assert!(colored.contains("background = #102030\n"));
    assert!(colored.contains("window-padding-balance = true"));
}

#[test]
fn only_ctrl_alone_is_the_link_modifier() {
    assert!(is_link_modifier(&Modifiers::control()));
    assert!(!is_link_modifier(&Modifiers::control_shift()));
    assert!(!is_link_modifier(&Modifiers {
        control: true,
        alt: true,
        ..Modifiers::default()
    }));
    assert!(!is_link_modifier(&Modifiers::default()));
}

#[test]
fn the_helper_is_written_once_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = install_helper(dir.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HELPER);
    assert_eq!(install_helper(dir.path()), Some(path));
}

#[gpui_kit::test]
fn the_cell_estimate_measures_the_terminals_face(cx: &mut gpui_kit::TestAppContext) {
    let text = cx.update(|cx| {
        crate::terminal::register_embedded_fonts(cx.text_system());
        Arc::clone(cx.text_system())
    });
    let estimate = cell_estimator(text);
    let (width, height) = estimate(None, 13.0);
    assert!(width >= 1.0 && height > width, "{width}x{height}");
    let (bigger, _) = estimate(None, 26.0);
    assert!(bigger > width, "twice the size is wider: {bigger} > {width}");
}
