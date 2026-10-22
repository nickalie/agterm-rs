//! `dashboard` (upstream's `ControlDispatcherDashboardTests` and `ControlServer.setDashboard`), its `tree` read-back,
//! and the frontend's actions on the grid.

use ngterm_host::fakes::FakeSurface;
use ngterm_proto::command::{SessionCmd, SurfaceCmd};
use ngterm_proto::{Args, Command, ControlTree, Request, Response};
use serde_json::json;
use uuid::Uuid;

use super::{Fixture, UNKNOWN, args, failure, wire};
use crate::dashboard::{DashboardMember, Direction};
use crate::model::StatusPane;
use crate::persist::AppSettings;
use crate::zoom::{SurfaceId, ZoomSurface};

fn open(fixture: &mut Fixture, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.send(Command::Dashboard, None, args(edit))
}

fn open_ids(fixture: &mut Fixture, targets: &[String]) -> Response {
    open(fixture, |a| a.targets = Some(targets.to_vec()))
}

fn read_tree(fixture: &Fixture) -> ControlTree {
    fixture.core.tree(None).expect("the frontmost window's tree")
}

fn members(fixture: &Fixture) -> Option<Vec<String>> {
    read_tree(fixture).dashboard_members
}

fn left(id: Uuid) -> String {
    format!("{}:left", wire(id))
}

fn right(id: Uuid) -> String {
    format!("{}:right", wire(id))
}

fn split_on(fixture: &mut Fixture, id: Uuid) {
    fixture.store_mut().unwrap().set_split_visibility(id, true, None);
    assert!(fixture.store().session(id).unwrap().split.exists);
}

fn assert_closed(fixture: &Fixture) {
    let tree = read_tree(fixture);
    assert_eq!(
        (
            tree.dashboard_members,
            tree.dashboard_highlighted,
            tree.dashboard_font_size,
            tree.dashboard_font_mode
        ),
        (None, None, None, None)
    );
}

// MARK: - argument checks

/// One case's change to the request args.
type ArgsEdit = Box<dyn Fn(&mut Args)>;

#[test]
fn refusals_carry_upstreams_strings_and_open_nothing() {
    let mut fixture = Fixture::new(0);
    let sid = wire(fixture.sessions[0]);
    let close_error = "dashboard --close takes no ids, --mru, or font options";
    let cases: Vec<(ArgsEdit, String)> = vec![
        (
            Box::new(|_| {}),
            "dashboard requires at least one session id".to_owned(),
        ),
        (
            Box::new(|a| {
                a.font_size = Some(12.0);
                a.auto_size = Some(true);
            }),
            "dashboard: --font-size is mutually exclusive with --auto-size".to_owned(),
        ),
        (
            Box::new(|a| a.font_size = Some(0.0)),
            "dashboard --font-size must be a positive number".to_owned(),
        ),
        (
            Box::new(|a| a.font_size = Some(-3.0)),
            "dashboard --font-size must be a positive number".to_owned(),
        ),
        (
            Box::new(|a| a.font_size = Some(f64::NAN)),
            "dashboard --font-size must be a positive number".to_owned(),
        ),
        (Box::new(|a| a.close = Some(true)), close_error.to_owned()),
        (
            Box::new(|a| {
                a.targets = None;
                a.close = Some(true);
                a.mru = Some(true);
            }),
            close_error.to_owned(),
        ),
        (
            Box::new(|a| {
                a.targets = None;
                a.close = Some(true);
                a.auto_size = Some(true);
            }),
            close_error.to_owned(),
        ),
        (
            Box::new(|a| a.mru = Some(true)),
            "dashboard --mru cannot be combined with explicit session ids".to_owned(),
        ),
    ];
    for (at, (edit, error)) in cases.into_iter().enumerate() {
        let response = open(&mut fixture, |a| {
            // the empty case sends no ids; every other case names the session
            if at > 0 {
                a.targets = Some(vec![sid.clone()]);
            }
            edit(a);
        });
        assert_eq!(response, failure(&error), "case {at}");
        assert_closed(&fixture);
    }
}

#[test]
fn a_malformed_pane_ref_fails_the_whole_request() {
    let mut fixture = Fixture::new(0);
    let sid = wire(fixture.sessions[0]);
    let first = left(fixture.sessions[0]);
    let surface_form = format!("surface:{sid}:left");
    for bad in ["a:lft", "a:scratch", "a:overlay", "a:", ":left", surface_form.as_str()] {
        let response = open_ids(&mut fixture, &[first.clone(), bad.to_owned()]);
        let error = format!("dashboard: invalid session id '{bad}' — use <id>, <id>:left, or <id>:right");
        assert_eq!(response, failure(&error));
        assert_closed(&fixture);
    }
    // --close never reads the grammar
    let response = open(&mut fixture, |a| {
        a.targets = Some(vec!["a:nope".to_owned()]);
        a.close = Some(true);
    });
    assert_eq!(
        response,
        failure("dashboard --close takes no ids, --mru, or font options")
    );
}

#[test]
fn nothing_resolved_and_window_errors() {
    let mut fixture = Fixture::new(1);
    let single = fixture.sessions[0];
    assert_eq!(
        open_ids(&mut fixture, &[UNKNOWN.to_owned()]),
        failure("no dashboard sessions resolved")
    );
    assert_eq!(
        open_ids(&mut fixture, &[right(single)]),
        failure("no dashboard sessions resolved"),
        "a split pane the session lacks expands to nothing"
    );
    let response = open(&mut fixture, |a| {
        a.targets = Some(vec![wire(single)]);
        a.window = Some(UNKNOWN.to_owned());
    });
    assert_eq!(response, failure(&format!("no such window: {UNKNOWN}")));
    assert_closed(&fixture);

    let window = fixture.window;
    fixture.core.close_window(window);
    assert_eq!(
        open(&mut fixture, |a| a.close = Some(true)),
        failure("window not open — window.select it first")
    );
    assert_eq!(
        open(&mut fixture, |a| a.mru = Some(true)),
        failure("window not open — window.select it first")
    );
}

// MARK: - opening

#[test]
fn bare_ids_show_each_pane_and_the_highlight_starts_on_the_first() {
    let mut fixture = Fixture::new(1);
    let (single, split) = (fixture.sessions[0], fixture.sessions[1]);
    split_on(&mut fixture, split);
    let response = open_ids(&mut fixture, &[wire(single), wire(split)]);
    assert_eq!(response, Response::success());
    let tree = read_tree(&fixture);
    let expected = vec![left(single), left(split), right(split)];
    assert_eq!(tree.dashboard_members, Some(expected.clone()));
    assert_eq!(tree.dashboard_highlighted, Some(expected[0].clone()));
    assert_eq!(tree.dashboard_font_mode.as_deref(), Some("untouched"));
    assert_eq!(tree.dashboard_font_size, None);

    assert_eq!(open(&mut fixture, |a| a.close = Some(true)), Response::success());
    assert_closed(&fixture);
    assert_eq!(
        open(&mut fixture, |a| a.close = Some(true)),
        Response::success(),
        "closing twice is fine"
    );
}

#[test]
fn pane_refs_pick_one_pane_in_request_order_and_dedup() {
    let mut fixture = Fixture::new(1);
    let (single, split) = (fixture.sessions[0], fixture.sessions[1]);
    split_on(&mut fixture, split);
    let targets = [
        format!("{}:BOTTOM", wire(split)),
        format!("{}:primary", wire(single)),
        left(split),
        left(single),
    ];
    assert_eq!(open_ids(&mut fixture, &targets), Response::success());
    assert_eq!(members(&fixture), Some(vec![right(split), left(single), left(split)]));

    // a prefix and `active` resolve in the window
    let prefix = wire(single)[..8].to_owned();
    assert_eq!(
        open_ids(&mut fixture, &[format!("{prefix}:left"), "active".to_owned()]),
        Response::success()
    );
    assert_eq!(members(&fixture), Some(vec![left(single), left(split), right(split)]));
}

#[test]
fn font_modes_set_the_applied_size() {
    let settings = AppSettings {
        font_size: Some(16.0),
        ..AppSettings::default()
    };
    let mut fixture = Fixture::with_settings(1, settings);
    let ids: Vec<String> = fixture.sessions.iter().copied().map(wire).collect();
    open(&mut fixture, |a| {
        a.targets = Some(ids.clone());
        a.font_size = Some(14.0);
    });
    let tree = read_tree(&fixture);
    assert_eq!(
        (tree.dashboard_font_mode.as_deref(), tree.dashboard_font_size),
        (Some("fixed"), Some(14.0))
    );

    open(&mut fixture, |a| {
        a.targets = Some(ids.clone());
        a.auto_size = Some(true);
    });
    let tree = read_tree(&fixture);
    // two cells: a 2×1 grid over the Settings size, 16 × 0.85
    assert_eq!(
        (tree.dashboard_font_mode.as_deref(), tree.dashboard_font_size),
        (Some("auto"), Some(14.0))
    );

    open_ids(&mut fixture, &ids);
    let tree = read_tree(&fixture);
    assert_eq!(
        (tree.dashboard_font_mode.as_deref(), tree.dashboard_font_size),
        (Some("untouched"), None)
    );
}

#[test]
fn the_auto_size_base_defaults_to_ghosttys_size() {
    let mut fixture = Fixture::new(3);
    let ids: Vec<String> = fixture.sessions.iter().copied().map(wire).collect();
    open(&mut fixture, |a| {
        a.targets = Some(ids);
        a.auto_size = Some(true);
    });
    // four cells: 2×2 over 13
    assert_eq!(read_tree(&fixture).dashboard_font_size, Some(10.0));
}

#[test]
fn an_auto_size_follows_the_cells_that_survive() {
    let mut fixture = Fixture::new(3);
    let ids: Vec<String> = fixture.sessions.iter().copied().map(wire).collect();
    open(&mut fixture, |a| {
        a.targets = Some(ids);
        a.auto_size = Some(true);
    });
    assert_eq!(read_tree(&fixture).dashboard_font_size, Some(10.0));
    for id in fixture.sessions.clone().into_iter().skip(1) {
        fixture.store_mut().unwrap().close_session(id);
    }
    // one cell left: 1×1 over 13, in the read-back at once and in what the frontend applies after a reconcile
    assert_eq!(read_tree(&fixture).dashboard_font_size, Some(13.0));
    fixture.core.reconcile_dashboards();
    let controller = fixture.core.dashboard(fixture.window).unwrap();
    assert_eq!(controller.applied_font_size(), Some(13.0));
}

#[test]
fn what_it_left_out_is_named_in_the_text() {
    let mut fixture = Fixture::new(10);
    let single = fixture.sessions[0];
    let response = open_ids(&mut fixture, &[wire(single), UNKNOWN.to_owned(), right(single)]);
    assert_eq!(
        response.result,
        Some(json!({"text": format!("unresolved: {UNKNOWN}, {}", right(single))}))
    );
    assert_eq!(members(&fixture), Some(vec![left(single)]));

    let ten: Vec<String> = fixture.sessions[1..].iter().copied().map(wire).collect();
    assert_eq!(ten.len(), 10);
    let response = open_ids(&mut fixture, &ten);
    assert_eq!(
        response.result,
        Some(json!({"text": "dropped 1 pane(s) beyond the 9-cell limit"}))
    );
    let nine: Vec<String> = fixture.sessions[1..10].iter().copied().map(left).collect();
    assert_eq!(members(&fixture), Some(nine));

    let mut both = vec![UNKNOWN.to_owned()];
    both.extend(ten);
    let response = open_ids(&mut fixture, &both);
    let text = format!("unresolved: {UNKNOWN}; dropped 1 pane(s) beyond the 9-cell limit");
    assert_eq!(response.result, Some(json!({ "text": text })));
}

#[test]
fn mru_takes_the_recent_sessions_most_recent_first() {
    let mut fixture = Fixture::new(2);
    let [oldest, split, newest] = [fixture.sessions[0], fixture.sessions[1], fixture.sessions[2]];
    split_on(&mut fixture, split);
    let store = fixture.store_mut().unwrap();
    for id in [oldest, split, newest] {
        store.select_session(Some(id));
    }
    assert_eq!(open(&mut fixture, |a| a.mru = Some(true)), Response::success());
    let tree = read_tree(&fixture);
    assert_eq!(
        tree.dashboard_members,
        Some(vec![left(newest), left(split), right(split), left(oldest)])
    );
    assert_eq!(tree.dashboard_highlighted, Some(left(newest)));
}

#[test]
fn mru_without_recent_sessions_is_refused() {
    let mut fixture = Fixture::new(0);
    let only = fixture.sessions[0];
    fixture.store_mut().unwrap().close_session(only);
    assert_eq!(
        open(&mut fixture, |a| a.mru = Some(true)),
        failure("no recent sessions")
    );
}

#[test]
fn a_dashboard_and_a_terminal_zoom_replace_each_other() {
    let mut fixture = Fixture::new(0);
    let sid = fixture.sessions[0];
    let window = fixture.window;
    let zoomed = SurfaceId {
        session: sid,
        surface: ZoomSurface::Primary,
    };
    fixture.core.set_zoom_target(window, Some(zoomed));
    open_ids(&mut fixture, &[wire(sid)]);
    let opened = read_tree(&fixture);
    assert_eq!(opened.dashboard_members, Some(vec![left(sid)]));
    assert_eq!(opened.zoomed_surface, None);

    let response = fixture.send(
        Command::Surface(SurfaceCmd::Zoom),
        Some("active"),
        args(|a| a.mode = Some("show".into())),
    );
    assert!(response.ok, "{response:?}");
    assert_eq!(read_tree(&fixture).zoomed_surface, Some(zoomed.control_id()));
    assert_closed(&fixture);
}

#[test]
fn the_dashboard_follows_closed_sessions_and_panes() {
    let mut fixture = Fixture::new(2);
    let [a, b, c] = [fixture.sessions[0], fixture.sessions[1], fixture.sessions[2]];
    split_on(&mut fixture, b);
    open_ids(&mut fixture, &[wire(a), wire(b), wire(c)]);
    fixture.core.dashboard_move(fixture.window, Direction::Right);
    assert_eq!(read_tree(&fixture).dashboard_highlighted, Some(left(b)));

    let response = fixture.session(SessionCmd::Close, Some(&wire(b)), Args::default());
    assert!(response.ok, "{response:?}");
    let expected = Some(vec![left(a), left(c)]);
    assert_eq!(members(&fixture), expected, "the read-back follows at once");
    assert_eq!(read_tree(&fixture).dashboard_highlighted, Some(left(a)));
    fixture.core.reconcile_dashboards();
    let controller = fixture.core.dashboard(fixture.window).unwrap();
    assert_eq!(
        controller.members(),
        [DashboardMember::primary(a), DashboardMember::primary(c)]
    );

    for id in [a, c] {
        fixture.store_mut().unwrap().close_session(id);
    }
    assert_closed(&fixture);
    fixture.core.reconcile_dashboards();
    assert!(
        fixture.core.dashboard(fixture.window).is_none(),
        "an emptied dashboard closes"
    );
}

// MARK: - the frontend's actions

#[test]
fn entering_a_cell_selects_its_session_and_focuses_its_pane() {
    let mut fixture = Fixture::new(1);
    let (first, split) = (fixture.sessions[0], fixture.sessions[1]);
    let window = fixture.window;
    split_on(&mut fixture, split);
    fixture.store_mut().unwrap().set_split_visibility(split, false, None);
    let session = fixture.store_mut().unwrap().session_mut(split).unwrap();
    session
        .split_surface
        .replace(Box::new(ngterm_host::fakes::FakeSurface::new("R")));
    fixture.store_mut().unwrap().select_session(Some(first));
    open_ids(&mut fixture, &[wire(first), wire(split)]);

    fixture.core.dashboard_enter(window, DashboardMember::split(split));
    assert!(fixture.core.dashboard(window).is_none());
    assert_eq!(fixture.selected(), Some(split));
    assert!(
        fixture.store().session(split).unwrap().split.focused,
        "the hidden split's right pane holds focus"
    );
    assert!(fixture.core.pane_focus_requested(split));

    open_ids(&mut fixture, &[wire(first), wire(split)]);
    fixture.core.dashboard_enter(window, DashboardMember::primary(split));
    assert!(!fixture.store().session(split).unwrap().split.focused);
}

#[test]
fn highlight_close_and_the_gui_toggle() {
    let mut fixture = Fixture::new(1);
    let window = fixture.window;
    let [a, b] = [fixture.sessions[0], fixture.sessions[1]];
    fixture.core.toggle_dashboard(window);
    let opened = read_tree(&fixture);
    // the last added session is the most recent; two cells auto-sized over 13
    assert_eq!(opened.dashboard_members, Some(vec![left(b), left(a)]));
    assert_eq!(
        (opened.dashboard_font_mode.as_deref(), opened.dashboard_font_size),
        (Some("auto"), Some(11.0))
    );
    fixture.core.dashboard_highlight(window, DashboardMember::primary(a));
    assert_eq!(tree_highlight(&fixture), Some(left(a)));
    fixture.core.dashboard_highlight(window, DashboardMember::split(a));
    assert_eq!(tree_highlight(&fixture), Some(left(a)), "not a cell");
    fixture.core.toggle_dashboard(window);
    assert_closed(&fixture);
    assert!(fixture.core.pane_focus_requested(b));

    let zoomed = SurfaceId {
        session: a,
        surface: ZoomSurface::Primary,
    };
    fixture.core.set_zoom_target(window, Some(zoomed));
    fixture.core.toggle_dashboard(window);
    assert_closed(&fixture);
    fixture.core.set_zoom_target(window, None);
    fixture.core.toggle_dashboard(window);
    fixture.core.dashboard_close(window);
    assert_closed(&fixture);
}

fn tree_highlight(fixture: &Fixture) -> Option<String> {
    read_tree(fixture).dashboard_highlighted
}

#[test]
fn opening_ends_the_active_sessions_search_once() {
    let mut fixture = Fixture::new(0);
    let sid = fixture.sessions[0];
    let surface = ngterm_host::fakes::FakeSurface::new("L");
    let probe = surface.probe();
    let session = fixture.store_mut().unwrap().session_mut(sid).unwrap();
    session.surface.replace(Box::new(surface));
    session.search.active = true;
    session.search.owner = Some(crate::model::StatusPane::Left);
    open_ids(&mut fixture, &[wire(sid)]);
    assert_eq!(probe.actions(), ["end_search"]);
    open_ids(&mut fixture, &[wire(sid)]);
    assert_eq!(
        probe.actions(),
        ["end_search"],
        "a re-open over an open grid ends nothing"
    );
}

/// Lazy restore: the grid wakes only its cells, so `session.search` wakes the dormant session it selects under it, or
/// its poll would wait on a terminal that never starts.
#[test]
fn a_search_under_the_grid_wakes_its_dormant_target() {
    let mut fixture = Fixture::new(1);
    let (shown, dormant) = (fixture.sessions[0], fixture.sessions[1]);
    let session = fixture
        .store_mut()
        .and_then(|store| store.session_mut(dormant))
        .expect("the dormant session");
    session.was_restored = true;
    let token = wire(session.pane_identity);
    let _ = session
        .pane_slot_mut(StatusPane::Left)
        .replace(Box::new(FakeSurface::new(token.clone()).with_realized(false)));
    assert!(open_ids(&mut fixture, &[wire(shown)]).ok);
    assert_eq!(fixture.core.request_pane_spawn(dormant, &token), None);
    fixture.core.wake_shown_sessions();
    assert_eq!(
        fixture.core.take_spawn_grants(),
        Vec::new(),
        "the grid shows only the other"
    );
    let _ = fixture.core.begin(&Request {
        cmd: Command::Session(SessionCmd::Search),
        target: Some(wire(dormant)),
        args: Some(args(|a| a.text = Some("x".to_owned()))),
    });
    let woken: Vec<Uuid> = fixture
        .core
        .take_spawn_grants()
        .into_iter()
        .map(|grant| grant.session)
        .collect();
    assert_eq!(woken, vec![dormant]);
}
