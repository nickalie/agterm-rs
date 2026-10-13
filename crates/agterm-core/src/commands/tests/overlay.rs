//! `session.overlay.*` (upstream's `ControlDispatcherOverlayTests`, `AppStoreOverlayTests`, `OverlayJobsTests` and the
//! control server's overlay arms): full error strings, the slot rules shared with the HUD, the exit status captured at
//! the terminal's teardown, the pane overlays, and the remote job claim.

use std::cell::RefCell;
use std::rc::Rc;

use agterm_host::fakes::{FakeSurface, FakeSurfaceProbe};
use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Command, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Fixture, UNKNOWN, answered_id, args, failure, wire};
use crate::JobChannel;
use crate::dashboard::DashboardCover;
use crate::model::{OverlayPane, StatusPane};
use crate::overlay_capture::OverlayLaunchContext;
use crate::overlay_jobs::{OverlayJobFrame, OverlayJobOutcome, OverlayJobState};
use crate::pane_env::PaneEnv;
use crate::zoom::{SurfaceId, ZoomSurface};

fn overlay(fixture: &mut Fixture, cmd: SessionCmd, id: Uuid, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.session(cmd, Some(&wire(id)), args(edit))
}

fn open(fixture: &mut Fixture, id: Uuid, command: &str, edit: impl FnOnce(&mut Args)) -> Response {
    overlay(fixture, SessionCmd::OverlayOpen, id, |a| {
        a.command = Some(command.to_owned());
        edit(a);
    })
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

fn kinds(node: &Value) -> Vec<String> {
    node["surfaces"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|surface| surface["kind"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Installs a terminal in the session-wide slot, as the frontend does once the slot is open.
fn mount(fixture: &mut Fixture, id: Uuid) -> FakeSurfaceProbe {
    let surface = FakeSurface::new("");
    let probe = surface.probe();
    let session = fixture.store_mut().unwrap().session_mut(id).unwrap();
    session.overlay.surface.replace(Box::new(surface));
    probe
}

fn mount_pane(fixture: &mut Fixture, id: Uuid, pane: OverlayPane) -> FakeSurfaceProbe {
    let surface = FakeSurface::new("");
    let probe = surface.probe();
    let session = fixture.store_mut().unwrap().session_mut(id).unwrap();
    session.pane_overlays.slot_mut(pane).surface.replace(Box::new(surface));
    probe
}

/// Shows the split of session `id`, its right pane focused.
fn split(fixture: &mut Fixture, id: Uuid) {
    let store = fixture.store_mut().unwrap();
    store.set_split_visibility(id, true, None);
    let session = store.session_mut(id).unwrap();
    session.surface.replace(Box::new(FakeSurface::new("L")));
    session.split_surface.replace(Box::new(FakeSurface::new("R")));
}

/// Writes the exit status the wrapper would leave in the session-wide slot's file.
fn exit_with(fixture: &mut Fixture, id: Uuid, code: i32) {
    let file = fixture.store().session(id).unwrap().overlay.code_file.clone().unwrap();
    std::fs::write(file, format!("{code}\n")).unwrap();
}

fn result_code(response: &Response) -> Option<i64> {
    response.result.as_ref()?["exitCode"].as_i64()
}

#[test]
fn a_program_covers_the_session_and_closes() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    assert_eq!(answered_id(&open(&mut fixture, id, "exec cat", |_| {})), Some(id));
    let read = node(&mut fixture, id);
    assert_eq!(read["overlay"], json!(true));
    for absent in ["overlaySizePercent", "hud", "paneOverlays"] {
        assert!(read.get(absent).is_none(), "{absent}");
    }
    assert!(kinds(&read).contains(&"overlay".to_owned()));
    let active: Vec<&Value> = read["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|surface| surface["active"] == json!(true))
        .collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["kind"], "overlay", "the cover is the active surface");
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayResult, id, |_| {}),
        failure("overlay still running")
    );
    assert_eq!(open(&mut fixture, id, "true", |_| {}), failure("overlay already open"));
    assert!(overlay(&mut fixture, SessionCmd::OverlayClose, id, |_| {}).ok);
    let read = node(&mut fixture, id);
    assert_eq!(read["overlay"], json!(false));
    assert!(!kinds(&read).contains(&"overlay".to_owned()));
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayClose, id, |_| {}),
        failure("no overlay")
    );
}

#[test]
fn the_exit_status_lands_at_the_teardown_and_a_new_open_clears_it() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayResult, id, |_| {}),
        failure("no overlay result")
    );
    assert!(open(&mut fixture, id, "exit 7", |_| {}).ok);
    let probe = mount(&mut fixture, id);
    exit_with(&mut fixture, id, 7);
    let generation = fixture.store().session(id).unwrap().overlay.generation;
    assert!(!fixture.core.overlay_exited(id, generation + 1), "another generation");
    assert!(fixture.core.overlay_exited(id, generation));
    assert_eq!(probe.teardowns(), 1);
    let response = overlay(&mut fixture, SessionCmd::OverlayResult, id, |_| {});
    assert_eq!(answered_id(&response), Some(id));
    assert_eq!(result_code(&response), Some(7));

    assert!(open(&mut fixture, id, "exit 3", |_| {}).ok);
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayResult, id, |_| {}),
        failure("overlay still running")
    );
    exit_with(&mut fixture, id, 3);
    assert!(overlay(&mut fixture, SessionCmd::OverlayClose, id, |_| {}).ok);
    assert_eq!(
        result_code(&overlay(&mut fixture, SessionCmd::OverlayResult, id, |_| {})),
        Some(3)
    );
    // a program killed by its close writes nothing: the slot keeps no status
    assert!(open(&mut fixture, id, "exec cat", |_| {}).ok);
    assert!(overlay(&mut fixture, SessionCmd::OverlayClose, id, |_| {}).ok);
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayResult, id, |_| {}),
        failure("no overlay result")
    );
}

#[test]
fn the_program_launches_through_the_wrapper_with_its_command_in_the_environment() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    assert!(
        open(&mut fixture, id, "pwd; exec cat", |a| {
            a.cwd = Some("/srv".to_owned());
            a.wait = Some(true);
            a.color = Some("#203040".to_owned());
        })
        .ok
    );
    let session = fixture.store().session(id).unwrap();
    let slot = &session.overlay;
    assert_eq!(
        (slot.cwd.as_deref(), slot.wait, slot.background_color.as_deref()),
        (Some("/srv"), true, Some("#203040"))
    );
    let code_file = slot.code_file.clone().unwrap();
    let context = OverlayLaunchContext::new(slot.command.as_deref().unwrap(), "/srv", PaneEnv::new());
    let env = context.local_environment(&code_file, None);
    assert_eq!(env["AGTERM_OVL_CMD"], "pwd; exec cat");
    assert_eq!(env["AGTERM_OVL_CODE"], code_file.to_string_lossy());
}

/// Args from their wire JSON.
fn wire_args(value: Value) -> Args {
    serde_json::from_value(value).unwrap()
}

#[test]
fn open_sizes_outside_their_range_are_refused() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    for percent in [0, 101] {
        assert_eq!(
            open(&mut fixture, id, "exec cat", |a| a.size_percent = Some(percent)),
            failure("session.overlay.open: --size-percent must be 1...100")
        );
    }
    assert_eq!(node(&mut fixture, id)["overlay"], json!(false));
}

#[test]
fn resize_floats_or_covers_and_reads_back() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    let resize = |fixture: &mut Fixture, value: Value| {
        fixture.session(SessionCmd::OverlayResize, Some(&wire(id)), wire_args(value))
    };
    assert_eq!(resize(&mut fixture, json!({"sizePercent": 50})), failure("no overlay"));
    assert!(open(&mut fixture, id, "exec cat", |a| a.size_percent = Some(40)).ok);
    assert_eq!(node(&mut fixture, id)["overlaySizePercent"], 40);
    for (request, size) in [
        (json!({"sizePercent": 75}), Some(75)),
        (json!({"full": true}), None),
        (json!({"sizePercent": 1}), Some(1)),
    ] {
        assert_eq!(answered_id(&resize(&mut fixture, request)), Some(id));
        assert_eq!(
            node(&mut fixture, id).get("overlaySizePercent").cloned(),
            size.map(|size| json!(size))
        );
    }
    for (request, error) in [
        (
            json!({"full": true, "sizePercent": 50}),
            "session.overlay.resize: --full is mutually exclusive with --size-percent",
        ),
        (json!({}), "session.overlay.resize requires --size-percent or --full"),
        (
            json!({"sizePercent": 0}),
            "session.overlay.resize: --size-percent must be 1...100",
        ),
        (
            json!({"sizePercent": 101}),
            "session.overlay.resize: --size-percent must be 1...100",
        ),
        (
            json!({"sizePercent": 50, "pane": "left"}),
            "session.overlay.resize: --pane is not supported (pane overlays are always full)",
        ),
    ] {
        assert_eq!(resize(&mut fixture, request), failure(error));
    }
    assert_eq!(node(&mut fixture, id)["overlaySizePercent"], 1);
}

/// Each request is refused with its error and opens nothing.
fn assert_open_refusals(cases: &[(Value, &str)]) {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    for (request, error) in cases {
        assert_eq!(
            fixture.session(SessionCmd::OverlayOpen, Some(&wire(id)), wire_args(request.clone())),
            failure(error)
        );
    }
    let read = node(&mut fixture, id);
    assert_eq!(read["overlay"], json!(false));
    assert!(read.get("paneOverlays").is_none());
}

/// A program's and a URL page's flag combinations.
#[test]
fn open_refusals_name_the_flag_combination() {
    let url = "http://127.0.0.1:9/page.html";
    assert_open_refusals(&[
        (json!({}), "session.overlay.open requires a command"),
        (json!({"command": ""}), "session.overlay.open requires a command"),
        (
            json!({"command": "true", "url": url}),
            "session.overlay.open takes a command or --url, not both",
        ),
        (
            json!({"url": url, "wait": true}),
            "session.overlay.open: --wait cannot be combined with --url",
        ),
        (
            json!({"url": url, "cwd": "/tmp"}),
            "session.overlay.open: --cwd cannot be combined with --url",
        ),
        (
            json!({"url": "ftp://127.0.0.1/x"}),
            "session.overlay.open: --url must be an absolute http or https URL",
        ),
        (
            json!({"command": "true", "navigation": true}),
            "session.overlay.open: --navigation requires --html or --url",
        ),
        (
            json!({"command": "true", "javascript": true}),
            "session.overlay.open: --js requires --html or --url",
        ),
        (
            json!({"url": url, "chromeless": true}),
            "session.overlay.open: --chromeless requires --html",
        ),
        (
            json!({"command": "true", "color": "red"}),
            "invalid color: red (#rrggbb)",
        ),
        (
            json!({"command": "true", "pane": "scratch"}),
            "session.overlay: --pane must be left or right",
        ),
        (
            json!({"command": "true", "pane": "left", "sizePercent": 50}),
            "session.overlay.open: --pane is mutually exclusive with --size-percent",
        ),
        (
            json!({"command": "printf a\u{0}b"}),
            "session.overlay.open: the command must not contain a NUL byte",
        ),
        (
            json!({"command": "true", "pane": "left", "cwd": "/tmp\u{0}x"}),
            "session.overlay.open: --cwd must not contain a NUL byte",
        ),
    ]);
}

/// A file page's flags and its `--cwd` grant.
#[test]
fn open_refusals_check_a_file_page_and_its_grant() {
    let file = "/agterm-conformance/site/page.html";
    let url = "http://127.0.0.1:9/page.html";
    assert_open_refusals(&[
        (
            json!({"html": file, "url": url}),
            "session.overlay.open takes --html or --url, not both",
        ),
        (
            json!({"command": "true", "html": file}),
            "session.overlay.open takes a command or --html, not both",
        ),
        (
            json!({"html": file, "wait": true}),
            "session.overlay.open: --wait cannot be combined with --html",
        ),
        (
            json!({"html": file, "chromeless": true, "navigation": true}),
            "session.overlay.open: --chromeless cannot be combined with --navigation",
        ),
        (
            json!({"html": file, "persistent": true}),
            "session.overlay.open: --persistent requires --url",
        ),
        (
            json!({"html": file, "browse": true}),
            "session.overlay.open: --browse requires --url",
        ),
        (
            json!({"html": "site/page.html"}),
            "session.overlay.open: html file must be an absolute path",
        ),
        (
            json!({"html": file, "cwd": "/"}),
            "session.overlay.open: cwd must not be / or the home directory",
        ),
        (
            json!({"html": file, "cwd": "/home/u"}),
            "session.overlay.open: cwd must not be / or the home directory",
        ),
        (
            json!({"html": file, "cwd": file}),
            "session.overlay.open: cwd must be a directory containing the html file",
        ),
        (
            json!({"html": file, "cwd": "/other"}),
            "session.overlay.open: html file is outside cwd",
        ),
    ]);
}

#[test]
fn validation_runs_before_the_target_and_an_unknown_target_is_named() {
    let mut fixture = Fixture::new(1);
    let unknown = |cmd, edit: fn(&mut Args)| (cmd, args(edit));
    for (cmd, request) in [
        unknown(SessionCmd::OverlayOpen, |a| a.command = Some("exec cat".into())),
        unknown(SessionCmd::OverlayClose, |_| {}),
        unknown(SessionCmd::OverlayResize, |a| a.size_percent = Some(50)),
        unknown(SessionCmd::OverlayResult, |_| {}),
        unknown(SessionCmd::OverlayText, |_| {}),
        unknown(SessionCmd::OverlaySubmit, |a| a.value = Some("x".into())),
        unknown(SessionCmd::OverlayReload, |_| {}),
        unknown(SessionCmd::OverlayCopy, |_| {}),
    ] {
        assert_eq!(
            fixture.session(cmd, Some(UNKNOWN), request),
            failure(&format!("no such session: {UNKNOWN}")),
            "{cmd:?}"
        );
    }
    assert_eq!(
        fixture.session(SessionCmd::OverlayOpen, Some(UNKNOWN), Args::default()),
        failure("session.overlay.open requires a command")
    );
    for cmd in [
        SessionCmd::OverlayClose,
        SessionCmd::OverlayResult,
        SessionCmd::OverlayText,
        SessionCmd::OverlayReload,
        SessionCmd::OverlayCopy,
    ] {
        assert_eq!(
            fixture.session(cmd, Some(UNKNOWN), args(|a| a.pane = Some("scratch".into()))),
            failure("session.overlay: --pane must be left or right")
        );
    }
}

#[test]
fn follow_selects_and_a_plain_open_leaves_the_selection() {
    let mut fixture = Fixture::new(2);
    let (first, second) = (fixture.sessions[0], fixture.sessions[1]);
    fixture.store_mut().unwrap().select_session(Some(first));
    assert!(open(&mut fixture, second, "exec cat", |_| {}).ok);
    assert_eq!(fixture.selected(), Some(first));
    assert!(overlay(&mut fixture, SessionCmd::OverlayClose, second, |_| {}).ok);
    assert!(open(&mut fixture, second, "exec cat", |a| a.follow = Some(true)).ok);
    assert_eq!(fixture.selected(), Some(second));
}

#[test]
fn text_and_copy_read_the_covering_program() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayText, id, |_| {}),
        failure("no overlay")
    );
    assert!(open(&mut fixture, id, "exec cat", |_| {}).ok);
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayText, id, |_| {}),
        failure("overlay not realized")
    );
    let probe = mount(&mut fixture, id);
    probe.set_lines(["OVL-1", "OVL-2", "", ""]);
    let text = overlay(&mut fixture, SessionCmd::OverlayText, id, |_| {});
    assert_eq!(text.result.unwrap()["text"], "OVL-1\nOVL-2\n\n");
    let last = overlay(&mut fixture, SessionCmd::OverlayText, id, |a| a.lines = Some(1));
    assert_eq!(last.result.unwrap()["text"], "OVL-2");
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayText, id, |a| {
            a.all = Some(true);
            a.lines = Some(2);
        }),
        failure("use either --all or --lines, not both")
    );
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayText, id, |a| a.lines = Some(0)),
        failure("--lines must be greater than 0")
    );
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayCopy, id, |_| {}),
        failure("no selection")
    );
    probe.set_selection(Some("picked"));
    let copied = overlay(&mut fixture, SessionCmd::OverlayCopy, id, |_| {});
    assert_eq!(copied.result.unwrap(), json!({"text": "picked"}));
}

/// Installs the HUD painter.
fn with_hud_helper(fixture: &mut Fixture) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("hud.sh");
    std::fs::write(&helper, "#!/bin/sh\n").unwrap();
    fixture.core.set_hud_helper(helper, dir.path().join("bodies"));
    dir
}

fn open_hud(fixture: &mut Fixture, id: Uuid) -> Response {
    overlay(fixture, SessionCmd::HudOpen, id, |a| a.message = Some("working".into()))
}

#[test]
fn a_hud_is_refused_by_the_reads_and_a_full_resize() {
    let mut fixture = Fixture::new(1);
    let _dir = with_hud_helper(&mut fixture);
    let id = fixture.sessions[0];
    assert!(open_hud(&mut fixture, id).ok);
    for (cmd, error) in [
        (SessionCmd::OverlayResult, "no overlay result: the slot holds a hud"),
        (SessionCmd::OverlayText, "no overlay to read: the slot holds a hud"),
        (SessionCmd::OverlayCopy, "no overlay to read: the slot holds a hud"),
        (SessionCmd::OverlaySubmit, "no overlay"),
    ] {
        assert_eq!(
            overlay(&mut fixture, cmd, id, |a| a.value = Some("x".into())),
            failure(error),
            "{cmd:?}"
        );
    }
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayResize, id, |a| a.full = Some(true)),
        failure("a hud is always floating: pass --size-percent, not --full")
    );
    assert!(
        overlay(&mut fixture, SessionCmd::OverlayResize, id, |a| a.size_percent =
            Some(50))
        .ok
    );
    assert!(
        !kinds(&node(&mut fixture, id)).contains(&"overlay".to_owned()),
        "a hud is not addressable"
    );
}

#[test]
fn a_program_and_a_hud_share_the_slot() {
    let mut fixture = Fixture::new(1);
    let _dir = with_hud_helper(&mut fixture);
    let id = fixture.sessions[0];
    assert!(open(&mut fixture, id, "exec cat", |_| {}).ok);
    assert_eq!(open_hud(&mut fixture, id), failure("overlay already open"));
    assert!(overlay(&mut fixture, SessionCmd::OverlayClose, id, |_| {}).ok);
    assert!(open_hud(&mut fixture, id).ok);
    // a program replaces the HUD
    assert!(open(&mut fixture, id, "exec cat", |_| {}).ok);
    let read = node(&mut fixture, id);
    assert!(read.get("hud").is_none());
    assert_eq!(read["overlay"], json!(true));
    // the close courtesy closes a HUD too
    assert!(overlay(&mut fixture, SessionCmd::OverlayClose, id, |_| {}).ok);
    assert!(open_hud(&mut fixture, id).ok);
    assert!(overlay(&mut fixture, SessionCmd::OverlayClose, id, |_| {}).ok);
    assert!(node(&mut fixture, id).get("hud").is_none());
}

#[test]
fn page_commands_need_a_page_and_a_build_without_web_views_refuses_pages() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    for cmd in [SessionCmd::OverlayReload, SessionCmd::OverlaySubmit] {
        assert_eq!(
            overlay(&mut fixture, cmd, id, |a| a.value = Some("x".into())),
            failure("no overlay")
        );
    }
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlaySubmit, id, |_| {}),
        failure("session.overlay.submit requires a value")
    );
    for to in [None, Some("sideways")] {
        assert_eq!(
            overlay(&mut fixture, SessionCmd::OverlayNavigate, id, |a| a.to =
                to.map(str::to_owned)),
            failure("session.overlay.navigate requires back, forward, browser or finder")
        );
    }
    assert!(open(&mut fixture, id, "exec cat", |_| {}).ok);
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayNavigate, id, |a| a.to =
            Some("back".into())),
        failure("the overlay is not an html page")
    );
    for (page, error) in [
        ("nope", "invalid page id"),
        ("6F9619FF-8B86-D011-B42D-00C04FC964FF", "no such page"),
    ] {
        assert_eq!(
            fixture.session(SessionCmd::OverlayResult, None, args(|a| a.page = Some(page.into()))),
            failure(error)
        );
    }
    let page = args(|a| a.html = Some("/tmp/page.html".into()));
    assert!(overlay(&mut fixture, SessionCmd::OverlayClose, id, |_| {}).ok);
    assert_eq!(
        fixture.session(SessionCmd::OverlayOpen, Some(&wire(id)), page),
        failure("session.overlay.open: html overlays are not available in this build")
    );
}

/// The generation of the overlay over `pane`.
fn pane_generation(fixture: &Fixture, id: Uuid, pane: OverlayPane) -> u64 {
    fixture
        .store()
        .session(id)
        .unwrap()
        .pane_overlay(pane)
        .unwrap()
        .generation
}

/// Sets `--pane`.
fn on(pane: &'static str) -> impl FnOnce(&mut Args) {
    move |a: &mut Args| a.pane = Some(pane.to_owned())
}

#[test]
fn pane_overlays_cover_one_rendered_pane_each() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    assert_eq!(
        open(&mut fixture, id, "exec cat", on("right")),
        failure("pane not visible")
    );
    split(&mut fixture, id);
    assert!(open(&mut fixture, id, "exec cat", on("split")).ok);
    let read = node(&mut fixture, id);
    assert_eq!(read["paneOverlays"], json!(["right"]));
    assert_eq!(read["overlay"], json!(false));
    assert!(kinds(&read).contains(&"overlay-right".to_owned()));
    assert!(!kinds(&read).contains(&"overlay-left".to_owned()));
    assert_eq!(
        open(&mut fixture, id, "exec cat", on("right")),
        failure("pane overlay already open")
    );
    assert!(open(&mut fixture, id, "exec cat", on("left")).ok);
    assert_eq!(node(&mut fixture, id)["paneOverlays"], json!(["left", "right"]));
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayResult, id, on("right")),
        failure("overlay still running")
    );
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayText, id, |_| {}),
        failure("no overlay")
    );
    let probe = mount_pane(&mut fixture, id, OverlayPane::Right);
    probe.set_lines(["PANE-RIGHT"]);
    let text = overlay(&mut fixture, SessionCmd::OverlayText, id, on("right"));
    assert_eq!(text.result.unwrap()["text"], "PANE-RIGHT");
}

#[test]
fn a_pane_overlay_closes_alone() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    split(&mut fixture, id);
    assert!(open(&mut fixture, id, "exec cat", on("left")).ok);
    assert!(open(&mut fixture, id, "exec cat", on("right")).ok);
    assert!(overlay(&mut fixture, SessionCmd::OverlayClose, id, on("left")).ok);
    assert_eq!(node(&mut fixture, id)["paneOverlays"], json!(["right"]));
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayClose, id, on("left")),
        failure("no overlay")
    );
}

#[test]
fn a_pane_overlay_records_its_status_when_its_program_exits() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    split(&mut fixture, id);
    assert!(open(&mut fixture, id, "exit 5", on("right")).ok);
    let probe = mount_pane(&mut fixture, id, OverlayPane::Right);
    let file = fixture
        .store()
        .session(id)
        .unwrap()
        .pane_overlay(OverlayPane::Right)
        .unwrap()
        .code_file
        .clone();
    std::fs::write(file.unwrap(), "5").unwrap();
    let generation = pane_generation(&fixture, id, OverlayPane::Right);
    assert!(fixture.core.pane_overlay_exited(id, generation));
    assert!(!fixture.core.pane_overlay_exited(id, generation), "once");
    assert_eq!(probe.teardowns(), 1);
    assert!(node(&mut fixture, id).get("paneOverlays").is_none());
    let result = overlay(&mut fixture, SessionCmd::OverlayResult, id, on("right"));
    assert_eq!(result_code(&result), Some(5));
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayResult, id, on("left")),
        failure("no overlay result")
    );
}

#[test]
fn a_pane_overlay_takes_the_active_surface_of_its_focused_pane() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    split(&mut fixture, id);
    assert!(open(&mut fixture, id, "exec cat", |a| a.pane = Some("right".into())).ok);
    fixture.store_mut().unwrap().select_session(Some(id));
    assert_eq!(
        SurfaceId::active_in(fixture.store()).map(|surface| surface.surface),
        Some(ZoomSurface::OverlayRight)
    );
    // the focus moving to the covered pane's sibling makes that pane active again
    let generation = pane_generation(&fixture, id, OverlayPane::Right);
    fixture.store_mut().unwrap().set_split_focus(id, false);
    assert_eq!(
        SurfaceId::active_in(fixture.store()).map(|surface| surface.surface),
        Some(ZoomSurface::Primary)
    );
    assert!(fixture.core.pane_overlay_focused(id, generation));
    assert!(
        fixture.store().session(id).unwrap().split.focused,
        "clicking the overlay focuses its pane"
    );
}

#[test]
fn pane_overlays_follow_their_panes() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    split(&mut fixture, id);
    assert!(open(&mut fixture, id, "exec cat", |a| a.pane = Some("right".into())).ok);
    let generation = pane_generation(&fixture, id, OverlayPane::Right);
    fixture.store_mut().unwrap().swap_panes(id).unwrap();
    assert_eq!(node(&mut fixture, id)["paneOverlays"], json!(["left"]));
    fixture.store_mut().unwrap().swap_panes(id).unwrap();
    // the primary's shell exits: the survivor's overlay moves left with it
    fixture.store_mut().unwrap().close_primary_pane(id);
    let session = fixture.store().session(id).unwrap();
    assert_eq!(session.pane_overlays.pane_of(generation), Some(OverlayPane::Left));
    assert!(session.pane_slot(StatusPane::Left).is_present());
    // closing the split takes the right pane's overlay with it
    split(&mut fixture, id);
    assert!(open(&mut fixture, id, "exec cat", |a| a.pane = Some("right".into())).ok);
    fixture.store_mut().unwrap().close_split(id);
    assert_eq!(node(&mut fixture, id)["paneOverlays"], json!(["left"]));
}

#[test]
fn an_overlay_exit_during_a_pending_close_lands_on_the_session_an_undo_restores() {
    let mut fixture = Fixture::new(2);
    let id = fixture.sessions[0];
    split(&mut fixture, id);
    assert!(open(&mut fixture, id, "exit 5", |_| {}).ok);
    let program = mount(&mut fixture, id);
    assert!(open(&mut fixture, id, "exec cat", |a| a.pane = Some("right".into())).ok);
    let pane = mount_pane(&mut fixture, id, OverlayPane::Right);
    let generation = fixture.store().session(id).unwrap().overlay.generation;
    let pane_generation = pane_generation(&fixture, id, OverlayPane::Right);
    exit_with(&mut fixture, id, 5);
    assert!(fixture.store_mut().unwrap().soft_close_session(id));
    // both programs end inside the grace period, their terminals still running in the pending close
    assert!(fixture.core.overlay_exited(id, generation));
    assert!(fixture.core.pane_overlay_exited(id, pane_generation));
    assert_eq!((program.teardowns(), pane.teardowns()), (1, 1));
    assert!(fixture.store_mut().unwrap().undo_pending_close(None));
    let response = overlay(&mut fixture, SessionCmd::OverlayResult, id, |_| {});
    assert_eq!(result_code(&response), Some(5), "{response:?}");
    assert!(node(&mut fixture, id).get("paneOverlays").is_none());
}

#[test]
fn a_stranded_pane_overlay_goes_at_the_commit() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    split(&mut fixture, id);
    assert!(open(&mut fixture, id, "exec cat", |a| a.pane = Some("left".into())).ok);
    let zoom = SurfaceId {
        session: id,
        surface: ZoomSurface::OverlayLeft,
    };
    fixture.core.set_zoom_target(fixture.window, Some(zoom));
    let _ = fixture.core.commit();
    // hide the split behind the focused right pane before the overlay realizes: the left pane is no longer laid out
    fixture.store_mut().unwrap().set_split_visibility(id, false, None);
    let _ = fixture.core.commit();
    assert_eq!(
        node(&mut fixture, id)["paneOverlays"],
        json!(["left"]),
        "the zoom hosts it"
    );
    fixture.core.set_zoom_target(fixture.window, None);
    let _ = fixture.core.commit();
    assert!(node(&mut fixture, id).get("paneOverlays").is_none());
}

#[test]
fn the_dashboard_names_what_covers_a_member_pane() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    split(&mut fixture, id);
    let cover = |fixture: &Fixture, pane| fixture.store().session(id).unwrap().dashboard_cover(pane);
    assert_eq!(cover(&fixture, OverlayPane::Left), None);
    assert!(open(&mut fixture, id, "htop", |a| a.pane = Some("right".into())).ok);
    assert_eq!(
        cover(&fixture, OverlayPane::Right),
        Some(DashboardCover::Program {
            command: Some("htop".into())
        })
    );
    assert!(open(&mut fixture, id, "top", |a| a.size_percent = Some(50)).ok);
    assert_eq!(
        cover(&fixture, OverlayPane::Left),
        None,
        "a floating overlay covers nothing"
    );
    assert!(overlay(&mut fixture, SessionCmd::OverlayResize, id, |a| a.full = Some(true)).ok);
    assert_eq!(
        cover(&fixture, OverlayPane::Left),
        Some(DashboardCover::Program {
            command: Some("top".into())
        })
    );
}

// MARK: - jobs

/// A helper connection that records what the origin sends.
#[derive(Clone, Default)]
struct Recorded {
    lines: Rc<RefCell<Vec<OverlayJobFrame>>>,
    closed: Rc<RefCell<bool>>,
}

impl JobChannel for Recorded {
    fn send(&self, line: Vec<u8>) -> bool {
        self.lines.borrow_mut().push(OverlayJobFrame::parse(&line).unwrap());
        true
    }

    fn shutdown(&self) {
        *self.closed.borrow_mut() = true;
    }
}

fn claim(fixture: &mut Fixture, job: Option<&str>) -> Response {
    fixture.session(SessionCmd::OverlayJobRun, job, Args::default())
}

#[test]
fn a_job_claim_is_validated_then_single_use() {
    let mut fixture = Fixture::new(0);
    assert_eq!(
        claim(&mut fixture, None),
        failure("session.overlay.job.run requires a job id")
    );
    assert_eq!(
        claim(&mut fixture, Some("  ")),
        failure("session.overlay.job.run requires a job id")
    );
    assert_eq!(claim(&mut fixture, Some(UNKNOWN)), failure("invalid job id"));
    assert_eq!(
        claim(&mut fixture, Some("00000000-0000-0000-0000-000000000000")),
        failure("job not claimable")
    );
    let context = OverlayLaunchContext::new("true", "/", PaneEnv::new());
    let job = fixture
        .core
        .jobs
        .jobs
        .register(Uuid::new_v4(), None, 1, context.clone());
    let claimed = claim(&mut fixture, Some(&job));
    assert_eq!(claimed.result.unwrap(), json!({"id": job}));
    assert_eq!(claim(&mut fixture, Some(&job)), failure("job not claimable"));
    assert_eq!(
        fixture.core.take_overlay_job_expiries().len(),
        1,
        "the start window is armed"
    );

    let helper = Recorded::default();
    let serial = fixture.core.adopt_overlay_job_stream(&job, Box::new(helper.clone()));
    assert_eq!(*helper.lines.borrow(), vec![OverlayJobFrame::Context { context }]);
    fixture.core.overlay_job_line(&job, serial, br#"{"kind":"started"}"#);
    assert_eq!(
        fixture.core.jobs.jobs.job(&job).unwrap().state,
        OverlayJobState::Running
    );
    assert!(fixture.core.cancel_overlay_job(&job));
    assert_eq!(helper.lines.borrow().last(), Some(&OverlayJobFrame::Cancel));
    fixture
        .core
        .overlay_job_line(&job, serial + 1, br#"{"kind":"exited","code":1}"#);
    assert_eq!(
        fixture.core.jobs.jobs.job(&job).unwrap().state,
        OverlayJobState::Running,
        "a stale stream"
    );
    fixture
        .core
        .overlay_job_line(&job, serial, br#"{"kind":"exited","code":2}"#);
    fixture.core.overlay_job_closed(&job, serial);
    assert_eq!(
        fixture.core.jobs.jobs.job(&job).unwrap().state,
        OverlayJobState::Finished(OverlayJobOutcome::Exited(2)),
        "the first outcome holds"
    );
}

#[test]
fn a_job_cancelled_before_its_connection_gets_the_cancel_after_the_context() {
    let mut fixture = Fixture::new(0);
    let context = OverlayLaunchContext::new("true", "/", PaneEnv::new());
    let job = fixture
        .core
        .jobs
        .jobs
        .register(Uuid::new_v4(), None, 1, context.clone());
    assert!(claim(&mut fixture, Some(&job)).ok);
    assert!(fixture.core.cancel_overlay_job(&job));
    let helper = Recorded::default();
    let serial = fixture.core.adopt_overlay_job_stream(&job, Box::new(helper.clone()));
    assert_eq!(
        *helper.lines.borrow(),
        vec![OverlayJobFrame::Context { context }, OverlayJobFrame::Cancel]
    );
    fixture.core.overlay_job_line(&job, serial, b"not json");
    assert!(*helper.closed.borrow(), "a bad frame closes the connection");
    fixture.core.overlay_job_closed(&job, serial);
    assert_eq!(
        fixture.core.jobs.jobs.job(&job).unwrap().state,
        OverlayJobState::Finished(OverlayJobOutcome::Unknown)
    );
    // a job over by its adoption never launches
    let late = Recorded::default();
    fixture.core.adopt_overlay_job_stream(&job, Box::new(late.clone()));
    assert!(late.lines.borrow().is_empty() && *late.closed.borrow());
}
