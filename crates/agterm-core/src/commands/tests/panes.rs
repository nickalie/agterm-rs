//! The split and scratch commands, with upstream's full error strings, and the frontend's pane hooks.

use agterm_host::fakes::{FakeSurface, FakeSurfaceProbe};
use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Command, EventKind, Request, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Fixture, UNKNOWN, answered_id, args, failure, wire};
use crate::model::{Session, SplitAxis, StatusPane};
use crate::zoom::{SurfaceId, ZoomSurface};
use crate::{Dispatched, PaneReport};

fn ok_id(response: &Response, id: Uuid) {
    assert_eq!(answered_id(response), Some(id), "{response:?}");
}

fn session(fixture: &Fixture, id: Uuid) -> &Session {
    fixture.store().session(id).expect("the session is in the tree")
}

fn install(fixture: &mut Fixture, id: Uuid, pane: StatusPane, token: &str) -> FakeSurfaceProbe {
    let surface = FakeSurface::new(token);
    let probe = surface.probe();
    let session = fixture.store_mut().and_then(|store| store.session_mut(id)).unwrap();
    session.pane_slot_mut(pane).replace(Box::new(surface));
    probe
}

fn split_on(fixture: &mut Fixture, id: Uuid) {
    let response = fixture.session(SessionCmd::Split, Some(&wire(id)), args(|a| a.mode = Some("on".into())));
    ok_id(&response, id);
}

/// A fixture whose first session has a shown split with terminals `L` and `R`.
fn split_fixture() -> (Fixture, Uuid) {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    split_on(&mut fixture, id);
    install(&mut fixture, id, StatusPane::Left, "L");
    install(&mut fixture, id, StatusPane::Right, "R");
    (fixture, id)
}

fn node(fixture: &mut Fixture, id: Uuid) -> Value {
    let response = fixture.send(Command::Tree, None, Args::default());
    let tree = response.result.expect("a tree")["tree"].clone();
    tree["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|workspace| workspace["sessions"].as_array().cloned().unwrap_or_default())
        .find(|node| node["id"] == json!(wire(id)))
        .expect("the session's node")
}

fn surfaces(node: &Value) -> Vec<(String, bool, bool)> {
    node["surfaces"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|surface| {
            (
                surface["kind"].as_str().unwrap_or_default().to_owned(),
                surface["active"] == json!(true),
                surface["visible"] == json!(true),
            )
        })
        .collect()
}

fn pane_events(fixture: &mut Fixture, kind: EventKind) -> Vec<String> {
    let outbox = fixture
        .store_mut()
        .map(crate::model::AppStore::take_outbox)
        .unwrap_or_default();
    outbox
        .events
        .into_iter()
        .filter(|event| event.kind == kind)
        .filter_map(|event| event.payload.status)
        .collect()
}

// MARK: - session.split

#[test]
fn split_shows_hides_and_toggles_against_the_shown_state() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let target = wire(id);
    split_on(&mut fixture, id);
    let node_value = node(&mut fixture, id);
    assert_eq!(node_value["split"], json!(true));
    assert_eq!(node_value["hasSplit"], json!(true));
    assert_eq!(node_value["splitAxis"], json!("vertical"));
    assert_eq!(node_value["splitFocused"], json!(true));
    assert_eq!(node_value["splitCwd"], json!("/home/u"));
    for (mode, shown) in [
        (Some("off"), false),
        (Some("toggle"), true),
        (None, false),
        (None, true),
    ] {
        let response = fixture.session(
            SessionCmd::Split,
            Some(&target),
            args(|a| a.mode = mode.map(Into::into)),
        );
        ok_id(&response, id);
        assert_eq!(session(&fixture, id).split.shown, shown, "{mode:?}");
        assert!(session(&fixture, id).split.exists);
    }
    assert_eq!(
        pane_events(&mut fixture, EventKind::PaneSplit),
        ["shown", "hidden", "shown", "hidden", "shown"]
    );
}

#[test]
fn split_takes_an_axis_and_keeps_it_when_omitted() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let target = wire(id);
    let on = |axis: Option<&str>| {
        args(|a| {
            a.mode = Some("on".into());
            a.axis = axis.map(Into::into);
        })
    };
    assert!(
        fixture
            .session(SessionCmd::Split, Some(&target), on(Some("horizontal")))
            .ok
    );
    assert_eq!(session(&fixture, id).split.axis, SplitAxis::TopBottom);
    assert!(fixture.session(SessionCmd::Split, Some(&target), on(None)).ok);
    assert_eq!(session(&fixture, id).split.axis, SplitAxis::TopBottom);
    assert!(
        fixture
            .session(SessionCmd::Split, Some(&target), on(Some("vertical")))
            .ok
    );
    assert_eq!(session(&fixture, id).split.axis, SplitAxis::LeftRight);
}

#[test]
fn split_refuses_a_bad_mode_or_axis() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let target = wire(id);
    assert_eq!(
        fixture.session(SessionCmd::Split, Some(&target), args(|a| a.mode = Some("show".into()))),
        failure("invalid split mode: show")
    );
    assert_eq!(
        fixture.session(
            SessionCmd::Split,
            Some(&target),
            args(|a| a.axis = Some("diagonal".into()))
        ),
        failure("invalid split axis: diagonal (vertical|horizontal)")
    );
    // the axis is the dispatcher's check, before the target resolves
    assert_eq!(
        fixture.session(
            SessionCmd::Split,
            Some(UNKNOWN),
            args(|a| a.axis = Some("diagonal".into()))
        ),
        failure("invalid split axis: diagonal (vertical|horizontal)")
    );
    assert!(!session(&fixture, id).split.exists);
}

#[test]
fn split_close_tears_the_pane_down_and_is_idempotent() {
    let (mut fixture, id) = split_fixture();
    let target = wire(id);
    fixture.store_mut().map(crate::model::AppStore::take_outbox);
    for _ in 0..2 {
        let response = fixture.session(SessionCmd::SplitClose, Some(&target), Args::default());
        ok_id(&response, id);
        let node_value = node(&mut fixture, id);
        assert_eq!(node_value["split"], json!(false));
        for key in ["hasSplit", "splitAxis", "splitRatio", "splitFocused", "splitCwd"] {
            assert!(node_value.get(key).is_none(), "{key}");
        }
        assert_eq!(surfaces(&node_value), [("left".to_owned(), true, true)]);
    }
    assert_eq!(pane_events(&mut fixture, EventKind::PaneSplit), ["hidden"]);
}

#[test]
fn every_pane_command_names_an_unknown_session() {
    let mut fixture = Fixture::new(0);
    let cases: [(SessionCmd, Args); 7] = [
        (SessionCmd::Split, args(|a| a.mode = Some("on".into()))),
        (SessionCmd::SplitClose, Args::default()),
        (SessionCmd::Swap, Args::default()),
        (SessionCmd::Scratch, args(|a| a.mode = Some("on".into()))),
        (SessionCmd::Focus, args(|a| a.pane = Some("left".into()))),
        (SessionCmd::Resize, args(|a| a.ratio = Some(0.5))),
        (SessionCmd::Lead, Args::default()),
    ];
    for (cmd, request_args) in cases {
        assert_eq!(
            fixture.session(cmd, Some(UNKNOWN), request_args),
            failure("no such session: zzz"),
            "{cmd:?}"
        );
    }
}

// MARK: - session.focus

#[test]
fn focus_names_a_pane_by_role_or_position_or_toggles() {
    let (mut fixture, id) = split_fixture();
    let target = wire(id);
    for (pane, focused) in [
        ("left", false),
        ("right", true),
        ("primary", false),
        ("split", true),
        ("top", false),
        ("bottom", true),
        ("other", false),
        ("other", true),
        ("toggle", false),
    ] {
        let response = fixture.session(SessionCmd::Focus, Some(&target), args(|a| a.pane = Some(pane.into())));
        ok_id(&response, id);
        let node_value = node(&mut fixture, id);
        assert_eq!(node_value["splitFocused"], json!(focused), "{pane}");
        assert_eq!(
            surfaces(&node_value),
            [("left".to_owned(), !focused, true), ("right".to_owned(), focused, true)],
            "{pane}"
        );
    }
    let response = fixture.session(SessionCmd::Focus, Some(&target), Args::default());
    ok_id(&response, id);
    assert!(session(&fixture, id).split.focused, "the default is other");
}

#[test]
fn focus_on_a_hidden_split_holds_and_shows_the_pane_maximized() {
    let (mut fixture, id) = split_fixture();
    let target = wire(id);
    assert!(
        fixture
            .session(SessionCmd::Focus, Some(&target), args(|a| a.pane = Some("left".into())))
            .ok
    );
    assert!(
        fixture
            .session(SessionCmd::Split, Some(&target), args(|a| a.mode = Some("off".into())))
            .ok
    );
    let response = fixture.session(
        SessionCmd::Focus,
        Some(&target),
        args(|a| a.pane = Some("right".into())),
    );
    assert_eq!(response.result, Some(json!({"id": target})));
    // the frontend's focus report about the pane it no longer shows is stale: the split's focus stays
    let _ = fixture.core.pane_focused(id, "L");
    let node_value = node(&mut fixture, id);
    assert_eq!(node_value["split"], json!(false));
    assert_eq!(node_value["splitFocused"], json!(true));
    assert_eq!(
        surfaces(&node_value),
        [("left".to_owned(), false, false), ("right".to_owned(), true, true)]
    );
    // a typed line with no pane named lands in the shown pane
    let typed = fixture.session(SessionCmd::Text, Some(&target), Args::default());
    assert_eq!(typed.result.unwrap()["pane"], json!("right"));
}

#[test]
fn focus_refuses_without_a_split_or_with_an_unknown_pane() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let target = wire(id);
    assert_eq!(
        fixture.session(
            SessionCmd::Focus,
            Some(&target),
            args(|a| a.pane = Some("right".into()))
        ),
        failure("session has no split")
    );
    split_on(&mut fixture, id);
    assert_eq!(
        fixture.session(
            SessionCmd::Focus,
            Some(&target),
            args(|a| a.pane = Some("middle".into()))
        ),
        failure("invalid pane: middle")
    );
}

// MARK: - session.resize

#[test]
fn resize_sets_clamps_and_echoes_the_ratio() {
    let (mut fixture, id) = split_fixture();
    let target = wire(id);
    for (ratio, stored) in [(0.3, 0.3), (0.0, 0.05), (1.5, 0.95), (0.5, 0.5)] {
        let response = fixture.session(SessionCmd::Resize, Some(&target), args(|a| a.ratio = Some(ratio)));
        assert_eq!(response.result, Some(json!({"id": target, "ratio": stored})), "{ratio}");
        assert_eq!(node(&mut fixture, id)["splitRatio"], json!(stored));
    }
}

#[test]
fn resize_nudges_from_an_even_split() {
    let (mut fixture, id) = split_fixture();
    let target = wire(id);
    let response = fixture.session(SessionCmd::Resize, Some(&target), args(|a| a.ratio_delta = Some(0.1)));
    assert_eq!(response.result.unwrap()["ratio"], json!(0.6));
    for (delta, stored) in [(-0.25, 0.35), (-1.0, 0.05), (2.0, 0.95)] {
        let response = fixture.session(SessionCmd::Resize, Some(&target), args(|a| a.ratio_delta = Some(delta)));
        assert!(response.ok);
        assert_eq!(session(&fixture, id).split.ratio, Some(stored), "{delta}");
    }
}

#[test]
fn resize_applies_to_a_hidden_split() {
    let (mut fixture, id) = split_fixture();
    let target = wire(id);
    assert!(
        fixture
            .session(SessionCmd::Split, Some(&target), args(|a| a.mode = Some("off".into())))
            .ok
    );
    assert!(
        fixture
            .session(SessionCmd::Resize, Some(&target), args(|a| a.ratio = Some(0.4)))
            .ok
    );
    assert!(
        fixture
            .session(SessionCmd::Split, Some(&target), args(|a| a.mode = Some("on".into())))
            .ok
    );
    assert_eq!(session(&fixture, id).split.ratio, Some(0.4));
}

#[test]
fn resize_refuses_as_upstream() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let target = wire(id);
    assert_eq!(
        fixture.session(SessionCmd::Resize, Some(&target), Args::default()),
        failure("session.resize requires --split-ratio, --grow-left, or --grow-right")
    );
    let both = args(|a| {
        a.ratio = Some(0.5);
        a.ratio_delta = Some(0.1);
    });
    assert_eq!(
        fixture.session(SessionCmd::Resize, Some(&target), both),
        failure("session.resize: --split-ratio is mutually exclusive with --grow-left/--grow-right")
    );
    assert_eq!(
        fixture.session(SessionCmd::Resize, Some(&target), args(|a| a.ratio = Some(0.3))),
        failure("session has no split")
    );
    assert_eq!(session(&fixture, id).split.ratio, None);
}

// MARK: - session.swap

#[test]
fn swap_exchanges_the_panes_and_focus_follows_the_terminal() {
    let (mut fixture, id) = split_fixture();
    let target = wire(id);
    assert!(
        fixture
            .session(SessionCmd::Resize, Some(&target), args(|a| a.ratio = Some(0.3)))
            .ok
    );
    assert!(
        fixture
            .session(SessionCmd::Focus, Some(&target), args(|a| a.pane = Some("left".into())))
            .ok
    );
    ok_id(&fixture.session(SessionCmd::Swap, Some(&target), Args::default()), id);
    let swapped = session(&fixture, id);
    assert_eq!(swapped.surface.get().map(agterm_host::Surface::pane_token), Some("R"));
    assert_eq!(
        swapped.split_surface.get().map(agterm_host::Surface::pane_token),
        Some("L")
    );
    assert!(swapped.split.focused);
    assert_eq!(swapped.split.ratio, Some(0.3));
    ok_id(&fixture.session(SessionCmd::Swap, Some(&target), Args::default()), id);
    assert_eq!(
        session(&fixture, id)
            .surface
            .get()
            .map(agterm_host::Surface::pane_token),
        Some("L")
    );
}

#[test]
fn a_swap_withdraws_the_sessions_banners_and_keeps_its_badge() {
    let (mut fixture, id) = split_fixture();
    let window = fixture.window;
    let plain = fixture.sessions[1];
    // a background session, its split's terminal notified
    fixture.store_mut().unwrap().select_session(Some(plain));
    assert!(fixture.core.terminal_notification(id, "R", "", "done"));
    let _ = fixture.core.take_notifications();
    ok_id(&fixture.session(SessionCmd::Swap, Some(&wire(id)), Args::default()), id);
    assert_eq!(
        fixture.core.take_notifications(),
        [crate::NotificationEffect::Clear { window, session: id }]
    );
    assert_eq!(session(&fixture, id).unseen_count, 1, "the badge stays");

    assert!(
        !fixture
            .session(SessionCmd::Swap, Some(&wire(plain)), Args::default())
            .ok
    );
    assert!(
        fixture.core.take_notifications().is_empty(),
        "a refused swap withdraws nothing"
    );
}

#[test]
fn swap_reaches_a_hidden_split_and_needs_one() {
    let (mut fixture, id) = split_fixture();
    let target = wire(id);
    assert!(
        fixture
            .session(SessionCmd::Split, Some(&target), args(|a| a.mode = Some("off".into())))
            .ok
    );
    assert!(fixture.session(SessionCmd::Swap, Some(&target), Args::default()).ok);
    assert_eq!(
        session(&fixture, id)
            .surface
            .get()
            .map(agterm_host::Surface::pane_token),
        Some("R")
    );

    let plain = fixture.sessions[1];
    assert_eq!(
        fixture.session(SessionCmd::Swap, Some(&wire(plain)), Args::default()),
        failure("session has no split pane")
    );
}

#[test]
fn swap_waits_for_a_new_splits_terminal_then_gives_up() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    split_on(&mut fixture, id);
    install(&mut fixture, id, StatusPane::Left, "L");
    let request = Request {
        cmd: Command::Session(SessionCmd::Swap),
        target: Some(wire(id)),
        args: None,
    };
    let Dispatched::Pending { resume, .. } = fixture.core.begin(&request) else {
        panic!("a swap with an empty slot waits");
    };
    let next = resume(&mut fixture.core);
    assert!(matches!(next, Dispatched::Pending { .. }));
    install(&mut fixture, id, StatusPane::Right, "R");
    let Dispatched::Pending { resume, .. } = next else {
        unreachable!()
    };
    let Dispatched::Done(response) = resume(&mut fixture.core) else {
        panic!("the swap lands once both terminals are up");
    };
    ok_id(&response, id);

    let other = fixture.sessions[1];
    split_on(&mut fixture, other);
    let request = Request {
        target: Some(wire(other)),
        ..request
    };
    assert_eq!(fixture.core.dispatch(&request), failure("session not realized"));
}

// MARK: - session.scratch

#[test]
fn scratch_shows_and_hides_against_its_state() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let target = wire(id);
    fixture.store_mut().map(crate::model::AppStore::take_outbox);
    for (mode, shown) in [
        (Some("on"), true),
        (Some("on"), true),
        (Some("toggle"), false),
        (None, true),
        (Some("off"), false),
    ] {
        let response = fixture.session(
            SessionCmd::Scratch,
            Some(&target),
            args(|a| a.mode = mode.map(Into::into)),
        );
        ok_id(&response, id);
        assert_eq!(node(&mut fixture, id)["scratch"], json!(shown), "{mode:?}");
    }
    assert_eq!(
        pane_events(&mut fixture, EventKind::PaneScratch),
        ["shown", "hidden", "shown", "hidden"]
    );
    assert_eq!(
        fixture.session(
            SessionCmd::Scratch,
            Some(&target),
            args(|a| a.mode = Some("show".into()))
        ),
        failure("invalid scratch mode: show")
    );
    assert!(!session(&fixture, id).scratch_active);
}

#[test]
fn showing_the_scratch_selects_its_session() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    assert_ne!(fixture.selected(), Some(id));
    let response = fixture.session(
        SessionCmd::Scratch,
        Some(&wire(id)),
        args(|a| a.mode = Some("off".into())),
    );
    assert!(response.ok);
    assert_ne!(fixture.selected(), Some(id), "hiding does not select");
    assert!(
        fixture
            .session(
                SessionCmd::Scratch,
                Some(&wire(id)),
                args(|a| a.mode = Some("on".into()))
            )
            .ok
    );
    assert_eq!(fixture.selected(), Some(id));
}

#[test]
fn a_scratch_command_runs_once_and_respawns_a_live_scratch_silently() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    let target = wire(id);
    let on_with = |command: &str| {
        let command = command.to_owned();
        args(move |a| {
            a.mode = Some("on".into());
            a.command = Some(command);
        })
    };
    assert!(fixture.session(SessionCmd::Scratch, Some(&target), on_with("htop")).ok);
    assert_eq!(session(&fixture, id).scratch_command.as_deref(), Some("htop"));
    let scratch = install(&mut fixture, id, StatusPane::Scratch, "S");
    fixture.store_mut().map(crate::model::AppStore::take_outbox);
    assert!(fixture.session(SessionCmd::Scratch, Some(&target), on_with("top")).ok);
    assert_eq!(scratch.teardowns(), 1, "the live scratch respawns");
    let respawned = session(&fixture, id);
    assert!(respawned.scratch_active && !respawned.scratch_surface.is_present());
    assert_eq!(respawned.scratch_command.as_deref(), Some("top"));
    assert!(
        pane_events(&mut fixture, EventKind::PaneScratch).is_empty(),
        "shown before and after"
    );
    // an empty command is a plain show
    assert!(fixture.session(SessionCmd::Scratch, Some(&target), on_with("")).ok);
    assert_eq!(session(&fixture, id).scratch_command.as_deref(), Some("top"));
}

#[test]
fn the_scratch_is_addressable_once_its_terminal_is_up() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    let target = wire(id);
    install(&mut fixture, id, StatusPane::Left, "L");
    assert!(
        fixture
            .session(SessionCmd::Scratch, Some(&target), args(|a| a.mode = Some("on".into())))
            .ok
    );
    let probe = install(&mut fixture, id, StatusPane::Scratch, "S");
    probe.set_lines(["scratch line"]);
    let response = fixture.session(SessionCmd::Text, Some(&target), Args::default());
    assert_eq!(
        response.result,
        Some(json!({"text": "scratch line", "pane": "scratch"}))
    );
    let typed = fixture.session(
        SessionCmd::Type,
        Some(&target),
        args(|a| {
            a.text = Some("ls".into());
            a.pane = Some("scratch".into());
        }),
    );
    assert_eq!(typed.result, Some(json!({"id": target, "pane": "scratch"})));
    assert_eq!(probe.typed(), ["ls"]);
    let node_value = node(&mut fixture, id);
    assert_eq!(node_value["scratchFontSize"], json!(FakeSurface::FONT_SIZE));
    assert_eq!(
        surfaces(&node_value),
        [("left".to_owned(), false, false), ("scratch".to_owned(), true, true)]
    );
    assert_eq!(node_value["surfaces"][1]["paneID"], json!("S"));
}

// MARK: - session.lead

#[test]
fn lead_has_nothing_to_take_without_live_sessions() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    let target = wire(id);
    let lead = |pane: Option<&str>| args(|a| a.pane = pane.map(Into::into));
    assert_eq!(
        fixture.session(SessionCmd::Lead, Some(&target), lead(Some("scratch"))),
        failure("the scratch terminal has no lead")
    );
    assert_eq!(
        fixture.session(SessionCmd::Lead, Some(&target), lead(Some("middle"))),
        failure("invalid pane: middle")
    );
    assert_eq!(
        fixture.session(SessionCmd::Lead, Some(&target), lead(Some("right"))),
        failure("session has no split pane")
    );
    assert_eq!(
        fixture.session(SessionCmd::Lead, Some(&target), lead(None)),
        failure("session not realized")
    );
    install(&mut fixture, id, StatusPane::Left, &wire(Uuid::new_v4()));
    assert_eq!(
        fixture.session(SessionCmd::Lead, Some(&target), lead(Some("left"))),
        failure("pane has no lead to take")
    );
}

// MARK: - tree read-back

#[test]
fn the_tree_reports_each_panes_font_size_once_its_terminal_is_up() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    let node_value = node(&mut fixture, id);
    for key in ["fontSize", "splitFontSize", "scratchFontSize"] {
        assert!(node_value.get(key).is_none(), "{key}");
    }
    split_on(&mut fixture, id);
    install(&mut fixture, id, StatusPane::Left, "L");
    let probe = install(&mut fixture, id, StatusPane::Right, "R");
    probe.set_realized(false);
    let node_value = node(&mut fixture, id);
    assert_eq!(node_value["fontSize"], json!(FakeSurface::FONT_SIZE));
    assert!(node_value.get("splitFontSize").is_none(), "not up yet");
    probe.set_realized(true);
    assert_eq!(node(&mut fixture, id)["splitFontSize"], json!(FakeSurface::FONT_SIZE));
}

// MARK: - the frontend's pane hooks

#[test]
fn a_panes_exit_routes_by_its_current_role() {
    let (mut fixture, id) = split_fixture();
    assert!(!fixture.core.pane_exited(id, "gone"));
    assert!(
        fixture.core.pane_exited(id, "L"),
        "the primary exits: the split survives"
    );
    assert_eq!(
        session(&fixture, id)
            .surface
            .get()
            .map(agterm_host::Surface::pane_token),
        Some("R")
    );
    assert!(!session(&fixture, id).split.exists);
    assert!(fixture.core.pane_exited(id, "R"), "the last pane exits");
    assert!(fixture.store().session(id).is_none());

    let (mut fixture, id) = split_fixture();
    assert!(fixture.core.pane_exited(id, "R"));
    assert!(!session(&fixture, id).split.exists);
    install(&mut fixture, id, StatusPane::Scratch, "S");
    assert!(fixture.core.pane_exited(id, "S"));
    assert!(!session(&fixture, id).scratch_surface.is_present());
}

#[test]
fn a_click_moves_a_shown_splits_focus() {
    let (mut fixture, id) = split_fixture();
    assert!(fixture.core.pane_focused(id, "L"));
    assert!(!session(&fixture, id).split.focused);
    assert!(
        fixture.core.pane_focused(id, "L"),
        "the same pane again still withdraws the banners"
    );
    assert!(!session(&fixture, id).split.focused);
    assert!(!fixture.core.pane_focused(id, "unknown"));
    install(&mut fixture, id, StatusPane::Scratch, "S");
    assert!(
        !fixture.core.pane_focused(id, "S"),
        "the scratch reports no focus (upstream wires none)"
    );
    assert!(fixture.core.pane_focused(id, "R"));
    assert!(session(&fixture, id).split.focused);
}

#[test]
fn a_zoom_taking_focus_does_not_move_the_splits_focus() {
    let (mut fixture, id) = split_fixture();
    assert!(session(&fixture, id).split.focused, "a new split focuses right");
    let window = fixture.window;
    let zoomed = SurfaceId {
        session: id,
        surface: ZoomSurface::Primary,
    };
    fixture.core.set_zoom_target(window, Some(zoomed));
    assert!(
        fixture.core.pane_focused(id, "L"),
        "the zoom's own grab of the left pane still means the session was seen"
    );
    assert!(
        session(&fixture, id).split.focused,
        "but it is not a click: unzooming returns to the right pane"
    );
    fixture.core.set_zoom_target(window, None);
    assert!(fixture.core.pane_focused(id, "L"));
    assert!(!session(&fixture, id).split.focused);
}

#[test]
fn focus_asks_the_frontend_to_regrab_even_an_unchanged_pane() {
    let (mut fixture, id) = split_fixture();
    let target = wire(id);
    assert!(!fixture.core.pane_focus_requested(id));
    assert!(session(&fixture, id).split.focused);
    let right = fixture.session(
        SessionCmd::Focus,
        Some(&target),
        args(|a| a.pane = Some("right".into())),
    );
    ok_id(&right, id);
    assert!(session(&fixture, id).split.focused, "unchanged");
    assert!(fixture.core.pane_focus_requested(id));
    fixture.core.clear_pane_focus_requests();
    assert!(!fixture.core.pane_focus_requested(id));
    let refused = fixture.session(
        SessionCmd::Focus,
        Some(&target),
        args(|a| a.pane = Some("middle".into())),
    );
    assert_eq!(refused, failure("invalid pane: middle"));
    assert!(!fixture.core.pane_focus_requested(id), "a refusal asks nothing");
}

#[test]
fn pane_reports_land_on_the_reporting_panes_role() {
    let (mut fixture, id) = split_fixture();
    assert!(fixture.core.report_pane(id, "R", &PaneReport::Pwd("/right".into())));
    assert!(
        fixture
            .core
            .report_pane(id, "L", &PaneReport::Title("left title".into()))
    );
    assert!(!fixture.core.report_pane(id, "unknown", &PaneReport::Pwd("/x".into())));
    let reported = session(&fixture, id);
    assert_eq!(reported.split_cwd.as_deref(), Some("/right"));
    assert_eq!(reported.osc_title.as_deref(), Some("left title"));
    let target = wire(id);
    assert!(fixture.session(SessionCmd::Swap, Some(&target), Args::default()).ok);
    assert!(fixture.core.report_pane(id, "R", &PaneReport::Pwd("/now-left".into())));
    assert_eq!(session(&fixture, id).current_cwd.as_deref(), Some("/now-left"));
    install(&mut fixture, id, StatusPane::Scratch, "S");
    assert!(!fixture.core.report_pane(id, "S", &PaneReport::Title("scratch".into())));
}
