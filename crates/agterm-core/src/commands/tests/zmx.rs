//! The `zmx` group, the live launch seeds, the pane finalizer, the lead book's reads and typing, `session.lead`,
//! `session.restart`'s live flow and `zmx.reset`, over a [`FakeSessionHost`] (upstream's `ControlServer+Zmx`,
//! `ZmxLaunch`, `PaneLead`, `ControlServer+Restart` and `LiveResetCoordinator` behaviors).

use std::path::Path;
use std::sync::Arc;

use agterm_host::fakes::{FakeProcessTable, FakeSessionHost, FakeSurface, FakeSurfaceProbe};
use agterm_host::{KillOutcome, ProcessRecord, ProcessStat, SessionHost, Surface};
use agterm_proto::command::{SessionCmd, ZmxCmd};
use agterm_proto::{Args, Command, ControlZmxEndpoint, Request, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Fixture, UNKNOWN, args, failure, wire};
use crate::dispatch::Dispatched;
use crate::model::StatusPane;
use crate::pane_env::uuid_string;
use crate::persist::RestoreMode;
use crate::restore::mode::RestoreLaunchDecision;
use crate::zmx::daemon_name;
use crate::zmx::support::Inputs;
use crate::{Core, LiveInstall, PaneReplacement};

/// A live launch over a fake host: the first session's primary pane is a live terminal.
pub(super) struct Live {
    pub(super) fixture: Fixture,
    pub(super) host: Arc<FakeSessionHost>,
    resources: tempfile::TempDir,
    pub(super) session: Uuid,
    pub(super) left: Uuid,
    probe: FakeSurfaceProbe,
}

/// A resources directory holding the zsh integration a wrap checks for.
fn resources() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let integration = dir.path().join("shell-integration/zsh");
    std::fs::create_dir_all(&integration).unwrap();
    std::fs::write(integration.join(".zshenv"), "").unwrap();
    dir
}

fn install(core: &mut Core, host: &Arc<FakeSessionHost>, resources: &Path, mode: RestoreMode) {
    core.set_restore_launch(RestoreLaunchDecision::new(mode, None));
    core.install_live_sessions(LiveInstall {
        host: Arc::clone(host) as Arc<dyn SessionHost>,
        endpoint: ControlZmxEndpoint {
            executable: "/opt/agterm/bin/zmx".to_owned(),
            socket_directory: "/tmp/agterm-zmx-test".to_owned(),
        },
        wrap: Inputs {
            zmx_executable: "/bin/sh".into(),
            login_shell: Some("/bin/zsh".to_owned()),
            resources_dir: Some(resources.to_path_buf()),
            state_dir: resources.join("state"),
            ..Inputs::default()
        },
        state_dir: resources.to_path_buf(),
        outdated_before: None,
    });
}

fn live_with(extra: usize, mode: RestoreMode) -> Live {
    live_over(Fixture::new(extra), mode)
}

/// [`live_with`] over a fixture the caller built.
fn live_over(mut fixture: Fixture, mode: RestoreMode) -> Live {
    let host = Arc::new(FakeSessionHost::new());
    let resources = resources();
    install(&mut fixture.core, &host, resources.path(), mode);
    let session = fixture.sessions[0];
    let left = fixture.store().session(session).unwrap().pane_identity;
    let surface = FakeSurface::new(uuid_string(left)).with_live_session(true);
    let probe = surface.probe();
    let slot = fixture
        .store_mut()
        .and_then(|store| store.session_mut(session))
        .unwrap()
        .pane_slot_mut(StatusPane::Left);
    slot.replace(Box::new(surface));
    Live {
        fixture,
        host,
        resources,
        session,
        left,
        probe,
    }
}

pub(super) fn live() -> Live {
    live_with(0, RestoreMode::Live)
}

impl Live {
    fn zmx(&mut self, cmd: ZmxCmd, target: Option<&str>, edit: impl FnOnce(&mut Args)) -> Response {
        self.fixture.send(Command::Zmx(cmd), target, args(edit))
    }

    fn core(&mut self) -> &mut Core {
        &mut self.fixture.core
    }

    /// Adds a shown split whose terminal is live, and returns its identity.
    fn split(&mut self) -> Uuid {
        let session = self.session;
        let store = self.fixture.store_mut().unwrap();
        store.set_split_visibility(session, true, None);
        let found = store.session_mut(session).unwrap();
        let identity = found.split_pane_identity.unwrap();
        found.pane_slot_mut(StatusPane::Right).replace(Box::new(
            FakeSurface::new(uuid_string(identity)).with_live_session(true),
        ));
        identity
    }
}

/// What a case does to a request's args.
type Edit = fn(&mut Args);

fn result(response: &Response) -> Value {
    assert!(response.ok, "{response:?}");
    response.result.clone().unwrap_or(Value::Null)
}

/// The rows of `zmx.list` claimed by `session`, by pane.
fn rows(response: &Response, session: Uuid) -> Vec<Value> {
    result(response)["zmx"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["sessionID"] == json!(wire(session)))
        .cloned()
        .collect()
}

// MARK: - zmx.list

#[test]
fn every_command_needs_this_instance_s_zmx() {
    let mut fixture = Fixture::new(0);
    for cmd in [ZmxCmd::List, ZmxCmd::Prune] {
        assert_eq!(
            fixture.send(Command::Zmx(cmd), None, Args::default()),
            failure("zmx is unavailable in this instance")
        );
    }
    let screen = fixture.send(
        Command::Zmx(ZmxCmd::Screen),
        None,
        args(|a| a.name = Some("agterm-x".to_owned())),
    );
    assert_eq!(screen, failure("zmx is unavailable in this instance"));
    assert_eq!(
        fixture.send(Command::Zmx(ZmxCmd::Reset), None, args(|a| a.force = Some(true))),
        failure("zmx.reset is not supported on this platform")
    );
}

#[test]
fn the_list_joins_claims_against_the_observed_daemons() {
    let mut live = live_with(0, RestoreMode::None);
    let daemon = daemon_name(live.left);
    let response = live.zmx(ZmxCmd::List, None, |_| {});
    let inventory = &result(&response)["zmx"];
    assert_eq!(
        inventory["restore"],
        json!({"configured": "none", "requestedAtLaunch": "none", "active": "none", "restartRequired": false})
    );
    assert_eq!(inventory["inventoryComplete"], json!(true));
    assert_eq!(
        inventory["endpoint"],
        json!({"executable": "/opt/agterm/bin/zmx", "socketDirectory": "/tmp/agterm-zmx-test"})
    );
    // outside live a pane still claims its daemon name; nothing runs, so it is observed absent
    let row = &rows(&response, live.session)[0];
    assert_eq!(row["daemon"], json!(daemon));
    assert_eq!(
        (&row["state"], &row["observation"], &row["pane"], &row["windowState"]),
        (&json!("claimed"), &json!("absent"), &json!("left"), &json!("open"))
    );
    assert!(row.get("clients").is_none() && row.get("leaderPID").is_none());

    live.host.set_record(&daemon, Some(1), Some(4242), Some(10));
    live.host.set_record("notes", Some(0), Some(7), None);
    let response = live.zmx(ZmxCmd::List, None, |_| {});
    let row = &rows(&response, live.session)[0];
    assert_eq!(
        (&row["observation"], &row["clients"], &row["leaderPID"]),
        (&json!("running"), &json!(1), &json!(4242))
    );
    let entries = result(&response)["zmx"]["entries"].clone();
    let foreign: Vec<&Value> = entries
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["daemon"] == "notes")
        .collect();
    assert_eq!(foreign[0]["state"], json!("foreign"));

    live.host.fail_list("down");
    assert_eq!(
        live.zmx(ZmxCmd::List, None, |_| {}),
        failure("could not read the zmx session list")
    );
}

// MARK: - zmx.prune

#[test]
fn prune_kills_only_detached_orphans_and_reports_each() {
    let mut live = live();
    assert_eq!(
        result(&live.zmx(ZmxCmd::Prune, None, |_| {})),
        json!({"text": "no orphan daemons", "affected": 0})
    );
    let (orphan, stale, busy) = (
        daemon_name(Uuid::from_u128(1)),
        daemon_name(Uuid::from_u128(2)),
        daemon_name(Uuid::from_u128(3)),
    );
    live.host.set_record(&orphan, Some(0), Some(11), None);
    live.host.set_record(&stale, Some(0), Some(12), None);
    live.host.set_record(&busy, Some(1), Some(13), None);
    live.host.set_record(&daemon_name(live.left), Some(0), Some(14), None);
    live.host.set_kill_outcome(&stale, Some(KillOutcome::StaleSocket));
    let response = live.zmx(ZmxCmd::Prune, None, |_| {});
    assert_eq!(
        result(&response),
        json!({
            "text": format!(
                "killed {orphan}; {stale}: cleaned up a stale socket, the daemon may still be running"
            ),
            "affected": 1
        })
    );
    // never forced, and never the claimed pane's or an attached one
    assert_eq!(live.host.kills(), [(orphan, false), (stale, false)]);
}

// MARK: - zmx.kill

#[test]
fn kill_refuses_in_upstream_s_order() {
    let mut live = live();
    let sid = wire(live.session);
    let cases: [(Option<&str>, Edit, String); 7] = [
        (
            None,
            |a| (a.pane, a.force) = (Some("left".to_owned()), Some(true)),
            "zmx.kill requires an explicit --target".to_owned(),
        ),
        (
            Some("S"),
            |a| a.force = Some(true),
            "zmx.kill requires --pane left|right".to_owned(),
        ),
        (
            Some("S"),
            |a| (a.pane, a.force) = (Some("scratch".to_owned()), Some(true)),
            "zmx.kill requires --pane left|right".to_owned(),
        ),
        (
            Some("S"),
            |a| a.pane = Some("left".to_owned()),
            "zmx.kill requires --force".to_owned(),
        ),
        (
            Some("active"),
            |a| (a.pane, a.force) = (Some("left".to_owned()), Some(true)),
            "zmx.kill needs a session id; 'active' is not accepted".to_owned(),
        ),
        (
            Some(UNKNOWN),
            |a| (a.pane, a.force) = (Some("left".to_owned()), Some(true)),
            format!("no left pane daemon for session {UNKNOWN}"),
        ),
        (
            Some("S"),
            |a| (a.pane, a.force) = (Some("right".to_owned()), Some(true)),
            "no right pane daemon for session S".to_owned(),
        ),
    ];
    for (target, edit, error) in cases {
        let target = target.map(|target| if target == "S" { sid.as_str() } else { target });
        assert_eq!(
            live.zmx(ZmxCmd::Kill, target, edit),
            failure(&error.replace("session S", &format!("session {sid}"))),
            "{target:?}"
        );
    }
    let window = live.zmx(ZmxCmd::Kill, Some(&sid), |a| {
        (a.pane, a.force, a.window) = (Some("left".to_owned()), Some(true), Some("active".to_owned()));
    });
    assert_eq!(window, failure("zmx.kill needs a window id; 'active' is not accepted"));
    let other = live.zmx(ZmxCmd::Kill, Some(&sid), |a| {
        (a.pane, a.force, a.window) = (Some("left".to_owned()), Some(true), Some(UNKNOWN.to_owned()));
    });
    assert_eq!(other, failure(&format!("no such window: {UNKNOWN}")));
    // claimed, but no daemon runs
    let absent = live.zmx(ZmxCmd::Kill, Some(&sid), |a| {
        (a.pane, a.force) = (Some("left".to_owned()), Some(true));
    });
    assert_eq!(absent, failure(&format!("{} is not running", daemon_name(live.left))));
    assert!(live.host.kills().is_empty());
}

#[test]
fn a_confirmed_kill_closes_the_split_and_is_not_finalized_twice() {
    let mut live = live();
    let split = live.split();
    let daemon = daemon_name(split);
    live.host.set_record(&daemon, Some(1), Some(20), None);
    let sid = wire(live.session);
    let response = live.zmx(ZmxCmd::Kill, Some(&sid), |a| {
        (a.pane, a.force) = (Some("bottom".to_owned()), Some(true));
    });
    assert_eq!(
        result(&response),
        json!({"id": sid, "text": format!("killed {daemon}"), "pane": "right"})
    );
    let _ = live.core().commit();
    let session = live.fixture.store().session(live.session).unwrap();
    assert!(!session.split.exists && session.split_pane_identity.is_none());
    // the forced kill only: the split's removal does not ask zmx to kill a name that is gone
    assert_eq!(live.host.kills(), [(daemon, true)]);
}

#[test]
fn an_unconfirmed_kill_leaves_the_pane() {
    let mut live = live();
    let daemon = daemon_name(live.left);
    live.host.set_record(&daemon, Some(1), Some(20), None);
    live.host.set_kill_outcome(&daemon, Some(KillOutcome::StaleSocket));
    let sid = wire(live.session);
    let kill = |live: &mut Live| {
        live.zmx(ZmxCmd::Kill, Some(&sid), |a| {
            (a.pane, a.force) = (Some("left".to_owned()), Some(true));
        })
    };
    assert_eq!(
        kill(&mut live),
        failure(&format!(
            "{daemon} did not confirm the kill; zmx cleaned up a stale socket and the daemon may still be running"
        ))
    );
    live.host
        .set_kill_outcome(&daemon, Some(KillOutcome::Failed("busy".to_owned())));
    assert_eq!(kill(&mut live), failure(&format!("could not kill {daemon}: busy")));
    assert!(live.fixture.store().session(live.session).is_some());
}

// MARK: - zmx.screen

#[test]
fn screen_reads_the_daemon_grid_by_name() {
    let mut live = live();
    let cases: [(Edit, &str); 4] = [
        (|_| {}, "zmx.screen requires a daemon name"),
        (
            |a| a.name = Some("two words".to_owned()),
            "zmx.screen requires a daemon name",
        ),
        (
            |a| (a.name, a.all, a.lines) = (Some("agterm-x".to_owned()), Some(true), Some(3)),
            "use either --all or --lines, not both",
        ),
        (
            |a| (a.name, a.lines) = (Some("agterm-x".to_owned()), Some(0)),
            "--lines must be greater than 0",
        ),
    ];
    for (edit, error) in cases {
        assert_eq!(live.zmx(ZmxCmd::Screen, None, edit), failure(error));
    }
    assert_eq!(
        live.zmx(ZmxCmd::Screen, None, |a| a.name = Some("agterm-x".to_owned())),
        failure("could not read the zmx screen of agterm-x")
    );
    live.host.insert("agterm-x", "two\nthree\n");
    live.host.set_scrollback("agterm-x", "one\n");
    assert_eq!(
        result(&live.zmx(ZmxCmd::Screen, None, |a| a.name = Some(" agterm-x ".to_owned()))),
        json!({"text": "two\nthree\n"})
    );
    assert_eq!(
        result(&live.zmx(ZmxCmd::Screen, None, |a| (a.name, a.lines) =
            (Some("agterm-x".to_owned()), Some(2)))),
        json!({"text": "two\nthree"})
    );
}

// MARK: - the finalizer

#[test]
fn a_soft_closed_session_ends_its_daemons_only_once_the_close_is_final() {
    let mut live = live();
    let split = live.split();
    let session = live.session;
    assert!(live.fixture.store_mut().unwrap().soft_close_session(session));
    // a soft close holds the daemons for its undo window; making it final ends both
    let _ = live.core().commit();
    assert!(live.host.kills().is_empty(), "killed inside the undo window");
    live.core().library_mut().finalize_all_pending_closes();
    let _ = live.core().commit();
    let mut killed: Vec<(String, bool)> = live.host.kills();
    killed.sort();
    let mut expected = vec![(daemon_name(live.left), true), (daemon_name(split), true)];
    expected.sort();
    assert_eq!(killed, expected);
}

#[test]
fn a_hard_closed_session_ends_its_daemons_at_once() {
    let mut live = live();
    let sid = wire(live.session);
    // one target closes for good
    assert!(live.fixture.session(SessionCmd::Close, Some(&sid), Args::default()).ok);
    let _ = live.core().commit();
    assert_eq!(live.host.kills(), [(daemon_name(live.left), true)]);
}

#[test]
fn a_closed_split_ends_only_its_daemon() {
    let mut live = live();
    let split = live.split();
    let sid = wire(live.session);
    assert!(
        live.fixture
            .session(SessionCmd::SplitClose, Some(&sid), Args::default())
            .ok
    );
    let _ = live.core().commit();
    assert_eq!(live.host.kills(), [(daemon_name(split), true)]);
}

// MARK: - live seeds

#[test]
fn a_live_launch_wraps_the_pane_in_its_daemon() {
    let mut live = live();
    let token = uuid_string(live.left);
    let seed = live
        .fixture
        .core
        .request_pane_spawn(live.session, &token, std::time::Instant::now())
        .unwrap();
    assert!(seed.live && !seed.wait_after_command && seed.initial_input.is_none());
    let command = seed.command.unwrap();
    assert!(command.contains("; exec '/bin/sh' 'attach' '"), "{command}");
    assert!(command.contains(&daemon_name(live.left)), "{command}");
    let env = |key: &str| {
        seed.env
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(env("AGTERM_PANE_ID"), Some(token));
    assert_eq!(env("SHELL").as_deref(), Some("/bin/zsh"));
    assert!(env("ZMX_MANAGED").is_some_and(|nonce| nonce.len() == 32));
    assert_eq!(env("ZMX_MANAGED_CLAIM"), None, "a launch attach never claims the lead");
}

#[test]
fn a_fallback_or_ordinary_launch_runs_a_plain_shell() {
    for (requested, reason) in [
        (RestoreMode::Live, Some("no zmx".to_owned())),
        (RestoreMode::None, None),
    ] {
        let mut live = live();
        live.core()
            .set_restore_launch(RestoreLaunchDecision::new(requested, reason));
        let token = uuid_string(live.left);
        let seed = live
            .fixture
            .core
            .request_pane_spawn(live.session, &token, std::time::Instant::now())
            .unwrap();
        assert!(
            !seed.live && seed.command.is_none() && seed.env.is_empty(),
            "{requested:?}"
        );
    }
}

// MARK: - the lead

/// Feeds the role report `role` of the live pane's current attachment.
pub(super) fn report(live: &mut Live, token: &str, nonce: &str, role: &str, generation: u32) -> bool {
    let title = format!("zmx-role;{nonce}:{role}:{generation}");
    live.fixture
        .core
        .report_pane(live.session, token, &crate::PaneReport::Title(title))
}

/// Spawns the live pane, which begins its attachment, and returns its nonce.
pub(super) fn spawn_live(live: &mut Live) -> String {
    let token = uuid_string(live.left);
    let seed = live
        .fixture
        .core
        .request_pane_spawn(live.session, &token, std::time::Instant::now())
        .unwrap();
    seed.env
        .into_iter()
        .find(|(key, _)| key == "ZMX_MANAGED")
        .map(|(_, nonce)| nonce)
        .unwrap()
}

#[test]
fn a_pane_without_a_reported_role_has_no_lead_to_take() {
    let mut live = live();
    let sid = wire(live.session);
    assert_eq!(
        live.fixture.session(SessionCmd::Lead, Some(&sid), Args::default()),
        failure("pane has no lead to take")
    );
}

#[test]
fn a_leading_pane_answers_lead_and_reads_and_types_through_its_daemon() {
    let mut live = live();
    let token = uuid_string(live.left);
    let nonce = spawn_live(&mut live);
    let daemon = daemon_name(live.left);
    live.host.insert(&daemon, "from the daemon\n");
    live.probe.set_lines(["from the surface"]);
    // a report for another attachment is dropped; the title never reaches the session
    assert!(!report(&mut live, &token, "other", "leader", 1));
    assert!(report(&mut live, &token, &nonce, "leader", 1));
    let session = live.fixture.store().session(live.session).unwrap();
    assert_eq!(session.osc_title, None);
    let sid = wire(live.session);
    assert_eq!(
        live.fixture.session(SessionCmd::Lead, Some(&sid), Args::default()),
        Response::with_result(json!({"id": sid}))
    );
    let tree = result(&live.fixture.send(Command::Tree, None, Args::default()));
    let surfaces = &tree["tree"]["workspaces"][0]["sessions"][0]["surfaces"];
    assert_eq!(surfaces[0]["lead"], json!("leader"));
    // the viewport of a pane that leads is its own; the full buffer is the daemon's
    let text = |live: &mut Live, all| {
        result(
            &live
                .fixture
                .session(SessionCmd::Text, Some(&sid), args(|a| a.all = all)),
        )["text"]
            .clone()
    };
    assert_eq!(text(&mut live, None), json!("from the surface"));
    assert_eq!(text(&mut live, Some(true)), json!("from the daemon\n"));
    // typing takes the daemon's acknowledged path, the final Return paced
    assert!(
        live.fixture
            .session(SessionCmd::Type, Some(&sid), args(|a| a.text = Some("ls\n".to_owned())))
            .ok
    );
    assert_eq!(live.host.session(&daemon).unwrap().typed, b"ls\r");
    assert!(live.probe.typed().is_empty());
}

#[test]
fn a_follower_is_covered_and_taken_over_with_the_claim() {
    let mut live = live();
    let token = uuid_string(live.left);
    let nonce = spawn_live(&mut live);
    assert!(report(&mut live, &token, &nonce, "follower", 1));
    let cover = live.core().pane_lead_cover(&token).unwrap();
    assert_eq!(
        (cover.title, cover.offers_takeover),
        ("In use from another machine", true)
    );
    let sid = wire(live.session);
    // a covered pane's own surface answers nothing
    assert_eq!(
        live.fixture.session(SessionCmd::Copy, Some(&sid), Args::default()),
        failure("pane is covered while another Mac leads it; take the lead first (session lead)")
    );
    assert!(live.fixture.session(SessionCmd::Lead, Some(&sid), Args::default()).ok);
    let replacements = live.core().take_pane_replacements();
    assert_eq!(replacements.len(), 1);
    let fresh = &replacements[0];
    assert_eq!((fresh.session, fresh.token.as_str()), (live.session, token.as_str()));
    assert!(
        fresh
            .command
            .ends_with("'/bin/sh' '-c' 'printf '\\''%s\\n'\\'' '\\''agterm: session is gone'\\''; exit 1'")
    );
    assert!(
        fresh
            .env
            .iter()
            .any(|(key, value)| key == "ZMX_MANAGED_CLAIM" && value == "1")
    );
    // covered until the fresh attach reports, and asked once
    assert_eq!(live.core().pane_lead_cover(&token).unwrap().title, "Taking over…");
    assert!(!live.fixture.core.take_pane_lead(live.session, &token));
}

#[test]
fn an_unowned_report_reattaches_without_the_claim() {
    let mut live = live();
    let token = uuid_string(live.left);
    let nonce = spawn_live(&mut live);
    assert!(report(&mut live, &token, &nonce, "unowned", 1));
    let replacements = live.core().take_pane_replacements();
    assert_eq!(replacements.len(), 1);
    assert!(!replacements[0].env.iter().any(|(key, _)| key == "ZMX_MANAGED_CLAIM"));
}

// MARK: - session.restart

/// Runs `dispatched` step by step, calling `between` before each pending step.
fn drive(core: &mut Core, mut dispatched: Dispatched, mut between: impl FnMut(&mut Core, usize)) -> Response {
    let mut step = 0;
    loop {
        match dispatched {
            Dispatched::Done(response) => return response,
            Dispatched::Pending { resume, .. } => {
                between(core, step);
                step += 1;
                dispatched = resume(core);
            }
        }
    }
}

#[test]
fn a_restart_ends_the_old_shell_and_rebuilds_the_pane_on_its_daemon() {
    let mut live = live();
    let token = uuid_string(live.left);
    let daemon = daemon_name(live.left);
    live.host.set_record(&daemon, Some(1), Some(100), None);
    let job = ProcessRecord {
        pid: 101,
        started: 7,
        group: 101,
        foreground: 101,
    };
    live.host.set_job(100, vec![job]);
    let request = Request {
        cmd: Command::Session(SessionCmd::Restart),
        target: Some(wire(live.session)),
        args: Some(args(|a| {
            (a.pane, a.command) = (Some("left".to_owned()), Some("cat".to_owned()));
        })),
    };
    let dispatched = live.core().begin(&request);
    let host = Arc::clone(&live.host);
    let session = live.session;
    let response = drive(live.core(), dispatched, |core, step| {
        // the old program ignores the hangup until its grace ends and it is killed; the new shell shows up after the
        // rebuild
        if step == 0 {
            assert!(core.zmx.claimed_exits.contains(&token));
            assert!(!core.pane_exited(session, &token), "the dying client closed the pane");
        }
        if !core.take_pane_replacements().is_empty() {
            host.set_record(&daemon, Some(1), Some(200), None);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    });
    assert_eq!(
        result(&response),
        json!({
            "id": wire(live.session),
            "text": "restarted left pane: shell 100 -> 200",
            "pane": "left",
            "restart": {"paneID": token, "oldPid": 100, "newPid": 200}
        })
    );
    assert_eq!(live.host.kills(), [(daemon_name(live.left), true)]);
    assert_eq!(live.host.ended(), [job]);
    assert!(live.core().zmx.claimed_exits.is_empty());
}

/// Starts a restart of the live pane with `--command exit`, whose old shell (leader 100) has no job.
fn begin_restart(live: &mut Live) -> Dispatched {
    live.host.set_record(&daemon_name(live.left), Some(1), Some(100), None);
    let request = Request {
        cmd: Command::Session(SessionCmd::Restart),
        target: Some(wire(live.session)),
        args: Some(args(|a| {
            (a.pane, a.command) = (Some("left".to_owned()), Some("exit".to_owned()));
        })),
    };
    live.core().begin(&request)
}

#[test]
fn a_replacement_whose_attach_exits_closes_the_pane_and_ends_the_restart() {
    let mut live = live();
    let token = uuid_string(live.left);
    let session = live.session;
    let dispatched = begin_restart(&mut live);
    let mut rebuilt_at = None;
    let started = std::time::Instant::now();
    let response = drive(live.core(), dispatched, |core, step| {
        if !core.take_pane_replacements().is_empty() {
            rebuilt_at = Some(step);
        }
        // the frontend installed the replacement in the settle after the rebuild; the probe after that lifts the
        // claim, and the new terminal's attach then exits at once (the daemon ran `exit`)
        if rebuilt_at.is_some_and(|rebuilt| step == rebuilt + 2) {
            assert!(!core.zmx.claimed_exits.contains(&token));
            assert!(
                core.pane_exited(session, &token),
                "the replacement's own exit was swallowed"
            );
        }
    });
    assert_eq!(
        response,
        failure("the old shell ended (pid 100) and no new one was observed")
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "waited out the restart"
    );
    assert!(live.fixture.store().session(session).is_none());
}

#[test]
fn an_installed_replacement_s_exit_closes_the_pane_before_the_first_probe() {
    let mut live = live();
    let token = uuid_string(live.left);
    let session = live.session;
    let dispatched = begin_restart(&mut live);
    let response = drive(live.core(), dispatched, |core, step| {
        if step == 0 {
            // the settle after the rebuild installs the replacement; its attach exits before the restart probes again
            let replacements = core.take_pane_replacements();
            assert_eq!(replacements.len(), 1);
            core.pane_replaced(&replacements[0]);
            assert!(
                core.pane_exited(session, &token),
                "the new terminal's exit was swallowed"
            );
        }
    });
    assert_eq!(
        response,
        failure("the old shell ended (pid 100) and no new one was observed")
    );
    assert!(live.fixture.store().session(session).is_none());
}

#[test]
fn a_dropped_restart_replacement_closes_the_ended_pane() {
    let mut live = live();
    let session = live.session;
    let dispatched = begin_restart(&mut live);
    let response = drive(live.core(), dispatched, |core, _| {
        for replacement in core.take_pane_replacements() {
            assert!(core.replacement_dropped(&replacement));
        }
    });
    assert_eq!(
        response,
        failure("the old shell ended (pid 100) and no new one was observed")
    );
    assert!(live.fixture.store().session(session).is_none());
    assert!(live.core().zmx.claimed_exits.is_empty());
}

#[test]
fn a_dropped_takeover_leaves_the_pane_covered_as_it_was() {
    let mut live = live();
    let token = uuid_string(live.left);
    let nonce = spawn_live(&mut live);
    assert!(report(&mut live, &token, &nonce, "follower", 1));
    assert!(live.fixture.core.take_pane_lead(live.session, &token));
    let replacements = live.core().take_pane_replacements();
    assert_eq!(live.core().pane_lead_cover(&token).unwrap().title, "Taking over…");
    assert!(!live.core().replacement_dropped(&replacements[0]));
    let cover = live.core().pane_lead_cover(&token).unwrap();
    assert_eq!(
        (cover.title, cover.offers_takeover),
        ("In use from another machine", true)
    );
    // the old attachment's reports still count, and the takeover can be asked again
    assert!(report(&mut live, &token, &nonce, "leader", 2));
    assert_eq!(live.core().pane_lead_cover(&token), None);
}

#[test]
fn a_pane_a_pending_close_holds_is_not_reattached() {
    let mut live = live();
    let token = uuid_string(live.left);
    let nonce = spawn_live(&mut live);
    let session = live.session;
    assert!(live.fixture.store_mut().unwrap().soft_close_session(session));
    let held = live.fixture.store().pending_close_session(session).is_some();
    assert!(held, "the soft close holds the session for its undo");
    // its daemon lost the leader during the undo window: no page could take a fresh attach
    assert!(report(&mut live, &token, &nonce, "unowned", 1));
    assert!(live.core().take_pane_replacements().is_empty());
    assert_eq!(live.core().pane_lead_cover(&token).unwrap().title, "Reconnecting…");
    // the attachment it kept still takes the next report
    assert!(report(&mut live, &token, &nonce, "leader", 2));
    assert_eq!(live.core().pane_lead_cover(&token), None);
}

#[test]
fn search_is_refused_on_a_covered_pane() {
    let mut live = live();
    let token = uuid_string(live.left);
    let nonce = spawn_live(&mut live);
    assert!(report(&mut live, &token, &nonce, "follower", 1));
    let sid = wire(live.session);
    assert_eq!(
        live.fixture.session(
            SessionCmd::Search,
            Some(&sid),
            args(|a| a.text = Some("foo".to_owned()))
        ),
        failure("pane is covered while another Mac leads it; take the lead first (session lead)")
    );
    assert!(live.probe.actions().is_empty(), "the covered surface was searched");
    // closing stays available as cleanup
    assert!(
        live.fixture
            .session(
                SessionCmd::Search,
                Some(&sid),
                args(|a| a.to = Some("close".to_owned()))
            )
            .ok
    );
}

#[test]
fn a_restart_refuses_before_killing_when_the_program_cannot_be_tracked_or_replayed() {
    let mut live = live();
    let daemon = daemon_name(live.left);
    live.host.set_record(&daemon, Some(1), Some(100), None);
    // without a command it replays the foreground program, which a fresh table cannot read
    let table = FakeProcessTable::default();
    table.insert(
        ProcessStat {
            pid: 100,
            ppid: 1,
            pgrp: 100,
            session: 100,
        },
        Some(&["-zsh"]),
    );
    table.set_terminal_foreground(100, 100);
    live.core().set_process_table(Box::new(table), None);
    let sid = wire(live.session);
    let response = live.fixture.session(
        SessionCmd::Restart,
        Some(&sid),
        args(|a| a.pane = Some("left".to_owned())),
    );
    assert_eq!(
        response,
        failure(
            "no replayable foreground program: zsh holds the pane; pass --command to name the line. nothing was changed"
        )
    );
    assert!(live.host.kills().is_empty());
}

// MARK: - session.restart replay (upstream's `ControlServerRestartTests` replay cases)

/// The root shell's pid, the one the daemon listing reports as its leader.
const ROOT_SHELL: i32 = 100;
/// The pid of a job the root shell started, leading its own group.
const JOB: i32 = 300;

/// A process in the pane's terminal session.
fn member(pid: i32, parent: i32, group: i32) -> ProcessStat {
    ProcessStat {
        pid,
        ppid: parent,
        pgrp: group,
        session: ROOT_SHELL,
    }
}

/// A table whose root shell runs `root` and hands its terminal to `foreground` (`None`: no foreground to read).
fn replay_table(root: &[&str], foreground: Option<i32>) -> FakeProcessTable {
    let table = FakeProcessTable::default();
    table.insert(member(ROOT_SHELL, 1, ROOT_SHELL), Some(root));
    if let Some(group) = foreground {
        table.set_terminal_foreground(ROOT_SHELL, group);
    }
    table
}

/// A table whose root zsh runs job `argv` in the foreground, in `directory`.
fn job_table(argv: &[&str], directory: &str) -> FakeProcessTable {
    let table = replay_table(&["-zsh"], Some(JOB));
    table.insert(member(JOB, ROOT_SHELL, JOB), Some(argv));
    table.set_working_directory(JOB, directory);
    table
}

/// Starts a restart without a command of the live pane by its token, over `table`.
fn begin_replay(live: &mut Live, table: FakeProcessTable) -> Dispatched {
    live.host
        .set_record(&daemon_name(live.left), Some(1), Some(ROOT_SHELL), None);
    live.core().set_process_table(Box::new(table), None);
    let request = Request {
        cmd: Command::Session(SessionCmd::Restart),
        target: Some(wire(live.session)),
        args: Some(args(|a| a.pane_id = Some(uuid_string(live.left)))),
    };
    live.core().begin(&request)
}

/// Replays over `table`: the new shell shows up as pid 200 once the pane is rebuilt. The response and the replacements.
fn replay(live: &mut Live, table: FakeProcessTable) -> (Response, Vec<PaneReplacement>) {
    let dispatched = begin_replay(live, table);
    let host = Arc::clone(&live.host);
    let daemon = daemon_name(live.left);
    let mut launched = Vec::new();
    let response = drive(live.core(), dispatched, |core, _| {
        let replacements = core.take_pane_replacements();
        if !replacements.is_empty() {
            host.set_record(&daemon, Some(1), Some(200), None);
        }
        launched.extend(replacements);
    });
    (response, launched)
}

#[test]
fn a_replay_launches_the_program_s_argv_in_the_directory_it_runs_in() {
    let mut live = live();
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path().to_str().unwrap().to_owned();
    let token = uuid_string(live.left);
    let (response, launched) = replay(&mut live, job_table(&["/bin/sleep", "300"], &directory));
    assert_eq!(
        result(&response),
        json!({
            "id": wire(live.session),
            "text": "restarted left pane: shell 100 -> 200; replay requested: '/bin/sleep' '300'",
            "pane": "left",
            "restart": {"paneID": token, "oldPid": 100, "newPid": 200, "replayedArgv": ["/bin/sleep", "300"]}
        })
    );
    assert_eq!(live.host.kills(), [(daemon_name(live.left), true)]);
    let [launch] = launched.as_slice() else {
        panic!("{launched:?}")
    };
    let session_cwd = live
        .fixture
        .store()
        .session(live.session)
        .unwrap()
        .cwd_for(StatusPane::Left)
        .to_owned();
    assert_eq!(launch.cwd.as_deref(), Some(directory.as_str()));
    assert_ne!(directory, session_cwd);
    assert!(
        launch.command.contains(r"'\''/bin/sleep'\'' '\''300'\''"),
        "{}",
        launch.command
    );
}

#[test]
fn a_shell_started_as_a_job_is_replayed_like_any_program() {
    let mut live = live();
    let directory = tempfile::tempdir().unwrap();
    let table = job_table(&["/bin/sh", "-c", "read line"], directory.path().to_str().unwrap());
    let (response, launched) = replay(&mut live, table);
    assert_eq!(
        result(&response)["restart"]["replayedArgv"],
        json!(["/bin/sh", "-c", "read line"])
    );
    assert_eq!(live.host.kills(), [(daemon_name(live.left), true)]);
    assert_eq!(launched.len(), 1);
}

/// Upstream's refusal of a replay: `reason`, then the way out.
fn refused(reason: &str) -> String {
    format!("{reason}; pass --command to name the line. nothing was changed")
}

/// Asserts a replay over `table` is refused with `error` before anything is killed or replaced.
fn assert_replay_refused(live: &mut Live, table: FakeProcessTable, error: &str) {
    let Dispatched::Done(response) = begin_replay(live, table) else {
        panic!("a refused replay waited")
    };
    assert_eq!(response, failure(error));
    assert!(live.host.kills().is_empty(), "{:?}", live.host.kills());
    assert!(live.core().take_pane_replacements().is_empty());
    assert!(live.core().zmx.claimed_exits.is_empty());
    let token = uuid_string(live.left);
    let session = live.fixture.store().session(live.session).unwrap();
    assert_eq!(session.pane_role_for_token(&token), Some(StatusPane::Left));
    let surface = session.pane_slot(StatusPane::Left).get();
    assert_eq!(surface.map(Surface::pane_token), Some(token.as_str()));
}

#[test]
fn a_replay_is_refused_before_anything_is_killed() {
    let present = tempfile::tempdir().unwrap();
    let present = present.path().to_str().unwrap().to_owned();
    let gone = tempfile::tempdir().unwrap();
    let gone_path = gone.path().to_str().unwrap().to_owned();
    gone.close().unwrap();
    let long = "a".repeat(crate::restart_replay::MAX_COMMAND_BYTES);
    let cases = [
        // the pane's root shell runs its own line
        (
            replay_table(&["/bin/sh", "-c", "read line"], Some(ROOT_SHELL)),
            "no replayable foreground program: sh holds the pane",
        ),
        (
            job_table(&["/bin/sleep", "300"], &gone_path),
            "the foreground program's working directory is unavailable",
        ),
        // a shell reading its script from stdin holds the pane
        (
            job_table(&["/bin/sh", "-s"], &present),
            "no replayable foreground program: sh holds the pane",
        ),
        (
            replay_table(&["-zsh"], None),
            "the pane's foreground program cannot be read",
        ),
        (
            job_table(&["/bin/sh", "-c", "read line", &long], &present),
            "the foreground program's command line is too long to replay (max 4096 bytes)",
        ),
    ];
    for (table, reason) in cases {
        let mut live = live();
        assert_replay_refused(&mut live, table, &refused(reason));
    }
}

#[test]
fn a_denylisted_replay_is_refused_before_anything_is_killed() {
    let config = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join(crate::command_restore::DENYLIST_FILE), "sleep\n").unwrap();
    let settings = crate::persist::AppSettings {
        config_directory: Some(config.path().to_string_lossy().into_owned()),
        ..crate::persist::AppSettings::default()
    };
    let mut live = live_over(Fixture::with_settings(0, settings), RestoreMode::Live);
    live.core().load_restore_denylist_at_launch();
    let directory = tempfile::tempdir().unwrap();
    let table = job_table(&["/bin/sleep", "300"], directory.path().to_str().unwrap());
    assert_replay_refused(&mut live, table, &refused("sleep is on the restore denylist"));
}

// MARK: - zmx.reset

#[test]
fn reset_refuses_outside_live_and_without_anything_to_reset() {
    let mut fresh = live_with(0, RestoreMode::None);
    assert_eq!(
        fresh.zmx(ZmxCmd::Reset, None, |_| {}),
        failure("zmx.reset requires --force")
    );
    assert_eq!(
        fresh.zmx(ZmxCmd::Reset, None, |a| a.force = Some(true)),
        failure("zmx.reset requires Live sessions mode both configured and active for this launch")
    );
    let mut live = live();
    live.core().save_restore_mode(RestoreMode::Live);
    live.host
        .set_record(&daemon_name(live.left), Some(1), Some(100), Some(50));
    assert_eq!(
        live.zmx(ZmxCmd::Reset, None, |a| a.force = Some(true)),
        failure("zmx.reset found no live session to reset")
    );
    live.host.fail_list("down");
    assert_eq!(
        live.zmx(ZmxCmd::Reset, None, |a| a.force = Some(true)),
        failure("zmx.reset could not read the live session list")
    );
}

#[test]
fn a_confirmed_reset_holds_the_outdated_panes_quits_and_arms_the_marker() {
    let mut live = live();
    live.core().save_restore_mode(RestoreMode::Live);
    live.core().zmx.outdated_before = Some(100);
    live.host
        .set_record(&daemon_name(live.left), Some(1), Some(100), Some(50));
    let response = live.zmx(ZmxCmd::Reset, None, |a| a.force = Some(true));
    let body = crate::zmx::reset::dialog_text(1, 1).1;
    assert_eq!(
        result(&response),
        json!({"text": body, "liveReset": {"sessions": 1, "panes": 1, "pending": true, "outdated": 1}})
    );
    assert_eq!(live.core().take_live_quit(), Some(crate::LiveQuit { relaunch: true }));
    let tree = result(&live.fixture.send(Command::Tree, None, Args::default()));
    assert_eq!(tree["tree"]["liveReset"], json!({"pending": 1}));
    assert!(live.core().arm_live_reset_on_quit());
    let marker = live.resources.path().join(crate::zmx::reset::MARKER_FILE_NAME);
    assert!(marker.exists());
    // the next launch that gets live consumes it: the marker's pane is not claimed by that launch's model, so it is
    // skipped rather than killed
    let mut next = live_with(0, RestoreMode::Live);
    next.core().save_restore_mode(RestoreMode::Live);
    let host = Arc::new(FakeSessionHost::new());
    host.set_record(&daemon_name(live.left), Some(0), Some(100), Some(50));
    install(next.core(), &host, live.resources.path(), RestoreMode::Live);
    next.core().zmx.outdated_before = Some(100);
    next.core().consume_live_reset(host.as_ref());
    assert!(!marker.exists());
    assert!(host.kills().is_empty());
    assert_eq!(next.core().zmx.reset.last.clone().unwrap().panes.skipped, 1);
}

#[test]
fn a_relauncher_that_did_not_start_disarms_the_reset() {
    let mut live = live();
    live.core().save_restore_mode(RestoreMode::Live);
    live.core().zmx.outdated_before = Some(100);
    live.host
        .set_record(&daemon_name(live.left), Some(1), Some(100), Some(50));
    assert!(live.zmx(ZmxCmd::Reset, None, |a| a.force = Some(true)).ok);
    assert!(live.core().arm_live_reset_on_quit());
    let marker = live.resources.path().join(crate::zmx::reset::MARKER_FILE_NAME);
    assert!(marker.exists());
    live.core().disarm_live_reset();
    assert!(!marker.exists(), "the next launch resets nothing");
}

/// Arms a reset of the live pane's outdated daemon (leader 100) and consumes it in the same model, whose process
/// table holds the processes `alive`.
fn consume_reset(alive: &[i32]) -> (Live, Arc<FakeSessionHost>) {
    let mut live = live();
    live.core().save_restore_mode(RestoreMode::Live);
    live.core().zmx.outdated_before = Some(100);
    let daemon = daemon_name(live.left);
    live.host.set_record(&daemon, Some(1), Some(100), Some(50));
    assert!(live.zmx(ZmxCmd::Reset, None, |a| a.force = Some(true)).ok);
    assert!(live.core().arm_live_reset_on_quit());
    let table = FakeProcessTable::default();
    for pid in alive {
        table.insert(
            ProcessStat {
                pid: *pid,
                ppid: 1,
                pgrp: *pid,
                session: *pid,
            },
            Some(&["zsh"]),
        );
    }
    live.core().set_process_table(Box::new(table), None);
    live.core().zmx.reset.consume_budget = std::time::Duration::from_millis(50);
    let host = Arc::clone(&live.host);
    live.core().consume_live_reset(host.as_ref());
    (live, host)
}

#[test]
fn a_consumed_reset_ends_the_claimed_outdated_daemon() {
    let (mut live, host) = consume_reset(&[]);
    assert_eq!(host.kills(), [(daemon_name(live.left), true)]);
    let last = live.core().zmx.reset.last.clone().unwrap();
    assert_eq!((last.panes.killed, last.unconfirmed.len()), (1, 0));
    assert!(live.core().zmx.suppressed.is_empty());
}

#[test]
fn a_leader_outliving_the_reset_suppresses_its_pane_s_payloads() {
    let (mut live, host) = consume_reset(&[100]);
    assert_eq!(host.kills(), [(daemon_name(live.left), true)]);
    let last = live.core().zmx.reset.last.clone().unwrap();
    assert_eq!(last.unconfirmed, [live.left]);
    assert!(live.fixture.core.zmx.suppressed.contains(&live.left));
    // its pane attaches with neither a replay nor its durable command
    let policy = live.core().seed_policy();
    assert!(policy.suppressed_daemons.contains(&daemon_name(live.left)));
}
