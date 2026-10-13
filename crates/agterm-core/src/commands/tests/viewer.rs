//! Remote sessions, viewer side: `zmx.attach`, the attached row's presentation stream and what its frames put on the
//! row, the replicas of asks and overlays the origin hands over, the origin's layout, and a pane whose ssh lost the
//! connection (upstream's `ControlServer+RemotePresentation`, `ControlServer+RemoteReconnect`,
//! `RemotePresentationStateTests` and `RemoteLayoutCloseTests`), over a fake ssh runner and a fake bridge.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agterm_host::fakes::FakeSurface;
use agterm_proto::command::ZmxCmd;
use agterm_proto::{Args, Command, ControlAskOutcome, ControlAskResult, EventKind, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Fixture, args, failure, wire};
use crate::Core;
use crate::model::StatusPane;
use crate::remote::reconnect::RemoteLinkNotice;
use crate::remote::tree::{RemoteCommandResult, RemoteCommandRunner};
use crate::remote::{LinkKey, PresentationLink, PresentationTransport};
use crate::zmx::daemon_name;

const ORIGIN_SESSION: &str = "0A0B0C0D-0000-0000-0000-00000000000A";

// MARK: - fakes

/// Answers `zmx tree` with `tree` and a probe with `probe_status`.
struct Runner {
    tree: Mutex<RemoteCommandResult>,
    probe_status: Mutex<i32>,
    runs: Mutex<Vec<Vec<String>>>,
}

impl RemoteCommandRunner for Runner {
    fn run(&self, argv: &[String], _deadline: Duration) -> RemoteCommandResult {
        self.runs.lock().unwrap().push(argv.to_vec());
        if argv.last().is_some_and(|last| last == "true") {
            return RemoteCommandResult {
                status: *self.probe_status.lock().unwrap(),
                stdout: String::new(),
                stderr: "ssh: connect to host origin port 22: Connection refused\n".to_owned(),
            };
        }
        self.tree.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct LinkState {
    argv: Vec<String>,
    key: Option<LinkKey>,
    sent: Vec<Value>,
    stopped: bool,
}

#[derive(Clone, Default)]
struct Link(Rc<RefCell<LinkState>>);

impl PresentationLink for Link {
    fn send(&self, line: &[u8]) {
        self.0.borrow_mut().sent.push(serde_json::from_slice(line).unwrap());
    }

    fn stop(&self) {
        self.0.borrow_mut().stopped = true;
    }
}

#[derive(Default)]
struct Transport(RefCell<Vec<Link>>);

impl PresentationTransport for Transport {
    fn open(&self, argv: &[String], key: LinkKey) -> Box<dyn PresentationLink> {
        let link = Link::default();
        link.0.borrow_mut().argv = argv.to_vec();
        link.0.borrow_mut().key = Some(key);
        self.0.borrow_mut().push(link.clone());
        Box::new(link)
    }
}

/// A viewer with one attached row of the origin's split session.
struct Viewer {
    fixture: Fixture,
    runner: Arc<Runner>,
    transport: Rc<Transport>,
    session: Uuid,
    /// The origin's pane identities, left and right.
    origin: (Uuid, Uuid),
    /// The generation the origin's frames carry.
    revision: i64,
}

fn tree_answer(origin: (Uuid, Uuid), split: bool) -> String {
    let mut panes = vec![json!({"pane": "left", "daemon": daemon_name(origin.0), "foreground": ["/usr/bin/vim"]})];
    if split {
        panes.push(json!({"pane": "right", "daemon": daemon_name(origin.1)}));
    }
    json!({"ok": true, "result": {"remote": {
        "endpoint": {"executable": "/opt/agterm/bin/zmx", "socketDirectory": "/tmp/agterm-zmx-o"},
        "presentation": 1,
        "sessions": [{
            "id": ORIGIN_SESSION, "name": "build", "windowID": "W", "windowName": "main", "workspaceID": "WS",
            "workspaceName": "dev", "cwd": "/repo", "splitAxis": split.then_some("horizontal"), "panes": panes
        }]
    }}})
    .to_string()
}

fn attach(core: &mut Core, host: Option<&str>, session: Option<&str>) -> Response {
    core.dispatch(&agterm_proto::Request {
        cmd: Command::Zmx(ZmxCmd::Attach),
        target: session.map(str::to_owned),
        args: Some(args(|a| a.host = host.map(str::to_owned))),
    })
}

fn viewer_with(split: bool) -> Viewer {
    let mut fixture = Fixture::new(0);
    let origin = (Uuid::new_v4(), Uuid::new_v4());
    let runner = Arc::new(Runner {
        tree: Mutex::new(RemoteCommandResult {
            status: 0,
            stdout: tree_answer(origin, split),
            stderr: String::new(),
        }),
        probe_status: Mutex::new(255),
        runs: Mutex::new(Vec::new()),
    });
    fixture.core.set_remote_runner(runner.clone());
    let transport = Rc::new(Transport::default());
    fixture.core.set_presentation_transport(transport.clone());
    let response = attach(&mut fixture.core, Some("origin"), Some(ORIGIN_SESSION));
    let session = super::answered_id(&response).unwrap_or_else(|| panic!("{response:?}"));
    // the frontend mounts the replicas' terminals, each carrying its pane identity as its token
    let store = fixture.store_mut().unwrap();
    let found = store.session_mut(session).unwrap();
    let (primary, split_pane) = (found.pane_identity, found.split_pane_identity);
    found
        .pane_slot_mut(StatusPane::Left)
        .replace(Box::new(FakeSurface::new(wire(primary))));
    if let Some(split_pane) = split_pane {
        found
            .pane_slot_mut(StatusPane::Right)
            .replace(Box::new(FakeSurface::new(wire(split_pane))));
    }
    fixture.core.commit();
    Viewer {
        fixture,
        runner,
        transport,
        session,
        origin,
        revision: 0,
    }
}

impl Viewer {
    fn link(&self) -> Link {
        self.transport.0.borrow().last().cloned().unwrap()
    }

    fn sent(&self, kind: &str) -> Vec<Value> {
        self.link()
            .0
            .borrow()
            .sent
            .iter()
            .filter(|frame| frame["kind"] == kind)
            .cloned()
            .collect()
    }

    /// Delivers one origin frame of `body`, in order.
    fn frame(&mut self, mut body: Value) {
        self.revision += 1;
        body["gen"] = json!(5);
        body["rev"] = json!(self.revision);
        let key = self.link().0.borrow().key.unwrap();
        let line = serde_json::to_vec(&body).unwrap();
        self.fixture.core.viewer_line(key, &line);
        self.fixture.core.commit();
    }

    /// The origin's hello and snapshot.
    fn connect(&mut self, snapshot: &Value) {
        let key = self.link().0.borrow().key.unwrap();
        let hello = json!({"kind": "hello", "gen": 5, "rev": 0,
            "hello": {"version": 1, "kinds": ["status", "hud", "notify", "context", "layout"], "mode": "presenter"}});
        self.fixture.core.viewer_line(key, &serde_json::to_vec(&hello).unwrap());
        self.frame(json!({"kind": "snapshot", "snapshot": snapshot}));
    }

    fn node(&mut self) -> Value {
        let response = self.fixture.send(Command::Tree, None, Args::default());
        let tree = response.result.expect("a tree")["tree"].clone();
        tree["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|workspace| workspace["sessions"].as_array().cloned().unwrap_or_default())
            .find(|node| node["id"] == json!(wire(self.session)))
            .expect("the attached row")
    }

    fn session(&self) -> &crate::model::Session {
        self.fixture.store().session(self.session).unwrap()
    }
}

// MARK: - zmx.attach

#[test]
fn attach_refuses_before_creating_anything() {
    let mut fixture = Fixture::new(0);
    let before = fixture.store().sessions().count();
    let cases = [
        (None, Some(ORIGIN_SESSION), "zmx.attach requires a host"),
        (Some("origin"), None, "zmx.attach requires a remote session"),
        (Some("origin"), Some("a\u{1b}b"), "invalid remote session"),
        (Some("bad host"), Some(ORIGIN_SESSION), "invalid host"),
    ];
    for (host, session, error) in cases {
        assert_eq!(attach(&mut fixture.core, host, session), failure(error));
    }
    let origin = (Uuid::new_v4(), Uuid::new_v4());
    let runner = Arc::new(Runner {
        tree: Mutex::new(RemoteCommandResult {
            status: 0,
            stdout: tree_answer(origin, false),
            stderr: String::new(),
        }),
        probe_status: Mutex::new(0),
        runs: Mutex::new(Vec::new()),
    });
    fixture.core.set_remote_runner(runner.clone());
    let unknown = super::UNKNOWN;
    assert_eq!(
        attach(&mut fixture.core, Some("origin"), Some(unknown)),
        failure(&format!("no attachable session {unknown} on origin"))
    );
    // two lefts are refused rather than becoming one pane, or the wrong one
    let mut value: Value = serde_json::from_str(&tree_answer(origin, false)).unwrap();
    let pane = value["result"]["remote"]["sessions"][0]["panes"][0].clone();
    value["result"]["remote"]["sessions"][0]["panes"]
        .as_array_mut()
        .unwrap()
        .push(pane);
    runner.tree.lock().unwrap().stdout = value.to_string();
    assert_eq!(
        attach(&mut fixture.core, Some("origin"), Some(ORIGIN_SESSION)),
        failure("origin reported panes agterm cannot address")
    );
    *runner.tree.lock().unwrap() = RemoteCommandResult {
        status: 255,
        stdout: String::new(),
        stderr: "ssh: Could not resolve hostname origin\n".to_owned(),
    };
    assert_eq!(
        attach(&mut fixture.core, Some("origin"), Some(ORIGIN_SESSION)),
        failure("ssh: Could not resolve hostname origin")
    );
    assert_eq!(
        fixture.store().sessions().count(),
        before,
        "a refusal leaves no half-built row"
    );
}

#[test]
fn attach_creates_a_remote_row_with_both_panes_and_opens_its_stream() {
    let mut viewer = viewer_with(true);
    let node = viewer.node();
    assert_eq!(node["remoteHost"], "origin");
    assert_eq!(node["name"], "build");
    assert_eq!(node["splitAxis"], "horizontal");
    assert_eq!(node["commandWait"], true);
    assert_eq!(node["splitCommandWait"], true);
    assert_eq!(node["presentation"], json!({"state": "connecting", "mode": "mirror"}));
    let session = viewer.session();
    let primary = session.launch.command.clone().unwrap();
    assert!(primary.contains(&daemon_name(viewer.origin.0)), "{primary}");
    assert!(primary.contains("ZMX_MANAGED_CLAIM=1"), "the attach claims the lead");
    let split = session.split_launch.command.clone().unwrap();
    assert!(split.contains(&daemon_name(viewer.origin.1)));
    assert!(!session.is_persistable());
    // the remote is resolved again by the attach itself, and the stream runs `zmx present` for the origin's session
    assert!(
        viewer.runner.runs.lock().unwrap()[0]
            .last()
            .unwrap()
            .contains("zmx tree --json")
    );
    let link = viewer.link();
    assert!(
        link.0
            .borrow()
            .argv
            .last()
            .unwrap()
            .contains("exec agtermctl zmx present")
    );
    assert!(link.0.borrow().argv.last().unwrap().contains(ORIGIN_SESSION));
    assert_eq!(viewer.sent("hello").len(), 1);
}

#[test]
fn the_rows_visibility_raises_remote_events_and_follows_the_stream() {
    let mut store = crate::model::AppStore::new();
    let workspace = store.add_workspace("work");
    let id = store
        .add_session(
            workspace,
            crate::model::NewSession {
                remote_host: Some("origin".to_owned()),
                ..crate::model::NewSession::at_cwd("/tmp")
            },
        )
        .unwrap();
    store.close_session(id);
    let events: Vec<(EventKind, Option<String>)> = store
        .take_outbox()
        .events
        .into_iter()
        .filter(|event| matches!(event.kind, EventKind::RemoteOpened | EventKind::RemoteClosed))
        .map(|event| (event.kind, event.payload.host))
        .collect();
    assert_eq!(
        events,
        [
            (EventKind::RemoteOpened, Some("origin".to_owned())),
            (EventKind::RemoteClosed, Some("origin".to_owned()))
        ]
    );
    // the viewer's stream stops with its row
    let mut viewer = viewer_with(false);
    let link = viewer.link();
    let session = viewer.session;
    viewer.fixture.store_mut().unwrap().close_session(session);
    viewer.fixture.core.commit();
    assert!(link.0.borrow().stopped);
    assert_eq!(
        viewer.fixture.core.remote_wake(),
        None,
        "nothing remote is left to tick"
    );
}

// MARK: - the stream's state on the row

#[test]
fn the_snapshot_and_deltas_land_on_the_row_and_losing_the_stream_withdraws_them() {
    let mut viewer = viewer_with(false);
    let pane = wire(viewer.origin.0);
    viewer.connect(&json!({
        "status": {"status": "blocked", "blink": true, "pane": pane, "changedAt": 1000.0},
        "context": "release prep",
    }));
    assert_eq!(viewer.sent("presenter.acquire").len(), 1, "the origin offered the role");
    viewer.frame(json!({"kind": "presenter.granted"}));
    let node = viewer.node();
    assert_eq!(node["presentation"], json!({"state": "connected", "mode": "presenter"}));
    assert_eq!(node["status"], "blocked");
    assert_eq!(node["statusPane"], "left");
    assert_eq!(node["context"], "release prep");
    viewer.frame(json!({"kind": "context", "context": "shipping"}));
    assert_eq!(viewer.node()["context"], "shipping");
    viewer.frame(json!({"kind": "ping"}));
    assert_eq!(viewer.sent("ack").len(), 1);
    let key = viewer.link().0.borrow().key.unwrap();
    viewer.fixture.core.viewer_closed(key, "exit 255");
    viewer.fixture.core.commit();
    let node = viewer.node();
    assert_eq!(
        node["presentation"],
        json!({"state": "failed", "mode": "mirror", "error": "exit 255"})
    );
    assert!(node.get("context").is_none());
    assert!(node.get("status").is_none(), "the mirrored status went idle");
    // the backoff relaunches the bridge
    viewer.fixture.core.remote_tick(Instant::now() + Duration::from_secs(2));
    assert_eq!(viewer.transport.0.borrow().len(), 2);
}

// MARK: - replica asks

fn ask_request(id: &str, pane: Option<Uuid>) -> Value {
    let mut ask = json!({"id": id, "title": "Deploy?", "buttons": [{"id": "yes", "label": "Yes"}, {"id": "no", "label": "No"}],
        "style": "terminal", "align": "right", "owner": 3});
    if let Some(pane) = pane {
        ask["pane"] = json!(wire(pane));
    }
    json!({"kind": "ask.request", "ask": ask})
}

#[test]
fn a_handed_over_ask_is_shown_as_a_replica_and_its_answer_goes_to_the_origin() {
    let mut viewer = viewer_with(false);
    viewer.connect(&json!({}));
    let origin_pane = viewer.origin.0;
    viewer.frame(ask_request("A1", Some(origin_pane)));
    assert_eq!(
        viewer.node()["ask"],
        json!({"id": "A1", "pane": "left", "replica": true})
    );
    let answer = viewer.session().ask().unwrap().ask.answer(0).unwrap();
    let session = viewer.session;
    viewer.fixture.core.answer_session_ask(session, "A1", answer);
    viewer.fixture.core.commit();
    assert_eq!(
        viewer.sent("ask.resolve").last().unwrap()["ask"],
        json!({"id": "A1", "owner": 3, "button": "yes"})
    );
    // Escape sends no button
    viewer.frame(ask_request("A2", None));
    viewer
        .fixture
        .core
        .answer_session_ask(session, "A2", ControlAskResult::outcome(ControlAskOutcome::Escaped));
    viewer.fixture.core.commit();
    assert_eq!(
        viewer.sent("ask.resolve").last().unwrap()["ask"],
        json!({"id": "A2", "owner": 3})
    );
    // a cancel here refuses it, so the origin takes it back
    viewer.frame(ask_request("A3", None));
    viewer.fixture.store_mut().unwrap().cancel_pending_ask(session);
    viewer.fixture.core.commit();
    assert_eq!(
        viewer.sent("ask.rejected").last().unwrap()["ask"],
        json!({"id": "A3", "owner": 3})
    );
}

#[test]
fn a_replica_ask_the_row_cannot_show_is_refused_and_the_origins_dismissal_takes_it_down() {
    let mut viewer = viewer_with(false);
    viewer.connect(&json!({}));
    viewer.frame(ask_request("A1", Some(Uuid::new_v4())));
    assert_eq!(
        viewer.sent("ask.rejected").last().unwrap()["ask"],
        json!({"id": "A1", "owner": 3}),
        "a pane with no counterpart here"
    );
    viewer.frame(ask_request("A2", None));
    viewer.frame(ask_request("A3", None));
    assert_eq!(viewer.sent("ask.rejected").len(), 2, "the slot is taken");
    viewer.frame(json!({"kind": "ask.dismiss", "ask": {"id": "OTHER", "owner": 3}}));
    assert!(
        viewer.session().ask().is_some(),
        "a dismissal naming another ask leaves it"
    );
    viewer.frame(json!({"kind": "ask.dismiss", "ask": {"id": "A2", "owner": 3}}));
    assert!(viewer.session().ask().is_none());
    assert_eq!(viewer.sent("ask.resolve").len(), 0, "a dismissal is never answered");
    // losing the stream takes a replica down without answering it
    viewer.frame(ask_request("A4", None));
    let key = viewer.link().0.borrow().key.unwrap();
    viewer.fixture.core.viewer_closed(key, "exit 255");
    viewer.fixture.core.commit();
    assert!(viewer.session().ask().is_none());
    assert_eq!(viewer.sent("ask.resolve").len(), 0);
}

// MARK: - replica overlays

fn overlay_request(job: &str, pane: Option<Uuid>, wait: bool) -> Value {
    let mut overlay = json!({"job": job, "follow": false, "wait": wait, "sizePercent": 60});
    if let Some(pane) = pane {
        overlay["pane"] = json!(wire(pane));
    }
    json!({"kind": "overlay.request", "overlay": overlay})
}

#[test]
fn a_handed_over_overlay_runs_the_jobs_helper_and_its_close_here_tells_the_origin() {
    let mut viewer = viewer_with(false);
    viewer.connect(&json!({}));
    viewer.frame(overlay_request("J1", None, false));
    let slot = &viewer.session().overlay;
    assert!(slot.program_active());
    assert_eq!(slot.size_percent, Some(60));
    let command = slot.command.clone().unwrap();
    assert!(
        command.contains("'-tt'") && command.contains("overlay run-job"),
        "{command}"
    );
    viewer.frame(json!({"kind": "overlay.resize", "overlay": {"job": "J1", "sizePercent": 80}}));
    assert_eq!(viewer.session().overlay.size_percent, Some(80));
    // an occupied slot refuses the next one
    viewer.frame(overlay_request("J2", None, false));
    assert_eq!(viewer.sent("overlay.rejected").last().unwrap()["overlay"]["job"], "J2");
    let session = viewer.session;
    viewer.fixture.store_mut().unwrap().close_overlay(session);
    viewer.fixture.core.commit();
    assert_eq!(viewer.sent("overlay.closed").last().unwrap()["overlay"]["job"], "J1");
    // the origin's close takes it down, and that too is reported closed
    let origin_pane = viewer.origin.0;
    viewer.frame(overlay_request("J3", Some(origin_pane), false));
    assert!(viewer.session().pane_overlay(crate::model::OverlayPane::Left).is_some());
    viewer.frame(json!({"kind": "overlay.close", "overlay": {"job": "J3"}}));
    assert!(viewer.session().pane_overlay(crate::model::OverlayPane::Left).is_none());
    assert_eq!(viewer.sent("overlay.closed").last().unwrap()["overlay"]["job"], "J3");
}

#[test]
fn a_held_replica_closes_when_its_stream_goes_and_a_running_one_when_its_ssh_ends() {
    let mut viewer = viewer_with(false);
    viewer.connect(&json!({}));
    viewer.frame(overlay_request("J1", None, true));
    let session = viewer.session;
    let generation = viewer.session().overlay.generation;
    assert!(viewer.fixture.core.replica_overlay_held(session, generation));
    assert!(
        viewer.session().overlay.program_active(),
        "it stays for the user while its stream is up"
    );
    let key = viewer.link().0.borrow().key.unwrap();
    viewer.fixture.core.viewer_closed(key, "exit 255");
    viewer.fixture.core.commit();
    assert!(
        !viewer.session().overlay.active,
        "cut off, a held surface closes at once"
    );
    // a running one keeps its program, and closes once its ssh ends
    viewer.fixture.core.remote_tick(Instant::now() + Duration::from_secs(2));
    viewer.connect(&json!({}));
    viewer.frame(overlay_request("J2", None, true));
    let generation = viewer.session().overlay.generation;
    let key = viewer.link().0.borrow().key.unwrap();
    viewer.fixture.core.viewer_closed(key, "exit 255");
    viewer.fixture.core.commit();
    assert!(viewer.session().overlay.program_active());
    assert!(viewer.fixture.core.replica_overlay_held(session, generation));
    assert!(!viewer.session().overlay.active);
}

// MARK: - layout

#[test]
fn the_origins_removal_of_a_pane_closes_its_replica_here() {
    let mut viewer = viewer_with(true);
    viewer.connect(&json!({}));
    let (left, right) = viewer.origin;
    viewer.frame(
        json!({"kind": "layout", "layout": {"panes": [wire(right), wire(left)], "primary": wire(right),
        "axis": "vertical", "shown": true}}),
    );
    let session = viewer.session();
    assert_eq!(
        session
            .remote_presentation()
            .unwrap()
            .binding
            .remote_pane(session.pane_identity),
        Some(right),
        "the replicas follow the origin's order"
    );
    viewer.frame(json!({"kind": "layout", "layout": {"panes": [wire(right)], "primary": wire(right), "shown": false}}));
    let session = viewer.session();
    assert!(!session.split.exists, "the removed pane's replica closed");
    assert_eq!(
        session
            .remote_presentation()
            .unwrap()
            .binding
            .remote_pane(session.pane_identity),
        Some(right)
    );
}

// MARK: - reconnecting

#[test]
fn a_lost_link_waits_probes_with_backoff_and_attaches_the_pane_again() {
    let mut viewer = viewer_with(false);
    let session = viewer.session;
    let pane = viewer.session().pane_identity;
    let token = wire(pane);
    let nonce = viewer.fixture.core.zmx.leads.nonce(pane).unwrap().to_owned();
    // a notice from an attachment the pane replaced is not believed
    let stale = crate::PaneReport::Title(RemoteLinkNotice::title("old"));
    assert!(!viewer.fixture.core.report_pane(session, &token, &stale));
    let lost = crate::PaneReport::Title(RemoteLinkNotice::title(&nonce));
    assert!(viewer.fixture.core.report_pane(session, &token, &lost));
    assert!(viewer.fixture.core.reconnect_waiting(&token));
    let probe = |viewer: &mut Viewer, _: Duration| {
        for _ in 0..200 {
            viewer.fixture.core.remote_tick(Instant::now());
            if viewer.fixture.core.remote.viewer.probes_idle() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the probe never finished");
    };
    probe(&mut viewer, Duration::ZERO);
    let surface = viewer
        .node()
        .get("surfaces")
        .and_then(|surfaces| surfaces[0].get("reconnect").cloned());
    assert_eq!(
        surface,
        Some(json!({"failures": 1, "reason": "ssh: connect to host origin port 22: Connection refused"}))
    );
    assert_eq!(
        viewer.fixture.core.pane_reconnect_note(&token).as_deref(),
        Some("ssh: connect to host origin port 22: Connection refused")
    );
    assert!(viewer.fixture.core.take_pane_replacements().is_empty());
    *viewer.runner.probe_status.lock().unwrap() = 0;
    // a key press on the waiting pane retries now rather than after the backoff
    assert!(viewer.fixture.core.take_pane_lead(session, &token));
    probe(&mut viewer, Duration::ZERO);
    let replacements = viewer.fixture.core.take_pane_replacements();
    assert_eq!(replacements.len(), 1);
    let replacement = &replacements[0];
    assert!(replacement.remote && replacement.token == token);
    assert!(replacement.command.contains(&daemon_name(viewer.origin.0)));
    assert!(
        !replacement.command.contains("ZMX_MANAGED_CLAIM"),
        "a reconnect never claims the lead"
    );
    assert!(!viewer.fixture.core.reconnect_waiting(&token));
}

// MARK: - File ▸ Attach Remote

#[test]
fn attach_remote_lists_the_machines_sessions_in_the_picker_and_attaches_the_chosen_one() {
    let config = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join("remotes.conf"), "origin Build box\nother\n").unwrap();
    let settings = crate::persist::AppSettings {
        config_directory: Some(config.path().to_string_lossy().into_owned()),
        ..crate::persist::AppSettings::default()
    };
    let mut fixture = Fixture::with_settings(0, settings);
    let origin = (Uuid::new_v4(), Uuid::new_v4());
    fixture.core.set_remote_runner(Arc::new(Runner {
        tree: Mutex::new(RemoteCommandResult {
            status: 0,
            stdout: tree_answer(origin, false),
            stderr: String::new(),
        }),
        probe_status: Mutex::new(0),
        runs: Mutex::new(Vec::new()),
    }));
    fixture.core.set_presentation_transport(Rc::new(Transport::default()));
    let window = fixture.window;
    assert!(
        fixture
            .core
            .palette_context(window)
            .has(crate::palette_catalog::PaletteFacts::HAS_REMOTES)
    );
    let pending = |core: &Core| core.modal_slot(window).and_then(|slot| slot.pending().cloned());
    let settle = |core: &mut Core| {
        for _ in 0..200 {
            core.remote_tick(Instant::now());
            if core.remote_wake() != Some(Duration::from_millis(100)) || pending(core).is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    fixture.core.attach_remote(window);
    let hosts = pending(&fixture.core).expect("which machine?");
    assert_eq!(hosts.prompt.as_deref(), Some("Attach from which Mac?"));
    assert_eq!(hosts.items[0].label, "Build box");
    assert_eq!(hosts.items[0].subtitle.as_deref(), Some("origin"));
    fixture.core.resolve_pick(window, &hosts.id, hosts.picked(0).unwrap());
    settle(&mut fixture.core);
    let sessions = pending(&fixture.core).expect("which session?");
    assert_eq!(sessions.prompt.as_deref(), Some("Attach from origin"));
    assert_eq!(sessions.items[0].label, "build");
    assert_eq!(
        sessions.items[0].subtitle.as_deref(),
        Some("main/dev  ·  /repo  ·  vim")
    );
    let before = fixture.store().sessions().count();
    fixture
        .core
        .resolve_pick(window, &sessions.id, sessions.picked(0).unwrap());
    settle(&mut fixture.core);
    assert_eq!(fixture.store().sessions().count(), before + 1);
    let attached = fixture.store().sessions().last().unwrap();
    assert_eq!(attached.remote_host(), Some("origin"));
    assert_eq!(
        fixture.core.remote_wake(),
        Some(Duration::from_secs(1)),
        "only the stream is left to tick"
    );
}
