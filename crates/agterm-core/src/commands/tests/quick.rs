//! `quick`, `quick.type` and `quick.text` (upstream's quick-terminal dispatcher tests and the app-side behavior of
//! `ControlServer+AppCommands.swift` > Quick terminal), plus the `quick` target of `surface.cursor` and `surface.zoom`.

use std::time::{Duration, Instant};

use agterm_host::fakes::{FakeSurface, FakeSurfaceProbe};
use agterm_proto::command::{QuickCmd, SurfaceCmd};
use agterm_proto::{Args, Command, Request, Response};
use serde_json::json;

use super::{Fixture, args, failure};
use crate::Dispatched;
use crate::commands::quick::RESHOW_SUPPRESSION;
use crate::commands::typing::ATTEMPTS;

fn quick(fixture: &mut Fixture, mode: Option<&str>) -> Response {
    fixture.send(
        Command::Quick(QuickCmd::Visibility),
        None,
        args(|a| a.mode = mode.map(str::to_owned)),
    )
}

fn type_text(fixture: &mut Fixture, text: Option<&str>) -> Response {
    fixture.send(
        Command::Quick(QuickCmd::Type),
        None,
        args(|a| a.text = text.map(str::to_owned)),
    )
}

fn read_text(fixture: &mut Fixture, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.send(Command::Quick(QuickCmd::Text), None, args(edit))
}

fn surface(fixture: &mut Fixture, cmd: SurfaceCmd, mode: Option<&str>) -> Response {
    fixture.send(
        Command::Surface(cmd),
        Some("quick"),
        args(|a| a.mode = mode.map(str::to_owned)),
    )
}

fn visible(fixture: &Fixture) -> Option<bool> {
    fixture.core.tree(None).unwrap().quick_visible
}

fn zoomed(fixture: &Fixture) -> Option<String> {
    fixture.core.tree(None).unwrap().zoomed_surface
}

/// Shows the quick terminal and installs a fake terminal holding `lines`, as the frontend does on the first show.
fn shown_with_terminal(lines: &[&str]) -> (Fixture, FakeSurfaceProbe) {
    let mut fixture = Fixture::new(1);
    assert_eq!(quick(&mut fixture, Some("show")), Response::success());
    let terminal = FakeSurface::new("");
    let probe = terminal.probe();
    probe.set_lines(lines.iter().copied());
    fixture.core.install_quick_surface(Box::new(terminal));
    (fixture, probe)
}

fn text_of(response: &Response) -> String {
    assert!(response.ok, "{response:?}");
    let text = response.result.as_ref().and_then(|result| result.get("text"));
    text.and_then(serde_json::Value::as_str).unwrap_or_default().to_owned()
}

fn begin(fixture: &mut Fixture, cmd: QuickCmd, edit: impl FnOnce(&mut Args)) -> Dispatched {
    fixture.core.begin(&Request {
        cmd: Command::Quick(cmd),
        target: None,
        args: Some(args(edit)),
    })
}

/// Resumes a waiting command `count` times, each one a further probe.
fn resume(fixture: &mut Fixture, mut dispatched: Dispatched, count: u32) -> Dispatched {
    for _ in 0..count {
        dispatched = match dispatched {
            Dispatched::Pending { resume, .. } => resume(&mut fixture.core),
            done @ Dispatched::Done(_) => return done,
        };
    }
    dispatched
}

fn answer(dispatched: Dispatched) -> Response {
    match dispatched {
        Dispatched::Done(response) => response,
        Dispatched::Pending { .. } => panic!("still pending"),
    }
}

#[test]
fn a_quick_terminal_never_shown_is_refused_at_once() {
    let mut fixture = Fixture::new(0);
    assert_eq!(visible(&fixture), Some(false));
    assert_eq!(type_text(&mut fixture, Some("x")), failure("quick terminal not open"));
    assert_eq!(read_text(&mut fixture, |_| {}), failure("quick terminal not open"));
    let unavailable = failure("surface not available: quick");
    assert_eq!(surface(&mut fixture, SurfaceCmd::Zoom, Some("show")), unavailable);
    assert_eq!(surface(&mut fixture, SurfaceCmd::Cursor, None), unavailable);
    assert!(matches!(
        begin(&mut fixture, QuickCmd::Type, |a| a.text = Some("x".into())),
        Dispatched::Done(_)
    ));
}

#[test]
fn quick_shows_hides_and_toggles_with_a_bare_ok() {
    let mut fixture = Fixture::new(0);
    for (mode, shown) in [
        (Some("show"), true),
        (Some("show"), true),
        (Some("hide"), false),
        (Some("hide"), false),
        (None, true),
        (Some("toggle"), false),
    ] {
        assert_eq!(quick(&mut fixture, mode), Response::success(), "{mode:?}");
        assert_eq!(visible(&fixture), Some(shown), "{mode:?}");
        assert_eq!(fixture.core.quick_visible(), shown);
    }
    assert_eq!(quick(&mut fixture, Some("maybe")), failure("invalid quick mode: maybe"));
    assert_eq!(quick(&mut fixture, Some("on")), failure("invalid quick mode: on"));
    assert_eq!(visible(&fixture), Some(false));
}

#[test]
fn quick_commands_need_an_open_window_after_their_argument_checks() {
    let mut fixture = Fixture::new(0);
    fixture.core.close_window(fixture.window);
    assert_eq!(quick(&mut fixture, Some("show")), failure("no open window"));
    assert_eq!(quick(&mut fixture, Some("maybe")), failure("no open window"));
    assert_eq!(type_text(&mut fixture, Some("x")), failure("no open window"));
    assert_eq!(read_text(&mut fixture, |_| {}), failure("no open window"));
    assert_eq!(type_text(&mut fixture, None), failure("quick.type requires text"));
    assert_eq!(
        read_text(&mut fixture, |a| a.lines = Some(0)),
        failure("--lines must be greater than 0")
    );
    // the user's toggle cannot summon a panel into an app with no window either
    fixture.core.toggle_quick_terminal();
    assert!(!fixture.core.quick_visible());
}

#[test]
fn quick_input_is_validated_before_the_terminal_is_reached() {
    let (mut fixture, probe) = shown_with_terminal(&["$"]);
    assert_eq!(type_text(&mut fixture, None), failure("quick.type requires text"));
    assert_eq!(
        type_text(&mut fixture, Some("safe\u{0}danger\n")),
        failure("text must not contain a NUL byte")
    );
    let refusals: [(Option<bool>, Option<i64>, &str); 3] = [
        (Some(true), Some(2), "use either --all or --lines, not both"),
        (None, Some(0), "--lines must be greater than 0"),
        (None, Some(-1), "--lines must be greater than 0"),
    ];
    for (all, lines, error) in refusals {
        let response = read_text(&mut fixture, |a| {
            a.all = all;
            a.lines = lines;
        });
        assert_eq!(response, failure(error), "{all:?} {lines:?}");
    }
    assert!(probe.typed().is_empty());
}

#[test]
fn quick_type_and_text_reach_the_shown_terminal() {
    let lines = ["one", "two", "three", "   ", ""];
    let (mut fixture, probe) = shown_with_terminal(&lines);
    assert_eq!(type_text(&mut fixture, Some("ls\n")), Response::success());
    assert_eq!(probe.typed(), ["ls\n"]);
    assert_eq!(text_of(&read_text(&mut fixture, |_| {})), lines.join("\n"));
    assert_eq!(
        text_of(&read_text(&mut fixture, |a| a.all = Some(true))),
        lines.join("\n")
    );
    // `--lines` counts content lines from the end of the whole buffer
    assert_eq!(text_of(&read_text(&mut fixture, |a| a.lines = Some(2))), "two\nthree");
    let response = read_text(&mut fixture, |_| {});
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({"ok": true, "result": {"text": lines.join("\n")}})
    );
}

#[test]
fn a_shown_then_hidden_quick_terminal_stays_typable_and_readable() {
    let (mut fixture, probe) = shown_with_terminal(&["marker"]);
    assert_eq!(quick(&mut fixture, Some("hide")), Response::success());
    assert_eq!(type_text(&mut fixture, Some("echo\n")), Response::success());
    assert_eq!(probe.typed(), ["echo\n"]);
    assert_eq!(text_of(&read_text(&mut fixture, |_| {})), "marker");
    assert_eq!(probe.teardowns(), 0, "hiding keeps the shell");
    // but `surface.*` address it only while shown
    let unavailable = failure("surface not available: quick");
    assert_eq!(surface(&mut fixture, SurfaceCmd::Cursor, None), unavailable);
    assert_eq!(surface(&mut fixture, SurfaceCmd::Zoom, Some("show")), unavailable);
}

#[test]
fn a_shown_quick_terminal_not_up_yet_is_probed_then_reported() {
    let mut fixture = Fixture::new(0);
    assert!(quick(&mut fixture, Some("show")).ok);
    // shown, but the frontend has not created its terminal: probe ATTEMPTS more times, then give up
    let typing = begin(&mut fixture, QuickCmd::Type, |a| a.text = Some("x".into()));
    let typing = resume(&mut fixture, typing, ATTEMPTS - 1);
    assert!(matches!(typing, Dispatched::Pending { .. }));
    assert_eq!(
        answer(resume(&mut fixture, typing, 1)),
        failure("quick terminal not realized")
    );
    assert_eq!(
        read_text(&mut fixture, |_| {}),
        failure("failed to read surface buffer")
    );
    // an installed terminal that is not realized is waited on too, even once hidden
    let terminal = FakeSurface::new("").with_realized(false);
    let probe = terminal.probe();
    fixture.core.install_quick_surface(Box::new(terminal));
    assert!(quick(&mut fixture, Some("hide")).ok);
    assert_eq!(
        type_text(&mut fixture, Some("x")),
        failure("quick terminal not realized")
    );
    // and the wait ends once it comes up
    let typing = begin(&mut fixture, QuickCmd::Type, |a| a.text = Some("late".into()));
    let typing = resume(&mut fixture, typing, 3);
    probe.set_realized(true);
    assert_eq!(answer(resume(&mut fixture, typing, 1)), Response::success());
    assert_eq!(probe.typed(), ["late"]);
}

#[test]
fn a_terminal_installed_during_the_wait_takes_the_text() {
    let mut fixture = Fixture::new(0);
    assert!(quick(&mut fixture, Some("show")).ok);
    let reading = begin(&mut fixture, QuickCmd::Text, |_| {});
    assert!(matches!(reading, Dispatched::Pending { .. }));
    let terminal = FakeSurface::new("");
    terminal.probe().set_lines(["ready"]);
    fixture.core.install_quick_surface(Box::new(terminal));
    assert_eq!(text_of(&answer(resume(&mut fixture, reading, 1))), "ready");
}

#[test]
fn the_quick_zoom_fills_the_panel_and_ends_with_it() {
    let (mut fixture, _probe) = shown_with_terminal(&[]);
    let zoom = |fixture: &mut Fixture, mode| surface(fixture, SurfaceCmd::Zoom, Some(mode));
    let ok = Response::with_result(json!({"id": "quick"}));
    assert_eq!(zoom(&mut fixture, "show"), ok);
    assert_eq!(zoomed(&fixture), Some("quick".to_owned()));
    assert!(fixture.core.quick_zoomed());
    assert_eq!(zoom(&mut fixture, "toggle"), ok);
    assert_eq!(zoomed(&fixture), None);
    assert_eq!(zoom(&mut fixture, "toggle"), ok);
    // the quick zoom wins over the window's own
    let left = format!("surface:{}:left", super::wire(fixture.sessions[0]));
    let _ = fixture.send(Command::Surface(SurfaceCmd::Zoom), Some(&left), args(|_| {}));
    assert_eq!(zoomed(&fixture), Some("quick".to_owned()));
    assert_eq!(
        zoom(&mut fixture, "on"),
        failure("invalid surface zoom mode: on"),
        "the mode is checked first"
    );
    assert!(quick(&mut fixture, Some("hide")).ok);
    assert!(!fixture.core.quick_zoomed());
    assert_eq!(zoom(&mut fixture, "show"), failure("surface not available: quick"));
    // shown again, the panel starts unzoomed
    assert!(quick(&mut fixture, Some("show")).ok);
    assert!(!fixture.core.quick_zoomed());
}

#[test]
fn the_quick_cursor_reads_the_shown_terminal() {
    let (mut fixture, probe) = shown_with_terminal(&[]);
    probe.set_cursor_column(Some(7));
    assert_eq!(
        surface(&mut fixture, SurfaceCmd::Cursor, None),
        Response::with_result(json!({"id": "quick", "cursor": {"column": 7}}))
    );
    probe.set_cursor_column(None);
    assert_eq!(
        surface(&mut fixture, SurfaceCmd::Cursor, None),
        failure("failed to read cursor position")
    );
    probe.set_realized(false);
    assert_eq!(
        surface(&mut fixture, SurfaceCmd::Cursor, None),
        failure("surface not realized")
    );
}

#[test]
fn a_shell_exit_drops_the_panel_with_its_terminal() {
    let (mut fixture, probe) = shown_with_terminal(&[]);
    assert!(surface(&mut fixture, SurfaceCmd::Zoom, Some("show")).ok);
    fixture.core.quick_shell_exited();
    assert_eq!(probe.teardowns(), 1);
    assert!(!fixture.core.quick_has_surface());
    assert_eq!(visible(&fixture), Some(false));
    assert_eq!(zoomed(&fixture), None);
    assert_eq!(type_text(&mut fixture, Some("x")), failure("quick terminal not open"));
}

#[test]
fn only_a_panel_the_user_summoned_hides_when_it_loses_focus() {
    let mut fixture = Fixture::new(0);
    // a control show pins the panel
    assert!(quick(&mut fixture, Some("show")).ok);
    fixture.core.quick_lost_focus();
    assert!(fixture.core.quick_visible());
    // hiding clears the pin: the user's summon dismisses on blur
    fixture.core.toggle_quick_terminal();
    assert!(!fixture.core.quick_visible());
    fixture.core.toggle_quick_terminal();
    assert!(fixture.core.quick_visible());
    let blurred = Instant::now();
    fixture.core.quick_lost_focus_at(blurred);
    assert!(!fixture.core.quick_visible());
    // a control show over a panel the user summoned pins it too
    fixture.core.toggle_quick_terminal_at(blurred + RESHOW_SUPPRESSION);
    assert!(quick(&mut fixture, Some("show")).ok);
    fixture.core.quick_lost_focus();
    assert!(fixture.core.quick_visible());
    fixture.core.dismiss_quick_terminal();
    assert!(!fixture.core.quick_visible());
}

#[test]
fn a_toggle_right_after_a_focus_loss_dismissal_does_not_show_the_panel_again() {
    let mut fixture = Fixture::new(0);
    fixture.core.toggle_quick_terminal();
    let blurred = Instant::now();
    fixture.core.quick_lost_focus_at(blurred);
    assert!(!fixture.core.quick_visible());
    // the click on the toggle that took the keyboard away arrives within the window: it only ends it
    fixture
        .core
        .toggle_quick_terminal_at(blurred + Duration::from_millis(299));
    assert!(!fixture.core.quick_visible());
    fixture
        .core
        .toggle_quick_terminal_at(blurred + Duration::from_millis(10));
    assert!(fixture.core.quick_visible(), "the window ends with the first toggle");

    // after the window a toggle shows the panel
    fixture.core.quick_lost_focus_at(blurred);
    fixture.core.toggle_quick_terminal_at(blurred + RESHOW_SUPPRESSION);
    assert!(fixture.core.quick_visible());

    // a control show or a hide that was no focus loss arms nothing
    fixture.core.quick_lost_focus_at(blurred);
    assert!(quick(&mut fixture, Some("show")).ok);
    assert!(quick(&mut fixture, Some("hide")).ok);
    fixture.core.toggle_quick_terminal_at(blurred);
    assert!(fixture.core.quick_visible());
    fixture.core.toggle_quick_terminal_at(blurred);
    assert!(!fixture.core.quick_visible());
    fixture.core.toggle_quick_terminal_at(blurred);
    assert!(fixture.core.quick_visible());

    // a shell exit ends the window too
    fixture.core.quick_lost_focus_at(blurred);
    fixture.core.quick_shell_exited();
    fixture.core.toggle_quick_terminal_at(blurred);
    assert!(fixture.core.quick_visible());
}

#[test]
fn a_fresh_quick_terminal_starts_in_the_active_session_directory_else_home() {
    let mut fixture = Fixture::new(1);
    let active = fixture
        .store()
        .active_session()
        .map(|session| session.effective_cwd().to_owned());
    assert_eq!(Some(fixture.core.quick_terminal_cwd()), active);
    fixture.core.close_window(fixture.window);
    assert_eq!(fixture.core.quick_terminal_cwd(), "/home/u");
}

#[test]
fn opening_a_dashboard_hides_the_quick_terminal() {
    let mut fixture = Fixture::new(0);
    assert_eq!(quick(&mut fixture, Some("show")), Response::success());
    assert_eq!(visible(&fixture), Some(true));
    let targets = Some(vec![super::wire(fixture.sessions[0])]);
    let open = fixture.send(Command::Dashboard, None, args(|a| a.targets = targets));
    assert!(open.ok, "{open:?}");
    assert_eq!(visible(&fixture), Some(false));
}
