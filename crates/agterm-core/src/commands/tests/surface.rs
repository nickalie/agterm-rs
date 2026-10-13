//! Terminal I/O over fake surfaces: `session.type`, `text`, `copy`, `paste`, `selectall`, `search`, `surface.*`.

use agterm_host::fakes::{FakeSurface, FakeSurfaceProbe};
use agterm_proto::command::{SessionCmd, SurfaceCmd};
use agterm_proto::{Command, Request, Response};
use serde_json::json;
use uuid::Uuid;

use super::{Fixture, UNKNOWN, args, failure, wire};
use crate::Dispatched;
use crate::model::SearchReport;

const TOKEN: &str = "TOKEN-L";

/// One case's change to the request args.
type ArgsEdit = Box<dyn Fn(&mut agterm_proto::Args)>;

/// One window with two sessions, the second selected; the first one's main pane holds a fake terminal.
fn fixture_with_terminal() -> (Fixture, Uuid, FakeSurfaceProbe) {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let probe = install(&mut fixture, id, FakeSurface::new(TOKEN));
    (fixture, id, probe)
}

fn install(fixture: &mut Fixture, id: Uuid, surface: FakeSurface) -> FakeSurfaceProbe {
    let probe = surface.probe();
    let session = fixture.store_mut().and_then(|store| store.session_mut(id)).unwrap();
    session.surface.replace(Box::new(surface));
    probe
}

fn result(response: &Response) -> serde_json::Value {
    assert!(response.ok, "{response:?}");
    response.result.clone().unwrap_or_default()
}

fn request(cmd: Command, target: Option<&str>, edit: impl FnOnce(&mut agterm_proto::Args)) -> Request {
    Request {
        cmd,
        target: target.map(str::to_owned),
        args: Some(args(edit)),
    }
}

fn resume(core: &mut crate::Core, dispatched: Dispatched) -> Dispatched {
    match dispatched {
        Dispatched::Pending { resume, .. } => resume(core),
        done @ Dispatched::Done(_) => done,
    }
}

fn answer(dispatched: Dispatched) -> Response {
    match dispatched {
        Dispatched::Done(response) => response,
        Dispatched::Pending { .. } => panic!("still pending"),
    }
}

// MARK: - session.type

#[test]
fn type_queues_the_text_on_the_main_pane_and_names_it() {
    let (mut fixture, id, probe) = fixture_with_terminal();
    let target = wire(id);
    let response = fixture.session(SessionCmd::Type, Some(&target), args(|a| a.text = Some("ls\n".into())));
    assert_eq!(result(&response), json!({"id": target, "pane": "left"}));
    assert_eq!(probe.typed(), ["ls\n"]);
    // typing into a realized pane never moves the selection, `--select` or not
    let response = fixture.session(
        SessionCmd::Type,
        Some(&target),
        args(|a| {
            a.text = Some("x".into());
            a.select = Some(true);
        }),
    );
    assert!(response.ok);
    assert_ne!(fixture.selected(), Some(id));
}

#[test]
fn type_refuses_as_upstream() {
    let (mut fixture, id, probe) = fixture_with_terminal();
    let target = wire(id);
    let cases: Vec<(ArgsEdit, &str)> = vec![
        (Box::new(|_| {}), "session.type requires text"),
        (
            Box::new(|a| a.text = Some("a\0b".into())),
            "text must not contain a NUL byte",
        ),
        (
            Box::new(|a| {
                a.text = Some("x".into());
                a.pane = Some("middle".into());
            }),
            "invalid pane: middle",
        ),
        (
            Box::new(|a| {
                a.text = Some("x".into());
                a.pane_id = Some(UNKNOWN.into());
            }),
            "unknown pane id: zzz",
        ),
        (
            Box::new(|a| {
                a.text = Some("x".into());
                a.pane = Some("right".into());
            }),
            "session has no split pane",
        ),
        (
            Box::new(|a| {
                a.text = Some("x".into());
                a.pane = Some("scratch".into());
            }),
            "session has no scratch terminal",
        ),
    ];
    for (edit, error) in cases {
        assert_eq!(
            fixture.session(SessionCmd::Type, Some(&target), args(edit)),
            failure(error)
        );
    }
    assert_eq!(
        fixture.session(SessionCmd::Type, Some(UNKNOWN), args(|a| a.text = Some("x".into()))),
        failure("no such session: zzz")
    );
    assert!(probe.typed().is_empty());
}

#[test]
fn type_follows_a_pane_token_and_falls_back_to_the_named_role() {
    let (mut fixture, id, probe) = fixture_with_terminal();
    let target = wire(id);
    let response = fixture.session(
        SessionCmd::Type,
        Some(&target),
        args(|a| {
            a.text = Some("by token".into());
            a.pane_id = Some(TOKEN.into());
            a.pane = Some("right".into());
        }),
    );
    assert_eq!(result(&response)["pane"], "left");
    let response = fixture.session(
        SessionCmd::Type,
        Some(&target),
        args(|a| {
            a.text = Some("fallback".into());
            a.pane_id = Some("gone".into());
            a.pane = Some("primary".into());
        }),
    );
    assert_eq!(result(&response)["pane"], "left");
    assert_eq!(probe.typed(), ["by token", "fallback"]);
}

#[test]
fn type_waits_for_a_main_pane_coming_up_and_selects_when_asked() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let probe = install(&mut fixture, id, FakeSurface::new(TOKEN).with_realized(false));
    let request = request(Command::Session(SessionCmd::Type), Some(&wire(id)), |a| {
        a.text = Some("late\n".into());
        a.select = Some(true);
    });
    let pending = fixture.core.begin(&request);
    assert!(matches!(pending, Dispatched::Pending { .. }));
    assert_eq!(fixture.selected(), Some(id));
    let pending = resume(&mut fixture.core, pending);
    assert!(matches!(pending, Dispatched::Pending { .. }));
    probe.set_realized(true);
    let response = answer(resume(&mut fixture.core, pending));
    assert_eq!(result(&response), json!({"id": wire(id), "pane": "left"}));
    assert_eq!(probe.typed(), ["late\n"]);
}

#[test]
fn type_into_a_terminal_that_never_comes_up_is_refused_without_selecting() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let probe = install(&mut fixture, id, FakeSurface::new(TOKEN).with_realized(false));
    let response = fixture.session(SessionCmd::Type, Some(&wire(id)), args(|a| a.text = Some("x".into())));
    assert_eq!(response, failure("session not realized"));
    assert_ne!(fixture.selected(), Some(id));
    assert!(probe.typed().is_empty());
    // a session without any terminal waits the same way
    let other = fixture.sessions[1];
    let response = fixture.session(
        SessionCmd::Type,
        Some(&wire(other)),
        args(|a| a.text = Some("x".into())),
    );
    assert_eq!(response, failure("session not realized"));
}

#[test]
fn typed_input_clears_a_blocked_status_as_the_keyboard_does() {
    let (mut fixture, id, _probe) = fixture_with_terminal();
    let target = wire(id);
    let blocked = fixture.session(
        SessionCmd::Status,
        Some(&target),
        args(|a| a.status = Some("blocked".into())),
    );
    assert!(blocked.ok);
    let response = fixture.session(SessionCmd::Type, Some(&target), args(|a| a.text = Some("yes\n".into())));
    assert!(response.ok);
    let status = fixture
        .store()
        .session(id)
        .map(|session| session.agent_indicator.status);
    assert_eq!(status, Some(crate::model::AgentStatus::Idle));
}

// MARK: - session.text

#[test]
fn text_reads_the_screen_the_scrollback_or_the_last_content_lines() {
    let (mut fixture, id, probe) = fixture_with_terminal();
    let target = wire(id);
    let mut lines: Vec<String> = (0..30).map(|n| format!("line {n}")).collect();
    lines.extend(["$ ".to_owned(), String::new(), "   ".to_owned()]);
    probe.set_lines(lines);
    let screen = result(&fixture.session(SessionCmd::Text, Some(&target), args(|_| {})));
    assert_eq!(screen["pane"], "left");
    assert!(screen["text"].as_str().unwrap().starts_with("line 9\n"));
    let all = result(&fixture.session(SessionCmd::Text, Some(&target), args(|a| a.all = Some(true))));
    assert!(all["text"].as_str().unwrap().starts_with("line 0\n"));
    let last = result(&fixture.session(SessionCmd::Text, Some(&target), args(|a| a.lines = Some(2))));
    assert_eq!(last, json!({"text": "line 29\n$ ", "pane": "left"}));
    let by_token = fixture.session(
        SessionCmd::Text,
        Some(&target),
        args(|a| a.pane_id = Some(TOKEN.into())),
    );
    assert_eq!(result(&by_token)["pane"], "left");
}

#[test]
fn text_refuses_as_upstream() {
    let (mut fixture, id, _probe) = fixture_with_terminal();
    let target = wire(id);
    let cases: Vec<(ArgsEdit, &str)> = vec![
        (Box::new(|a| a.lines = Some(0)), "--lines must be greater than 0"),
        (Box::new(|a| a.lines = Some(-1)), "--lines must be greater than 0"),
        (
            Box::new(|a| {
                a.all = Some(true);
                a.lines = Some(3);
            }),
            "use either --all or --lines, not both",
        ),
        (Box::new(|a| a.pane = Some("middle".into())), "invalid pane: middle"),
        (Box::new(|a| a.pane_id = Some(UNKNOWN.into())), "unknown pane id: zzz"),
        (Box::new(|a| a.pane = Some("right".into())), "session has no split pane"),
        (
            Box::new(|a| a.pane = Some("scratch".into())),
            "session has no scratch terminal",
        ),
    ];
    for (edit, error) in cases {
        assert_eq!(
            fixture.session(SessionCmd::Text, Some(&target), args(edit)),
            failure(error)
        );
    }
    assert_eq!(
        fixture.session(SessionCmd::Text, Some(UNKNOWN), args(|_| {})),
        failure("no such session: zzz")
    );
    // a session whose terminal is not up yet, or has none
    let other = wire(fixture.sessions[1]);
    assert_eq!(
        fixture.session(SessionCmd::Text, Some(&other), args(|_| {})),
        failure("session not realized")
    );
}

// MARK: - selection

#[test]
fn selectall_then_copy_reads_the_selection_back_and_paste_runs_on_the_pane() {
    let (mut fixture, id, probe) = fixture_with_terminal();
    let target = wire(id);
    probe.set_lines(["one", "two"]);
    assert_eq!(
        fixture.session(SessionCmd::Copy, Some(&target), args(|_| {})),
        failure("no selection")
    );
    let response = fixture.session(SessionCmd::SelectAll, Some(&target), args(|_| {}));
    assert_eq!(result(&response), json!({"id": target}));
    let response = fixture.session(SessionCmd::Copy, Some(&target), args(|_| {}));
    assert_eq!(result(&response), json!({"text": "one\ntwo"}));
    let response = fixture.session(
        SessionCmd::Paste,
        Some(&target),
        args(|a| a.pane = Some("primary".into())),
    );
    assert_eq!(result(&response), json!({"id": target}));
    assert_eq!(probe.actions(), ["select_all", "paste_from_clipboard"]);
}

#[test]
fn selection_commands_refuse_as_upstream() {
    let (mut fixture, id, _probe) = fixture_with_terminal();
    let target = wire(id);
    for (pane, error) in [
        ("middle", "--pane must be left, right, or scratch"),
        ("right", "session has no split pane"),
        ("split", "session has no split pane"),
        ("scratch", "session has no scratch terminal"),
    ] {
        assert_eq!(
            fixture.session(SessionCmd::Paste, Some(&target), args(|a| a.pane = Some(pane.into()))),
            failure(error)
        );
    }
    for cmd in [SessionCmd::SelectAll, SessionCmd::Copy, SessionCmd::Paste] {
        assert_eq!(
            fixture.session(cmd, Some(UNKNOWN), args(|_| {})),
            failure("no such session: zzz")
        );
    }
    let other = wire(fixture.sessions[1]);
    for cmd in [SessionCmd::SelectAll, SessionCmd::Copy, SessionCmd::Paste] {
        assert_eq!(
            fixture.session(cmd, Some(&other), args(|_| {})),
            failure("session not realized")
        );
    }
}

// MARK: - session.search

#[test]
fn search_opens_queries_and_answers_with_the_count_libghostty_reports() {
    let (mut fixture, id, probe) = fixture_with_terminal();
    let target = wire(id);
    let request = request(Command::Session(SessionCmd::Search), Some(&target), |a| {
        a.text = Some("needle".into());
    });
    let pending = fixture.core.begin(&request);
    assert_eq!(fixture.selected(), Some(id));
    assert_eq!(probe.actions(), ["start_search"]);
    fixture.core.report_search(id, TOKEN, SearchReport::Started(None));
    // the needle goes out a beat later, then the count is awaited
    let pending = resume(&mut fixture.core, pending);
    assert_eq!(probe.actions(), ["start_search", "search:needle"]);
    fixture.core.report_search(id, TOKEN, SearchReport::Total(Some(2)));
    let response = answer(resume(&mut fixture.core, pending));
    assert_eq!(result(&response), json!({"text": "2 matches", "count": 2}));

    // the same needle again sends it without clearing the count, then steps the selection
    let response = fixture.send(
        Command::Session(SessionCmd::Search),
        Some(&target),
        args(|a| {
            a.text = Some("needle".into());
            a.to = Some("next".into());
        }),
    );
    assert_eq!(result(&response)["count"], 2);
    assert_eq!(
        probe.actions()[2..],
        ["search:needle".to_owned(), "navigate_search:previous".to_owned()]
    );

    let response = fixture.send(
        Command::Session(SessionCmd::Search),
        Some(&target),
        args(|a| a.to = Some("close".into())),
    );
    assert_eq!(result(&response), json!({"id": target}));
    assert_eq!(probe.actions().last().map(String::as_str), Some("end_search"));
}

#[test]
fn a_second_start_while_open_ends_the_search_on_its_pane() {
    let (mut fixture, id, probe) = fixture_with_terminal();
    fixture
        .core
        .report_search(id, TOKEN, SearchReport::Started(Some("seed".into())));
    fixture.core.report_search(id, TOKEN, SearchReport::Started(None));
    assert_eq!(probe.actions(), ["end_search"]);
    fixture.core.report_search(id, TOKEN, SearchReport::Ended);
    // an unknown token lands nowhere
    fixture.core.report_search(id, "other", SearchReport::Started(None));
    let session = fixture.store().session(id).unwrap();
    assert!(!session.search.active);
}

#[test]
fn search_refuses_as_upstream() {
    let (mut fixture, id, _probe) = fixture_with_terminal();
    let target = wire(id);
    let search = |fixture: &mut Fixture, target: &str, to: &str| {
        fixture.send(
            Command::Session(SessionCmd::Search),
            Some(target),
            args(|a| a.to = Some(to.into())),
        )
    };
    // the target is resolved before the mode is checked
    assert_eq!(
        search(&mut fixture, UNKNOWN, "sideways"),
        failure("no such session: zzz")
    );
    assert_eq!(
        search(&mut fixture, &target, "sideways"),
        failure("session.search --to must be next|prev|close")
    );
    let other = wire(fixture.sessions[1]);
    assert_eq!(search(&mut fixture, &other, "next"), failure("session not realized"));
    let zoomed = fixture.send(
        Command::Surface(SurfaceCmd::Zoom),
        None,
        args(|a| a.mode = Some("show".into())),
    );
    assert!(zoomed.ok);
    assert_eq!(search(&mut fixture, &target, "next"), failure("terminal zoom active"));
}

// MARK: - surface.cursor

fn surface_id(id: Uuid, kind: &str) -> String {
    format!("surface:{}:{kind}", wire(id))
}

#[test]
fn cursor_reads_the_addressed_or_active_surface() {
    let (mut fixture, id, probe) = fixture_with_terminal();
    probe.set_cursor_column(Some(5));
    let left = surface_id(id, "left");
    let response = fixture.send(Command::Surface(SurfaceCmd::Cursor), Some(&left), args(|_| {}));
    assert_eq!(result(&response), json!({"id": left, "cursor": {"column": 5}}));
    let response = fixture.send(
        Command::Surface(SurfaceCmd::Cursor),
        Some(&wire(id)),
        args(|a| a.pane_id = Some(TOKEN.into())),
    );
    assert_eq!(result(&response)["id"], left);
    // the active surface is the selected session's; select the one with the terminal
    fixture.session(SessionCmd::Select, Some(&wire(id)), args(|_| {}));
    let response = fixture.send(Command::Surface(SurfaceCmd::Cursor), None, args(|_| {}));
    assert_eq!(result(&response)["id"], left);
    probe.set_cursor_column(None);
    assert_eq!(
        fixture.send(Command::Surface(SurfaceCmd::Cursor), Some(&left), args(|_| {})),
        failure("failed to read cursor position")
    );
    probe.set_realized(false);
    assert_eq!(
        fixture.send(Command::Surface(SurfaceCmd::Cursor), Some(&left), args(|_| {})),
        failure("surface not realized")
    );
}

#[test]
fn cursor_refuses_as_upstream() {
    let (mut fixture, id, _probe) = fixture_with_terminal();
    let sid = wire(id);
    let cases = [
        (UNKNOWN.to_owned(), None, "invalid surface: zzz".to_owned()),
        (sid.clone(), None, format!("invalid surface: {sid}")),
        (
            format!("surface:{sid}:bogus"),
            None,
            format!("invalid surface: surface:{sid}:bogus"),
        ),
        (
            surface_id(id, "right"),
            None,
            format!("surface not available: surface:{sid}:right"),
        ),
        (
            surface_id(id, "scratch"),
            None,
            format!("surface not available: surface:{sid}:scratch"),
        ),
        (sid.clone(), Some("nope"), "unknown pane id: nope".to_owned()),
        (
            surface_id(id, "left"),
            Some("nope"),
            "surface.cursor: --pane-id takes a session target".to_owned(),
        ),
        (
            "quick".to_owned(),
            Some("nope"),
            "surface.cursor: --pane-id takes a session target".to_owned(),
        ),
        ("quick".to_owned(), None, "surface not available: quick".to_owned()),
        (
            format!("surface:{}:left", wire(Uuid::from_u128(9))),
            None,
            format!("no such surface: surface:{}:left", wire(Uuid::from_u128(9))),
        ),
    ];
    for (target, pane_id, error) in cases {
        let response = fixture.send(
            Command::Surface(SurfaceCmd::Cursor),
            Some(&target),
            args(|a| a.pane_id = pane_id.map(str::to_owned)),
        );
        assert_eq!(response, failure(&error), "{target}");
    }
    // a session whose main pane has no terminal yet
    let other = surface_id(fixture.sessions[1], "left");
    assert_eq!(
        fixture.send(Command::Surface(SurfaceCmd::Cursor), Some(&other), args(|_| {})),
        failure(&format!("surface not available: {other}"))
    );
}

// MARK: - surface.zoom

fn zoomed(fixture: &mut Fixture) -> Option<String> {
    let tree = fixture.core.tree(None).unwrap();
    tree.zoomed_surface
}

#[test]
fn zoom_shows_hides_and_toggles_a_surface() {
    let (mut fixture, id, _probe) = fixture_with_terminal();
    let selected = fixture.selected().unwrap();
    let zoom = |fixture: &mut Fixture, target: Option<&str>, mode: Option<&str>| {
        fixture.send(
            Command::Surface(SurfaceCmd::Zoom),
            target,
            args(|a| a.mode = mode.map(str::to_owned)),
        )
    };
    // hiding with nothing zoomed is a bare ok
    assert_eq!(zoom(&mut fixture, None, Some("hide")), Response::success());
    let active = surface_id(selected, "left");
    assert_eq!(result(&zoom(&mut fixture, None, Some("show"))), json!({"id": active}));
    assert_eq!(zoomed(&mut fixture), Some(active.clone()));
    // an explicit id retargets; toggling it again unzooms
    let left = surface_id(id, "left");
    assert_eq!(result(&zoom(&mut fixture, Some(&left), None)), json!({"id": left}));
    assert_eq!(zoomed(&mut fixture), Some(left.clone()));
    assert!(zoom(&mut fixture, Some(&left), None).ok);
    assert_eq!(zoomed(&mut fixture), None);
    // a zoom whose session closes is dropped
    assert!(zoom(&mut fixture, Some(&left), Some("show")).ok);
    assert!(fixture.session(SessionCmd::Close, Some(&wire(id)), args(|_| {})).ok);
    let _ = fixture.core.commit();
    assert_eq!(zoomed(&mut fixture), None);
}

#[test]
fn zoom_refuses_as_upstream() {
    let (mut fixture, id, _probe) = fixture_with_terminal();
    let zoom = |fixture: &mut Fixture, target: &str, mode: &str| {
        fixture.send(
            Command::Surface(SurfaceCmd::Zoom),
            Some(target),
            args(|a| a.mode = Some(mode.into())),
        )
    };
    assert_eq!(
        zoom(&mut fixture, "active", "on"),
        failure("invalid surface zoom mode: on")
    );
    assert_eq!(
        zoom(&mut fixture, "quick", "show"),
        failure("surface not available: quick")
    );
    assert_eq!(zoom(&mut fixture, UNKNOWN, "show"), failure("invalid surface: zzz"));
    let right = surface_id(id, "right");
    assert_eq!(
        zoom(&mut fixture, &right, "show"),
        failure(&format!("surface not available: {right}"))
    );
    // hide never checks availability
    assert!(zoom(&mut fixture, &right, "hide").ok);
}
