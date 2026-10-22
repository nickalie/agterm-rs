//! The split's plan and geometry (ported from the GTK build's `split.rs` tests), the page's places and the divider's
//! drag.

use gpui_kit::{point, px};
use ngterm_core::model::{SessionSeed, SplitLayout};
use ngterm_core::zoom::ZoomSurface;
use ngterm_host::fakes::FakeSurface;
use uuid::Uuid;

use super::*;

/// A session whose primary pane is `P`, with a split pane `S` and a scratch `X` installed as asked.
pub(crate) fn session(split: Option<SplitLayout>, scratch: bool) -> Session {
    let mut session = Session::new(SessionSeed {
        initial_cwd: "/home/u".to_owned(),
        ..SessionSeed::default()
    });
    session.surface.replace(Box::new(FakeSurface::new("P")));
    if let Some(layout) = split {
        session.split = layout;
        session.split_pane_identity = Some(Uuid::new_v4());
        session.split_surface.replace(Box::new(FakeSurface::new("S")));
    }
    if scratch {
        session.scratch_active = true;
        session.scratch_surface.replace(Box::new(FakeSurface::new("X")));
    }
    session
}

pub(crate) fn split(shown: bool, focused: bool) -> SplitLayout {
    SplitLayout {
        shown,
        exists: true,
        focused,
        ..SplitLayout::default()
    }
}

fn zoom(session: &Session, surface: ZoomSurface) -> SurfaceId {
    SurfaceId {
        session: session.id(),
        surface,
    }
}

#[test]
fn a_lone_pane_fills_the_page_and_takes_the_keyboard() {
    let plan = PanePlan::layout(&session(None, false), None);
    assert_eq!(plan.panes, Panes::Alone(Slot::Primary));
    assert!(plan.shows(Slot::Primary) && !plan.shows(Slot::Split) && !plan.shows(Slot::Scratch));
    assert_eq!(plan.focus_token(), Some("P"));
    assert!(!plan.shows_divider());
    assert!((plan.ratio - SPLIT_RATIO_DEFAULT).abs() < f64::EPSILON);
}

#[test]
fn a_shown_split_lays_both_panes_out_and_focuses_the_focused_one() {
    let mut shown = session(Some(split(true, true)), false);
    shown.split.axis = SplitAxis::TopBottom;
    shown.split.ratio = Some(0.3);
    let plan = PanePlan::layout(&shown, None);
    assert_eq!(plan.panes, Panes::Both);
    assert!(plan.shows(Slot::Primary) && plan.shows(Slot::Split) && plan.shows_divider());
    assert_eq!(plan.focus_token(), Some("S"));
    assert_eq!(plan.axis, SplitAxis::TopBottom);
    assert!((plan.ratio - 0.3).abs() < f64::EPSILON);
    shown.split.focused = false;
    assert_eq!(PanePlan::layout(&shown, None).focus_token(), Some("P"));
}

#[test]
fn a_hidden_split_shows_only_the_focused_pane_maximized() {
    let plan = PanePlan::layout(&session(Some(split(false, false)), false), None);
    assert_eq!(plan.panes, Panes::Alone(Slot::Primary));
    assert!(plan.shows(Slot::Primary) && !plan.shows(Slot::Split) && !plan.shows_divider());
    assert_eq!(plan.focus_token(), Some("P"));

    let plan = PanePlan::layout(&session(Some(split(false, true)), false), None);
    assert_eq!(plan.panes, Panes::Alone(Slot::Split));
    assert!(plan.shows(Slot::Split) && !plan.shows(Slot::Primary));
    assert_eq!(
        plan.focus_token(),
        Some("S"),
        "the right pane shows and keeps the keyboard"
    );
}

#[test]
fn a_swap_moves_the_tokens_between_positions() {
    let mut swapped = session(Some(split(true, true)), false);
    std::mem::swap(&mut swapped.surface, &mut swapped.split_surface);
    swapped.split.focused = false;
    let plan = PanePlan::layout(&swapped, None);
    assert_eq!(plan.token(Slot::Primary), Some("S"));
    assert_eq!(plan.token(Slot::Split), Some("P"));
    assert_eq!(plan.slot_of("S"), Some(Slot::Primary));
    assert_eq!(plan.focus_token(), Some("S"), "focus followed the terminal");
}

#[test]
fn the_scratch_covers_the_panes_and_takes_the_keyboard() {
    let plan = PanePlan::layout(&session(Some(split(true, true)), true), None);
    assert!(plan.scratch_covers && plan.shows(Slot::Scratch));
    assert!(!plan.shows(Slot::Primary) && !plan.shows(Slot::Split) && !plan.shows_divider());
    assert_eq!(plan.panes, Panes::Both, "the panes keep their layout under it");
    assert_eq!(plan.focus_token(), Some("X"));

    let mut hidden = session(None, true);
    hidden.scratch_active = false;
    let plan = PanePlan::layout(&hidden, None);
    assert!(!plan.shows(Slot::Scratch));
    assert_eq!(plan.focus_token(), Some("P"));
}

#[test]
fn a_zoomed_pane_shows_alone() {
    let full = session(Some(split(true, false)), true);
    let plan = PanePlan::layout(&full, Some(zoom(&full, ZoomSurface::Split)));
    assert_eq!(plan.panes, Panes::Alone(Slot::Split));
    assert!(plan.shows(Slot::Split) && !plan.shows(Slot::Primary) && !plan.shows(Slot::Scratch));
    assert_eq!(plan.focus_token(), Some("S"));

    let plan = PanePlan::layout(&full, Some(zoom(&full, ZoomSurface::Primary)));
    assert!(plan.shows(Slot::Primary) && !plan.shows(Slot::Split) && !plan.shows(Slot::Scratch));
    assert_eq!(plan.focus_token(), Some("P"));

    let mut covered = session(Some(split(true, false)), true);
    covered.scratch_active = false;
    let plan = PanePlan::layout(&covered, Some(zoom(&covered, ZoomSurface::Scratch)));
    assert!(plan.shows(Slot::Scratch) && !plan.shows(Slot::Primary) && !plan.shows(Slot::Split));
    assert_eq!(plan.focus_token(), Some("X"));
}

#[test]
fn the_mute_washes_the_unfocused_pane_of_a_shown_uncovered_split() {
    let mut shown = session(Some(split(true, true)), false);
    assert_eq!(PanePlan::layout(&shown, None).muted(), Some(Slot::Primary));
    shown.split.focused = false;
    assert_eq!(PanePlan::layout(&shown, None).muted(), Some(Slot::Split));
    let mut swapped = session(Some(split(true, true)), false);
    std::mem::swap(&mut swapped.surface, &mut swapped.split_surface);
    swapped.split.focused = false;
    assert_eq!(
        PanePlan::layout(&swapped, None).muted(),
        Some(Slot::Split),
        "the wash marks a position, wherever the terminals went"
    );

    let hidden = PanePlan::layout(&session(Some(split(false, true)), false), None);
    assert_eq!(hidden.muted(), None, "a hidden split shows one pane");
    assert_eq!(PanePlan::layout(&session(None, false), None).muted(), None);
    let scratch = PanePlan::layout(&session(Some(split(true, true)), true), None);
    assert_eq!(scratch.muted(), None, "the scratch covers both panes");
    let zoomed = session(Some(split(true, true)), false);
    let plan = PanePlan::layout(&zoomed, Some(zoom(&zoomed, ZoomSurface::Primary)));
    assert_eq!(plan.muted(), None, "a zoomed pane shows alone");
}

#[test]
fn a_floating_program_washes_the_backdrop_in_place_of_the_pane() {
    let mut floating = session(Some(split(true, true)), false);
    program(&mut floating, Some(60));
    let plan = PanePlan::layout(&floating, None);
    assert!(plan.backdrop());
    assert_eq!(plan.muted(), None, "the backdrop mutes both panes already");

    let mut covered = session(Some(split(true, true)), false);
    program(&mut covered, None);
    let plan = PanePlan::layout(&covered, None);
    assert!(!plan.backdrop(), "a covering program is chromeless");
    assert_eq!(plan.muted(), None, "it hides the panes");

    let mut overlaid = session(Some(split(true, true)), false);
    pane_overlay(&mut overlaid, OverlayPane::Left, 1);
    let plan = PanePlan::layout(&overlaid, None);
    assert!(!plan.backdrop());
    assert_eq!(
        plan.muted(),
        Some(Slot::Primary),
        "an overlay on the unfocused pane is muted with it"
    );
    assert_eq!(plan.overlays.pane(Slot::Primary), Some(1));
}

/// Opens a program in `session`'s slot at `size_percent`.
pub(crate) fn program(session: &mut Session, size_percent: Option<i64>) {
    let slot = &mut session.overlay;
    slot.active = true;
    slot.generation += 1;
    slot.command = Some("exec cat".to_owned());
    slot.size_percent = size_percent;
}

/// Opens an overlay over `pane` with `generation`.
pub(crate) fn pane_overlay(session: &mut Session, pane: OverlayPane, generation: u64) {
    session.pane_overlays.slot_mut(pane).overlay = Some(ngterm_core::model::PaneOverlay {
        command: "exec cat".to_owned(),
        cwd: None,
        background_color: None,
        wait: false,
        generation,
        code_file: None,
        html: None,
    });
}

#[test]
fn a_covering_program_hides_the_panes_and_takes_the_keyboard() {
    let mut covered = session(Some(split(true, false)), true);
    program(&mut covered, None);
    let plan = PanePlan::layout(&covered, None);
    assert!(plan.overlays.covers);
    assert_eq!(
        plan.overlays.program,
        Some(ProgramShow {
            generation: 1,
            fraction: None
        })
    );
    assert!(!plan.shows(Slot::Primary) && !plan.shows(Slot::Split) && !plan.shows(Slot::Scratch));
    assert!(!plan.shows_divider());
    assert_eq!(plan.overlays.focus, Some(OverlayFocus::Program));
}

#[test]
fn a_page_takes_the_slot_as_a_program_does() {
    use ngterm_core::html::{HtmlOverlay, HtmlPageFlags, HtmlSource};
    let page = HtmlOverlay::new(
        HtmlSource::File {
            path: "/site/index.html".to_owned(),
            grant_root: None,
        },
        HtmlPageFlags::default(),
    );
    for (size_percent, fraction) in [(None, None), (Some(50), Some(0.5))] {
        let mut covered = session(Some(split(true, false)), false);
        let slot = &mut covered.overlay;
        slot.active = true;
        slot.generation = 4;
        slot.size_percent = size_percent;
        slot.html = Some(page.clone());
        let plan = PanePlan::layout(&covered, None);
        assert_eq!(
            plan.overlays.program,
            Some(ProgramShow {
                generation: 4,
                fraction
            })
        );
        assert_eq!(plan.overlays.covers, fraction.is_none());
        assert_eq!(plan.overlays.focus, Some(OverlayFocus::Program));
    }
}

#[test]
fn a_floating_program_leaves_the_panes_lit_but_holds_the_keyboard() {
    let mut floating = session(Some(split(true, false)), false);
    program(&mut floating, Some(40));
    pane_overlay(&mut floating, OverlayPane::Right, 3);
    let plan = PanePlan::layout(&floating, None);
    assert!(!plan.overlays.covers && plan.shows(Slot::Primary) && plan.shows_divider());
    assert_eq!(plan.overlays.program.and_then(|program| program.fraction), Some(0.4));
    assert_eq!(plan.overlays.focus, Some(OverlayFocus::Program));
    assert_eq!(
        plan.overlays.pane(Slot::Split),
        Some(3),
        "a pane overlay shows beside it"
    );
}

#[test]
fn a_pane_overlay_covers_its_laid_out_pane_and_takes_its_focus() {
    let mut both = session(Some(split(true, true)), false);
    pane_overlay(&mut both, OverlayPane::Right, 1);
    let plan = PanePlan::layout(&both, None);
    assert_eq!((plan.overlays.primary, plan.overlays.split), (None, Some(1)));
    assert_eq!(plan.overlays.focus, Some(OverlayFocus::Pane(Slot::Split)));
    both.split.focused = false;
    assert_eq!(
        PanePlan::layout(&both, None).overlays.focus,
        None,
        "the bare pane has focus"
    );

    let mut hidden = session(Some(split(false, false)), false);
    pane_overlay(&mut hidden, OverlayPane::Right, 2);
    assert_eq!(
        PanePlan::layout(&hidden, None).overlays.split,
        None,
        "its pane is not laid out"
    );
    let mut scratch = session(Some(split(true, true)), true);
    pane_overlay(&mut scratch, OverlayPane::Right, 2);
    let plan = PanePlan::layout(&scratch, None);
    assert_eq!(
        (plan.overlays.split, plan.overlays.focus),
        (None, None),
        "the scratch covers it"
    );
}

#[test]
fn a_zoomed_overlay_fills_the_page() {
    let mut full = session(Some(split(true, false)), false);
    program(&mut full, Some(30));
    let plan = PanePlan::layout(&full, Some(zoom(&full, ZoomSurface::Overlay)));
    assert!(plan.overlays.covers);
    assert_eq!(plan.overlays.program.and_then(|program| program.fraction), None);

    let mut panes = session(Some(split(true, false)), false);
    pane_overlay(&mut panes, OverlayPane::Right, 4);
    let plan = PanePlan::layout(&panes, Some(zoom(&panes, ZoomSurface::OverlayRight)));
    assert_eq!(plan.panes, Panes::Alone(Slot::Split));
    assert_eq!(plan.overlays.focus, Some(OverlayFocus::Pane(Slot::Split)));
    let plan = PanePlan::layout(&panes, Some(zoom(&panes, ZoomSurface::Split)));
    assert_eq!(plan.overlays, OverlayPlan::default(), "a zoomed terminal shows bare");
}

/// Upstream's zoom of the pane under a running overlay (3d9d561): the zoomed terminal shows bare while the overlay
/// keeps its slot, and leaving zoom covers the session again.
#[test]
fn a_pane_zoomed_under_a_session_wide_overlay_shows_bare() {
    let mut covered = session(Some(split(true, false)), false);
    program(&mut covered, None);
    let plan = PanePlan::layout(&covered, Some(zoom(&covered, ZoomSurface::Primary)));
    assert_eq!(plan.overlays, OverlayPlan::default(), "the zoomed terminal shows bare");
    assert_eq!(plan.panes, Panes::Alone(Slot::Primary));
    assert!(plan.shows(Slot::Primary) && !plan.shows(Slot::Split));
    assert_eq!(plan.focus_token(), Some("P"));

    let plan = PanePlan::layout(&covered, None);
    assert!(plan.overlays.covers, "unzoomed, the overlay covers the session again");
    assert_eq!(plan.overlays.focus, Some(OverlayFocus::Program));
    assert!(!plan.shows(Slot::Primary) && !plan.shows(Slot::Split));
}

#[test]
fn a_floating_panel_is_centered_at_its_share() {
    assert_eq!(
        program_rect(Some(0.5), 1000, 600),
        Rect {
            x: 250,
            y: 150,
            width: 500,
            height: 300
        }
    );
    assert_eq!(program_rect(None, 10, 20), Rect::full(10, 20));
    assert_eq!(
        program_rect(Some(1.5), 800, 600),
        Rect::full(800, 600),
        "a share past the page is the page"
    );
}

#[test]
fn another_sessions_zoom_or_an_overlay_kind_changes_nothing() {
    let full = session(Some(split(true, false)), false);
    let other = Some(SurfaceId {
        session: Uuid::new_v4(),
        surface: ZoomSurface::Split,
    });
    assert_eq!(PanePlan::layout(&full, other), PanePlan::layout(&full, None));
    assert_eq!(
        PanePlan::layout(&full, Some(zoom(&full, ZoomSurface::Overlay))),
        PanePlan::layout(&full, None)
    );
}

#[test]
fn the_split_terminal_is_created_once_laid_out() {
    let mut fresh = session(None, false);
    assert_eq!(needs(&fresh, None), Needs::default());
    fresh.split = split(true, true);
    fresh.split_pane_identity = Some(Uuid::new_v4());
    assert!(needs(&fresh, None).split);
    fresh.split_surface.replace(Box::new(FakeSurface::new("S")));
    assert!(!needs(&fresh, None).split, "it has one");

    let mut restored = session(None, false);
    restored.split = split(false, false);
    restored.split_pane_identity = Some(Uuid::new_v4());
    assert!(!needs(&restored, None).split, "a hidden split's unfocused pane waits");
    restored.split.focused = true;
    assert!(
        !needs(&restored, None).split,
        "focused but never laid out: the primary shows"
    );
    assert!(
        needs(&restored, Some(zoom(&restored, ZoomSurface::Split))).split,
        "a zoom lays it out"
    );
    restored.split.shown = true;
    assert!(needs(&restored, None).split);
}

#[test]
fn the_scratch_terminal_is_created_while_shown_without_one() {
    let mut scratch = session(None, false);
    scratch.scratch_active = true;
    assert!(needs(&scratch, None).scratch);
    scratch.scratch_surface.replace(Box::new(FakeSurface::new("X")));
    assert!(!needs(&scratch, None).scratch);
}

#[test]
fn a_lone_pane_and_the_one_under_it_fill_the_page() {
    let geometry = geometry(Panes::Alone(Slot::Split), SplitAxis::LeftRight, 0.3, 800, 600);
    assert_eq!(geometry.primary, Rect::full(800, 600));
    assert_eq!(geometry.split, Rect::full(800, 600));
    assert_eq!(geometry.divider, None);
}

#[test]
fn both_panes_split_the_axis_at_the_ratio() {
    let geometry = geometry(Panes::Both, SplitAxis::LeftRight, 0.5, 801, 600);
    assert_eq!(
        geometry.primary,
        Rect {
            x: 0,
            y: 0,
            width: 400,
            height: 600
        }
    );
    assert_eq!(
        geometry.split,
        Rect {
            x: 401,
            y: 0,
            width: 400,
            height: 600
        }
    );
    let divider = geometry.divider.unwrap();
    assert!(
        divider.x < 400 && divider.x + divider.width > 401,
        "the band straddles the separator"
    );

    let geometry = geometry_top_bottom();
    assert_eq!(
        geometry.primary,
        Rect {
            x: 0,
            y: 0,
            width: 800,
            height: 150
        }
    );
    assert_eq!(
        geometry.split,
        Rect {
            x: 0,
            y: 151,
            width: 800,
            height: 450
        }
    );
    assert_eq!(geometry.divider.unwrap().width, 800);
}

fn geometry_top_bottom() -> Geometry {
    geometry(Panes::Both, SplitAxis::TopBottom, 0.25, 800, 601)
}

#[test]
fn a_degenerate_page_never_allocates_a_negative_size() {
    let geometry = geometry(Panes::Both, SplitAxis::LeftRight, 0.9, 0, 0);
    assert!(geometry.primary.width >= 0 && geometry.split.width >= 0);
}

#[test]
fn a_drag_moves_the_ratio_within_its_bounds() {
    assert!((dragged_ratio(0.5, 100.0, 1001) - 0.6).abs() < 1e-9);
    assert!((dragged_ratio(0.5, -10_000.0, 1001) - 0.05).abs() < 1e-9);
    assert!((dragged_ratio(0.5, 10_000.0, 1001) - 0.95).abs() < 1e-9);
    assert!((dragged_ratio(0.4, 50.0, 1) - 0.4).abs() < f64::EPSILON);
}

#[test]
fn a_pane_under_its_own_overlay_or_off_the_layout_is_not_drawn() {
    let mut both = session(Some(split(true, true)), false);
    let plan = PanePlan::layout(&both, None);
    assert!(plan.draws("P") && plan.draws("S"));
    pane_overlay(&mut both, OverlayPane::Left, 1);
    let plan = PanePlan::layout(&both, None);
    assert!(!plan.draws("P"), "its overlay takes its place");
    assert!(plan.draws("S"));
    let hidden = PanePlan::layout(&session(Some(split(false, false)), false), None);
    assert!(hidden.draws("P") && !hidden.draws("S"));
    assert!(!hidden.draws("unknown"));
}

#[test]
fn every_place_lands_where_the_gtk_stack_allocates() {
    let geometry = geometry(Panes::Both, SplitAxis::LeftRight, 0.5, 801, 600);
    let full = Rect::full(801, 600);
    let rect = |place: Place, panes: Panes| place.rect(panes, &geometry, SplitAxis::LeftRight, 801, 600);
    assert_eq!(rect(Place::Full, Panes::Both), full);
    assert_eq!(rect(Place::Pane(Slot::Primary), Panes::Both), geometry.primary);
    assert_eq!(rect(Place::Pane(Slot::Split), Panes::Both), geometry.split);
    assert_eq!(
        rect(Place::Pane(Slot::Scratch), Panes::Both),
        full,
        "the scratch covers the page"
    );
    assert_eq!(rect(Place::Over(Slot::Split), Panes::Both), geometry.split);
    assert_eq!(
        rect(Place::Over(Slot::Split), Panes::Alone(Slot::Primary)),
        full,
        "a pane not laid out: the whole page"
    );
    assert_eq!(rect(Place::Divider, Panes::Both), geometry.divider.unwrap());
    assert_eq!(
        rect(Place::Separator, Panes::Both),
        Rect {
            x: 400,
            y: 0,
            width: 1,
            height: 600
        }
    );
    let alone = super::geometry(Panes::Alone(Slot::Primary), SplitAxis::LeftRight, 0.5, 801, 600);
    assert!(
        Place::Divider
            .rect(Panes::Alone(Slot::Primary), &alone, SplitAxis::LeftRight, 801, 600)
            .is_empty()
    );
}

#[test]
fn the_separator_lies_across_a_top_bottom_split() {
    let geometry = geometry_top_bottom();
    assert_eq!(
        separator_rect(&geometry, SplitAxis::TopBottom),
        Some(Rect {
            x: 0,
            y: 150,
            width: 800,
            height: 1
        })
    );
}

#[test]
fn a_drag_shows_its_ratio_until_the_model_takes_it() {
    let state = PageState::default();
    state.size.set((1001, 600));
    assert!(!state.dragging());
    state.begin_drag(0.5, point(px(500.), px(10.)));
    assert!(state.dragging());
    assert!(state.drag_to(point(px(600.), px(300.)), SplitAxis::LeftRight));
    assert!(
        (state.ratio(0.5) - 0.6).abs() < 1e-9,
        "the page lays out at the drag's ratio"
    );
    assert!(
        !state.drag_to(point(px(600.), px(0.)), SplitAxis::LeftRight),
        "the same ratio is no change"
    );
    state.settle();
    assert!((state.ratio(0.5) - 0.6).abs() < 1e-9, "a running drag keeps its ratio");
    let ended = state.end_drag().expect("a moved divider persists its ratio");
    assert!((ended - 0.6).abs() < 1e-9);
    assert!((state.ratio(0.5) - 0.6).abs() < 1e-9, "until the model has it");
    state.settle();
    assert!((state.ratio(0.6) - 0.6).abs() < 1e-9);
    assert!((state.ratio(0.4) - 0.4).abs() < 1e-9, "then the model's ratio stands");
}

#[test]
fn a_double_click_on_an_unmoved_divider_resets_it() {
    let state = PageState::default();
    state.size.set((1001, 600));
    state.begin_drag(0.5, point(px(500.), px(0.)));
    assert_eq!(state.end_drag(), None, "a press without a move persists nothing");
    assert!(state.is_reset(2));
    state.begin_drag(0.5, point(px(500.), px(0.)));
    state.drag_to(point(px(520.), px(0.)), SplitAxis::LeftRight);
    assert!(!state.is_reset(2), "a moved divider is re-grabbed, not reset");
    assert!(!state.is_reset(1));
}

#[test]
fn the_page_reports_its_panes_by_role() {
    let geometry = geometry(Panes::Alone(Slot::Split), SplitAxis::LeftRight, 0.5, 400, 300);
    let frames = page_frames(Panes::Alone(Slot::Split), &geometry, 400, 300);
    assert!(frames.left.is_none());
    assert_eq!(frames.right.map(|frame| frame.width), Some(400.0));
    assert_eq!(frames.detail.map(|frame| frame.height), Some(300.0));
    assert!(page_frames(Panes::Both, &geometry, 0, 0).detail.is_none());
}
