//! Remote sessions, origin side: `zmx.tree`, the `zmx.present` stream and its hub, and the asks and overlays handed to
//! a presenter (upstream's `ControlServer+Zmx` remote half, `ControlServer+Presentation`, `AppStoreRemoteAskTests`,
//! `AppStoreRemoteOverlayTests` and `ControlServerPresentationTests`), over a [`FakeSessionHost`] and fake streams.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agterm_proto::command::{AskCmd, SessionCmd, ZmxCmd};
use agterm_proto::{Args, AskButton, Command, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::zmx::{Live, live, report, spawn_live};
use super::{Fixture, UNKNOWN, args, failure, wire};
use crate::Core;
use crate::remote::PresentationChannel;
use crate::remote::tree::{RemoteCommandResult, RemoteCommandRunner};
use crate::zmx::daemon_name;

// MARK: - a fake stream

#[derive(Default)]
struct Seen {
    lines: Vec<Value>,
    closed: bool,
}

/// A presentation connection the test reads what the core sent on.
#[derive(Clone, Default)]
struct Viewer(Rc<RefCell<Seen>>);

impl PresentationChannel for Viewer {
    fn send(&self, line: Vec<u8>) -> bool {
        let mut seen = self.0.borrow_mut();
        if seen.closed {
            return false;
        }
        seen.lines.push(serde_json::from_slice(&line).unwrap());
        true
    }

    fn shutdown(&self) {
        self.0.borrow_mut().closed = true;
    }
}

impl Viewer {
    fn kinds(&self) -> Vec<String> {
        self.0
            .borrow()
            .lines
            .iter()
            .map(|frame| frame["kind"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn last(&self, kind: &str) -> Option<Value> {
        self.0
            .borrow()
            .lines
            .iter()
            .rev()
            .find(|frame| frame["kind"] == kind)
            .cloned()
    }

    fn closed(&self) -> bool {
        self.0.borrow().closed
    }

    fn generation(&self) -> i64 {
        self.last("hello").and_then(|hello| hello["gen"].as_i64()).unwrap()
    }
}

fn hello(mode: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"kind": "hello", "gen": 0, "rev": 0,
        "hello": {"version": 1, "kinds": ["status", "hud", "notify", "context", "layout"], "mode": mode}}))
    .unwrap()
}

/// Opens `zmx.present` on `session`, adopts its stream and says hello as `mode`.
fn subscribe(core: &mut Core, session: Uuid, mode: &str) -> (u64, Viewer) {
    let response = core.dispatch(&agterm_proto::Request {
        cmd: Command::Zmx(ZmxCmd::Present),
        target: Some(wire(session)),
        args: None,
    });
    assert_eq!(response.result, Some(json!({"id": wire(session)})), "{response:?}");
    let viewer = Viewer::default();
    let serial = core.adopt_presentation_stream(&wire(session), Box::new(viewer.clone()));
    core.presentation_line(serial, &hello(mode));
    core.commit();
    (serial, viewer)
}

fn frame(viewer: &Viewer, body: &Value) -> Vec<u8> {
    let mut frame = body.clone();
    frame["gen"] = json!(viewer.generation());
    frame["rev"] = json!(1);
    serde_json::to_vec(&frame).unwrap()
}

/// A live session whose primary pane follows a remote lead, and a presenter holding it.
fn presented() -> (Live, u64, Viewer) {
    let mut live = live();
    let token = crate::pane_env::uuid_string(live.left);
    let nonce = spawn_live(&mut live);
    assert!(report(&mut live, &token, &nonce, "follower", 1));
    let session = live.session;
    let (serial, presenter) = subscribe(&mut live.fixture.core, session, "presenter");
    let acquire = frame(&presenter, &json!({"kind": "presenter.acquire"}));
    live.fixture.core.presentation_line(serial, &acquire);
    live.fixture.core.commit();
    assert_eq!(presenter.kinds().last().map(String::as_str), Some("presenter.granted"));
    (live, serial, presenter)
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

// MARK: - zmx.present

#[test]
fn present_refuses_before_streaming() {
    let mut fixture = Fixture::new(0);
    let session = fixture.sessions[0];
    let present = |fixture: &mut Fixture, target: Option<&str>| {
        fixture.send(Command::Zmx(ZmxCmd::Present), target, Args::default())
    };
    assert_eq!(present(&mut fixture, None), failure("zmx.present requires a session"));
    assert_eq!(
        present(&mut fixture, Some("  ")),
        failure("zmx.present requires a session")
    );
    assert_eq!(present(&mut fixture, Some("a b")), failure("invalid session"));
    assert_eq!(
        present(&mut fixture, Some(UNKNOWN)),
        failure(&format!("no such session: {UNKNOWN}"))
    );
    assert_eq!(
        present(&mut fixture, Some(&wire(session))),
        failure("session is not live-backed, so nothing can be attached to it")
    );
}

#[test]
fn a_viewer_gets_hello_then_the_snapshot_then_each_change() {
    let mut live = live();
    let session = live.session;
    let (serial, viewer) = subscribe(&mut live.fixture.core, session, "mirror");
    assert_eq!(viewer.kinds(), ["hello", "snapshot"]);
    let answer = viewer.last("hello").unwrap();
    assert_eq!(answer["hello"]["mode"], "mirror");
    assert_eq!(answer["rev"], 0);
    let snapshot = viewer.last("snapshot").unwrap()["snapshot"].clone();
    assert_eq!(
        snapshot["layout"],
        json!({"panes": [wire(live.left)], "primary": wire(live.left), "shown": false})
    );
    assert!(snapshot.get("status").is_none(), "idle travels as no status");
    assert_eq!(node(&mut live.fixture, session)["presenters"], json!({"mirrors": 1}));
    publish_changes(&mut live, &viewer);
    let revisions: Vec<i64> = viewer
        .0
        .borrow()
        .lines
        .iter()
        .map(|frame| frame["rev"].as_i64().unwrap())
        .collect();
    assert_eq!(revisions, (0..5).collect::<Vec<_>>(), "monotonic within the generation");
    // an unchanged commit publishes nothing
    live.fixture.core.commit();
    assert_eq!(viewer.kinds().len(), 5);

    live.fixture.core.presentation_closed(serial);
    assert!(node(&mut live.fixture, session).get("presenters").is_none());
}

/// A status, a context and a notification, each checked as it reaches `viewer`.
fn publish_changes(live: &mut Live, viewer: &Viewer) {
    let sid = wire(live.session);
    assert!(
        live.fixture
            .session(
                SessionCmd::Status,
                Some(&sid),
                args(|a| a.status = Some("active".to_owned()))
            )
            .ok
    );
    live.fixture.core.commit();
    let status = viewer.last("status").unwrap();
    assert_eq!(status["status"]["status"], "active");
    assert_eq!(
        status["status"]["pane"],
        json!(wire(live.left)),
        "the primary pane by identity"
    );
    assert!(status["status"]["changedAt"].is_f64());
    let context = live.fixture.session(
        SessionCmd::Context,
        Some(&sid),
        args(|a| {
            a.mode = Some("set".to_owned());
            a.text = Some("PR 517".to_owned());
        }),
    );
    assert!(context.ok, "{context:?}");
    live.fixture.core.commit();
    assert_eq!(viewer.last("context").unwrap()["context"], "PR 517");
    let notify = live.fixture.send(
        Command::Notify,
        Some(&sid),
        args(|a| {
            a.title = Some("build".to_owned());
            a.body = Some("done".to_owned());
        }),
    );
    assert!(notify.ok);
    assert_eq!(
        viewer.last("notify").unwrap()["notify"],
        json!({"title": "build", "body": "done", "source": "control"})
    );
}

#[test]
fn a_stream_that_does_not_open_with_hello_is_closed() {
    let mut live = live();
    let session = live.session;
    let core = &mut live.fixture.core;
    let ping = br#"{"kind":"ping","gen":0,"rev":0}"#;
    for line in [
        &ping[..],
        b"not json",
        br#"{"kind":"hello","gen":0,"rev":0,"hello":{"version":0,"kinds":[],"mode":"mirror"}}"#,
    ] {
        let viewer = Viewer::default();
        let serial = core.adopt_presentation_stream(&wire(session), Box::new(viewer.clone()));
        core.presentation_line(serial, line);
        assert!(viewer.closed(), "{}", String::from_utf8_lossy(line));
        assert!(viewer.kinds().is_empty());
        core.presentation_closed(serial);
    }
    let silent = Viewer::default();
    let serial = core.adopt_presentation_stream(&wire(session), Box::new(silent.clone()));
    assert!(core.presentation_heartbeat(), "a stream is up");
    core.presentation_hello_deadline(serial);
    assert!(silent.closed(), "no hello within the deadline");
    core.presentation_closed(serial);
    assert!(!core.has_presentation_streams());
}

#[test]
fn the_heartbeat_pings_and_a_gone_session_drops_its_streams() {
    let mut live = live();
    let session = live.session;
    let (_, viewer) = subscribe(&mut live.fixture.core, session, "mirror");
    assert!(live.fixture.core.presentation_heartbeat());
    assert_eq!(viewer.kinds().last().map(String::as_str), Some("ping"));
    live.fixture.store_mut().unwrap().close_session(session);
    live.fixture.core.commit();
    assert!(viewer.closed(), "its session left every open window");
}

#[test]
fn a_hud_is_published_with_its_generation_and_withdrawn_once() {
    let mut live = live();
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("hud.sh");
    std::fs::write(&helper, "#!/bin/sh\n").unwrap();
    live.fixture.core.set_hud_helper(helper, dir.path().join("bodies"));
    let session = live.session;
    let sid = wire(session);
    let (_, viewer) = subscribe(&mut live.fixture.core, session, "mirror");
    let open = live.fixture.session(
        SessionCmd::HudOpen,
        Some(&sid),
        args(|a| {
            a.message = Some("deploying".to_owned());
            a.hide_after = Some(30.0);
        }),
    );
    assert!(open.ok, "{open:?}");
    live.fixture.core.commit();
    let hud = viewer.last("hud").unwrap()["hud"].clone();
    assert_eq!(hud["spec"]["message"], "deploying");
    assert_eq!(hud["generation"], 1);
    assert!(
        hud["remaining"]
            .as_f64()
            .is_some_and(|remaining| remaining > 29.0 && remaining <= 30.0)
    );
    assert!(
        live.fixture
            .session(SessionCmd::HudClose, Some(&sid), Args::default())
            .ok
    );
    live.fixture.core.commit();
    live.fixture.core.commit();
    let withdrawals = viewer
        .0
        .borrow()
        .lines
        .iter()
        .filter(|frame| frame["kind"] == "hud" && frame.get("hud").is_none())
        .count();
    assert_eq!(withdrawals, 1);
    // a later viewer starts without the withdrawn panel
    let (_, later) = subscribe(&mut live.fixture.core, session, "mirror");
    assert!(later.last("snapshot").unwrap()["snapshot"].get("hud").is_none());
}

// MARK: - zmx.tree

/// Answers every remote run with one result, remembering the argv.
struct FakeRunner {
    result: RemoteCommandResult,
    argv: Mutex<Vec<String>>,
}

impl RemoteCommandRunner for FakeRunner {
    fn run(&self, argv: &[String], deadline: Duration) -> RemoteCommandResult {
        assert_eq!(deadline, crate::remote::tree::TREE_DEADLINE);
        *self.argv.lock().unwrap() = argv.to_vec();
        self.result.clone()
    }
}

fn runner(status: i32, stdout: &str, stderr: &str) -> Arc<FakeRunner> {
    Arc::new(FakeRunner {
        result: RemoteCommandResult {
            status,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        },
        argv: Mutex::new(Vec::new()),
    })
}

fn tree(core: &mut Core, host: Option<&str>) -> Response {
    core.dispatch(&agterm_proto::Request {
        cmd: Command::Zmx(ZmxCmd::Tree),
        target: None,
        args: Some(args(|a| a.host = host.map(str::to_owned))),
    })
}

#[test]
fn the_local_tree_offers_live_sessions_whose_daemons_run() {
    let mut fixture = Fixture::new(0);
    assert_eq!(
        tree(&mut fixture.core, None),
        failure("zmx is unavailable in this instance")
    );
    let mut live = live();
    let empty = tree(&mut live.fixture.core, None);
    assert_eq!(
        empty.result.as_ref().unwrap()["remote"]["sessions"],
        json!([]),
        "no daemon runs yet"
    );
    assert_eq!(empty.result.as_ref().unwrap()["remote"]["presentation"], 1);
    let daemon = daemon_name(live.left);
    live.host.set_record(&daemon, Some(1), Some(42), None);
    let offered = tree(&mut live.fixture.core, None).result.unwrap()["remote"].clone();
    assert_eq!(offered["sessions"][0]["id"], json!(wire(live.session)));
    assert_eq!(
        offered["sessions"][0]["panes"],
        json!([{"pane": "left", "daemon": daemon}])
    );
    assert_eq!(offered["endpoint"]["socketDirectory"], "/tmp/agterm-zmx-test");
    assert!(offered.get("host").is_none());
    live.host.fail_list("boom");
    assert_eq!(
        tree(&mut live.fixture.core, None),
        failure("could not read the zmx session list")
    );
}

#[test]
fn the_remote_tree_runs_the_bare_form_over_ssh_and_stamps_the_host() {
    let mut fixture = Fixture::new(0);
    let remote = json!({"endpoint": {"executable": "/z", "socketDirectory": "/t"}, "sessions": [], "presentation": 1});
    let ok = runner(0, &json!({"ok": true, "result": {"remote": remote}}).to_string(), "");
    fixture.core.set_remote_runner(ok.clone());
    let answer = tree(&mut fixture.core, Some(" buildbox "));
    assert_eq!(answer.result.unwrap()["remote"]["host"], "buildbox");
    assert_eq!(ok.argv.lock().unwrap()[6], "buildbox");
    assert_eq!(
        tree(&mut fixture.core, Some("-oProxyCommand=x")),
        failure("invalid host")
    );
    assert_eq!(tree(&mut fixture.core, Some("a\u{1b}b")), failure("invalid host"));
    for (status, stdout, stderr, error) in [
        (
            1,
            r#"{"ok":false,"error":"zmx is unavailable in this instance"}"#,
            "x",
            "zmx is unavailable in this instance",
        ),
        (
            255,
            "",
            "ssh: connect to host buildbox: refused\n",
            "ssh: connect to host buildbox: refused",
        ),
        (1, "", "", "the remote command failed on buildbox"),
        (
            0,
            "command not found",
            "",
            "the remote did not return a readable session list",
        ),
        (
            0,
            r#"{"ok":true}"#,
            "",
            "the remote did not return a readable session list",
        ),
    ] {
        fixture.core.set_remote_runner(runner(status, stdout, stderr));
        assert_eq!(
            tree(&mut fixture.core, Some("buildbox")),
            failure(error),
            "{status} {stdout:?}"
        );
    }
}

// MARK: - asks handed to the presenter

fn ask(core: &mut Core, session: Uuid, style: &str) -> Response {
    core.dispatch(&agterm_proto::Request {
        cmd: Command::Ask(AskCmd::Open),
        target: Some(wire(session)),
        args: Some(args(|a| {
            a.title = Some("deploy?".to_owned());
            a.style = Some(style.to_owned());
            a.buttons = Some(vec![
                AskButton {
                    id: "y".to_owned(),
                    label: "Yes".to_owned(),
                    hotkey: None,
                },
                AskButton {
                    id: "n".to_owned(),
                    label: "No".to_owned(),
                    hotkey: None,
                },
            ]);
        })),
    })
}

fn ask_result(core: &mut Core, id: &str) -> Value {
    let response = core.dispatch(&agterm_proto::Request {
        cmd: Command::Ask(AskCmd::Result),
        target: Some(id.to_owned()),
        args: None,
    });
    response.result.unwrap()["ask"].clone()
}

#[test]
fn an_ask_on_a_presented_session_goes_to_the_presenter_alone() {
    let (mut live, serial, presenter) = presented();
    let session = live.session;
    let opened = ask(&mut live.fixture.core, session, "terminal");
    let id = opened.result.unwrap()["id"].as_str().unwrap().to_owned();
    live.fixture.core.commit();
    let request = presenter.last("ask.request").unwrap()["ask"].clone();
    assert_eq!(
        (request["id"].as_str(), request["owner"].as_i64()),
        (Some(id.as_str()), Some(1))
    );
    assert_eq!(request["buttons"][1]["label"], "No");
    assert_eq!(
        node(&mut live.fixture, session)["ask"],
        json!({"id": id, "remote": true})
    );
    assert_eq!(
        ask(&mut live.fixture.core, session, "terminal"),
        failure("ask already pending")
    );
    // a stale owner and an unknown button change nothing; the answer takes its label from the stored buttons
    for answer in [
        json!({"id": id, "owner": 2, "button": "n"}),
        json!({"id": id, "owner": 1, "button": "x"}),
    ] {
        let resolve = frame(&presenter, &json!({"kind": "ask.resolve", "ask": answer}));
        live.fixture.core.presentation_line(serial, &resolve);
    }
    assert_eq!(ask_result(&mut live.fixture.core, &id)["result"], "pending");
    let answered = frame(
        &presenter,
        &json!({"kind": "ask.resolve", "ask": {"id": id, "owner": 1, "button": "n"}}),
    );
    live.fixture.core.presentation_line(serial, &answered);
    live.fixture.core.commit();
    assert_eq!(
        ask_result(&mut live.fixture.core, &id),
        json!({"result": "answered", "id": "n", "label": "No", "index": 1})
    );
    assert_eq!(
        presenter.last("ask.dismiss").unwrap()["ask"],
        json!({"id": id, "owner": 1})
    );
}

#[test]
fn a_cancel_here_tells_the_presenter_and_a_local_ask_stays_without_a_follower() {
    let (mut live, _, presenter) = presented();
    let session = live.session;
    let id = ask(&mut live.fixture.core, session, "terminal").result.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let cancel = live.fixture.core.dispatch(&agterm_proto::Request {
        cmd: Command::Ask(AskCmd::Cancel),
        target: Some(id.clone()),
        args: None,
    });
    assert!(cancel.ok);
    live.fixture.core.commit();
    assert_eq!(presenter.last("ask.dismiss").unwrap()["ask"]["id"], json!(id));
    assert_eq!(ask_result(&mut live.fixture.core, &id)["result"], "cancelled");

    // a presented session whose pane leads here draws the ask itself
    let mut plain = live_unfollowed();
    let opened = ask(&mut plain.0.fixture.core, plain.0.session, "terminal");
    assert!(opened.ok);
    assert!(plain.1.last("ask.request").is_none());
    assert!(
        node(&mut plain.0.fixture, plain.0.session)["ask"]
            .get("remote")
            .is_none()
    );
}

/// A live session with a presenter whose primary pane has no remote lead.
fn live_unfollowed() -> (Live, Viewer) {
    let mut live = live();
    let session = live.session;
    let (serial, presenter) = subscribe(&mut live.fixture.core, session, "presenter");
    let acquire = frame(&presenter, &json!({"kind": "presenter.acquire"}));
    live.fixture.core.presentation_line(serial, &acquire);
    (live, presenter)
}

#[test]
fn losing_the_presenter_takes_a_terminal_ask_back_and_fails_a_gui_one_it_cannot_show() {
    let (mut live, serial, _) = presented();
    let session = live.session;
    let id = ask(&mut live.fixture.core, session, "terminal").result.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    live.fixture.core.presentation_closed(serial);
    live.fixture.core.commit();
    assert_eq!(
        node(&mut live.fixture, session)["ask"],
        json!({"id": id}),
        "drawn here now"
    );
    assert_eq!(ask_result(&mut live.fixture.core, &id)["result"], "pending");
    let cancel = live.fixture.core.dispatch(&agterm_proto::Request {
        cmd: Command::Ask(AskCmd::Cancel),
        target: Some(id),
        args: None,
    });
    assert!(cancel.ok);

    let (mut live, serial, presenter) = presented();
    let session = live.session;
    let other = live.fixture.add_session(live.fixture.workspace);
    let gui = ask(&mut live.fixture.core, session, "gui").result.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        presenter
            .last("ask.request")
            .is_some_and(|request| request["ask"]["style"] == "gui")
    );
    // the session is no longer shown, so the window cannot place the ask it takes back
    live.fixture.store_mut().unwrap().select_session(Some(other));
    let rejected = frame(
        &presenter,
        &json!({"kind": "ask.rejected", "ask": {"id": gui, "owner": 1}}),
    );
    live.fixture.core.presentation_line(serial, &rejected);
    live.fixture.core.commit();
    assert_eq!(
        ask_result(&mut live.fixture.core, &gui),
        json!({"result": "cancelled", "reason": "presentation-lost"})
    );
}

#[test]
fn a_taken_back_gui_ask_moves_into_its_window_slot() {
    let (mut live, serial, _) = presented();
    let session = live.session;
    live.fixture.store_mut().unwrap().select_session(Some(session));
    let gui = ask(&mut live.fixture.core, session, "gui").result.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    live.fixture.core.presentation_closed(serial);
    live.fixture.core.commit();
    let window = live.fixture.window;
    assert_eq!(
        live.fixture
            .core
            .modals
            .window_ask(window)
            .map(|pending| pending.id.clone()),
        Some(gui.clone())
    );
    assert!(
        node(&mut live.fixture, session).get("ask").is_none(),
        "the session slot is free"
    );
    assert_eq!(ask_result(&mut live.fixture.core, &gui)["result"], "pending");
}

// MARK: - overlays handed to the presenter

fn overlay(fixture: &mut Fixture, cmd: SessionCmd, session: Uuid, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.session(cmd, Some(&wire(session)), args(edit))
}

#[test]
fn an_overlay_on_a_presented_session_runs_as_a_job_the_presenter_shows() {
    let (mut live, _, presenter) = presented();
    let session = live.session;
    let opened = overlay(&mut live.fixture, SessionCmd::OverlayOpen, session, |a| {
        a.command = Some("true".to_owned());
        a.size_percent = Some(60);
    });
    assert_eq!(opened.result, Some(json!({"id": wire(session)})));
    let request = presenter.last("overlay.request").unwrap()["overlay"].clone();
    assert_eq!(request["sizePercent"], 60);
    assert_eq!(
        (request["follow"].as_bool(), request["wait"].as_bool()),
        (Some(false), Some(false))
    );
    let job = request["job"].as_str().unwrap().to_owned();
    let read = node(&mut live.fixture, session);
    assert_eq!(read["remoteOverlays"], json!([{"sizePercent": 60}]));
    assert_eq!(read["overlay"], false, "nothing covers the session here");
    assert_eq!(
        overlay(&mut live.fixture, SessionCmd::OverlayOpen, session, |a| a.command =
            Some("ls".to_owned())),
        failure("overlay already open")
    );
    assert_eq!(
        overlay(&mut live.fixture, SessionCmd::OverlayResult, session, |_| {}),
        failure("overlay still running")
    );
    assert_eq!(
        overlay(&mut live.fixture, SessionCmd::OverlayText, session, |_| {}),
        failure("overlay is shown on another Mac")
    );
    assert!(
        overlay(&mut live.fixture, SessionCmd::OverlayResize, session, |a| a
            .size_percent =
            Some(40))
        .ok
    );
    assert_eq!(
        presenter.last("overlay.resize").unwrap()["overlay"],
        json!({"job": job, "sizePercent": 40})
    );
    run_job(&mut live, &job);
}

/// The helper claims `job`, receives its context and reports how the program ended, which frees the slot with its
/// exit code.
fn run_job(live: &mut Live, job: &str) {
    let session = live.session;
    let claim = live
        .fixture
        .session(SessionCmd::OverlayJobRun, Some(job), Args::default());
    assert_eq!(claim.result, Some(json!({"id": job})));
    let helper = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
    let serial = live
        .fixture
        .core
        .adopt_overlay_job_stream(job, Box::new(JobSink(Rc::clone(&helper))));
    let context: Value = serde_json::from_slice(&helper.borrow()[0]).unwrap();
    assert_eq!(context["context"]["command"], "true");
    assert_eq!(
        context["context"]["environment"]["AGTERM_SESSION_ID"],
        json!(wire(session))
    );
    live.fixture
        .core
        .overlay_job_line(job, serial, br#"{"kind":"started"}"#);
    live.fixture
        .core
        .overlay_job_line(job, serial, br#"{"kind":"exited","code":3}"#);
    live.fixture.core.commit();
    assert!(node(&mut live.fixture, session).get("remoteOverlays").is_none());
    assert_eq!(
        overlay(&mut live.fixture, SessionCmd::OverlayResult, session, |_| {}).result,
        Some(json!({"id": wire(session), "exitCode": 3}))
    );
}

/// A job helper's connection the test reads.
struct JobSink(Rc<RefCell<Vec<Vec<u8>>>>);

impl crate::JobChannel for JobSink {
    fn send(&self, line: Vec<u8>) -> bool {
        self.0.borrow_mut().push(line);
        true
    }

    fn shutdown(&self) {}
}

#[test]
fn closing_or_losing_a_handed_over_overlay_cancels_its_job() {
    let (mut live, serial, presenter) = presented();
    let session = live.session;
    let open = |live: &mut Live| {
        let opened = overlay(&mut live.fixture, SessionCmd::OverlayOpen, session, |a| {
            a.command = Some("sleep 9".to_owned());
            a.pane = Some("left".to_owned());
        });
        assert!(opened.ok, "{opened:?}");
    };
    open(&mut live);
    let job = presenter.last("overlay.request").unwrap()["overlay"]["job"].clone();
    assert_eq!(
        node(&mut live.fixture, session)["remoteOverlays"],
        json!([{"pane": "left"}])
    );
    assert!(
        overlay(&mut live.fixture, SessionCmd::OverlayClose, session, |a| a.pane =
            Some("left".to_owned()))
        .ok
    );
    assert_eq!(presenter.last("overlay.close").unwrap()["overlay"]["job"], job);
    live.fixture.core.commit();
    assert!(node(&mut live.fixture, session).get("remoteOverlays").is_none());
    assert_eq!(
        overlay(&mut live.fixture, SessionCmd::OverlayResult, session, |a| a.pane =
            Some("left".to_owned())),
        failure("overlay ended: canceled")
    );

    // refused by the presenter: the job fails without launching
    open(&mut live);
    let job = presenter.last("overlay.request").unwrap()["overlay"]["job"].clone();
    let rejected = frame(
        &presenter,
        &json!({"kind": "overlay.rejected", "overlay": {"job": job}}),
    );
    live.fixture.core.presentation_line(serial, &rejected);
    live.fixture.core.commit();
    assert_eq!(
        overlay(&mut live.fixture, SessionCmd::OverlayResult, session, |a| a.pane =
            Some("left".to_owned())),
        failure("overlay ended: launch-failed")
    );

    // the presenter goes away: an unclaimed job is cancelled for good
    open(&mut live);
    live.fixture.core.presentation_closed(serial);
    live.fixture.core.commit();
    assert!(node(&mut live.fixture, session).get("remoteOverlays").is_none());
    assert_eq!(
        overlay(&mut live.fixture, SessionCmd::OverlayResize, session, |a| a
            .size_percent =
            Some(40)),
        failure("no overlay")
    );
}

#[test]
fn without_a_following_pane_an_overlay_opens_here() {
    let (mut live, presenter) = live_unfollowed();
    let session = live.session;
    let opened = overlay(&mut live.fixture, SessionCmd::OverlayOpen, session, |a| {
        a.command = Some("true".to_owned());
    });
    assert!(opened.ok);
    assert!(presenter.last("overlay.request").is_none());
    assert_eq!(node(&mut live.fixture, session)["overlay"], true);
}
