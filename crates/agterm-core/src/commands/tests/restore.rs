//! `restore.mode`, `restore.capture`, `restore.clear` (upstream's `ControlServerRestoreModeTests`,
//! `ControlServerRestoreCaptureTests`), and the whole relaunch chain on the fakes: capture at quit, save, relaunch,
//! and the seeds the panes spawn with.

use std::path::Path;
use std::time::{Duration, Instant};

use agterm_host::ProcessStat;
use agterm_host::fakes::{FakeProcessTable, FakeSurface};
use agterm_proto::command::{RestoreCmd, SessionCmd};
use agterm_proto::{AppIdentity, Args, Command, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Fixture, answered_id, args, failure, send, wire};
use crate::persist::{AppSettings, LibraryPersistence, RestoreMode};
use crate::restore::mode::RestoreLaunchDecision;
use crate::restore::spawn_plan::LaunchSeed;
use crate::{Core, CoreConfig, CoreState};

fn restore(core: &mut Core, cmd: RestoreCmd, edit: impl FnOnce(&mut Args)) -> Response {
    send(core, Command::Restore(cmd), None, args(edit))
}

fn status(response: &Response) -> Value {
    assert!(response.ok, "{response:?}");
    response.result.clone().expect("a result")["restore"].clone()
}

fn settings(mode: RestoreMode) -> AppSettings {
    AppSettings {
        restore_mode: Some(mode),
        ..AppSettings::default()
    }
}

fn stat(pid: i32, parent: i32, pgrp: i32, session: i32) -> ProcessStat {
    ProcessStat {
        pid,
        ppid: parent,
        pgrp,
        session,
    }
}

#[test]
fn the_mode_reads_the_launch_and_a_set_applies_to_the_next_one() {
    let mut fixture = Fixture::new(0);
    let none = json!({"configured": "none", "requestedAtLaunch": "none", "active": "none", "restartRequired": false});
    assert_eq!(status(&restore(&mut fixture.core, RestoreCmd::Mode, |_| {})), none);
    for mode in ["rerun", "live"] {
        let set = status(&restore(&mut fixture.core, RestoreCmd::Mode, |a| {
            a.mode = Some(mode.into());
        }));
        let expected =
            json!({"configured": mode, "requestedAtLaunch": "none", "active": "none", "restartRequired": true});
        assert_eq!(set, expected);
        assert_eq!(status(&restore(&mut fixture.core, RestoreCmd::Mode, |_| {})), expected);
    }
    assert_eq!(
        status(&restore(&mut fixture.core, RestoreCmd::Mode, |a| a.mode = Some("none".into()))),
        none
    );
    assert_eq!(
        restore(&mut fixture.core, RestoreCmd::Mode, |a| a.mode = Some("Rerun".into())),
        failure("invalid restore mode: Rerun")
    );
    assert_eq!(fixture.core.settings().effective_restore_mode(), RestoreMode::None);
}

#[test]
fn a_refused_live_request_reports_why_and_runs_fresh_shells() {
    let mut fixture = Fixture::with_settings(0, settings(RestoreMode::Live));
    let reason = "the zmx executable is unavailable";
    fixture
        .core
        .set_restore_launch(RestoreLaunchDecision::new(RestoreMode::Live, Some(reason.to_owned())));
    assert_eq!(
        status(&restore(&mut fixture.core, RestoreCmd::Mode, |_| {})),
        json!({"configured": "live", "requestedAtLaunch": "live", "active": "none", "restartRequired": false,
               "unavailableReason": reason})
    );
    // switching away keeps the reason off: it is only ever about a live request this launch made
    let set = status(&restore(&mut fixture.core, RestoreCmd::Mode, |a| {
        a.mode = Some("rerun".into());
    }));
    assert_eq!(set["restartRequired"], json!(true));
    assert_eq!(set["unavailableReason"], json!(reason));
}

fn persistent_core(dir: &Path, settings: AppSettings) -> Core {
    let mut persistence = LibraryPersistence::new(dir);
    let library = persistence.bootstrap("/home/u");
    let mut core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings,
            persistence: Some(persistence),
        },
    );
    let _ = core.commit();
    core
}

#[test]
fn a_set_mode_is_written_and_one_that_cannot_be_is_refused_and_rolled_back() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut core = persistent_core(dir.path(), AppSettings::default());
    assert!(restore(&mut core, RestoreCmd::Mode, |a| a.mode = Some("rerun".into())).ok);
    let text = std::fs::read_to_string(dir.path().join("settings.json")).expect("the settings");
    assert!(
        text.contains(r#""restoreMode":"rerun""#) || text.contains(r#""restoreMode" : "rerun""#),
        "{text}"
    );
    std::fs::remove_file(dir.path().join("settings.json")).expect("removed");
    std::fs::create_dir(dir.path().join("settings.json")).expect("a directory in its place");
    assert_eq!(
        restore(&mut core, RestoreCmd::Mode, |a| a.mode = Some("live".into())),
        failure("could not save the restore mode; settings keep rerun")
    );
    assert_eq!(core.settings().effective_restore_mode(), RestoreMode::Rerun);
}

#[test]
fn a_capture_needs_rerun_configured() {
    for (mode, name) in [(RestoreMode::None, "none"), (RestoreMode::Live, "live")] {
        let mut fixture = Fixture::with_settings(0, settings(mode));
        assert_eq!(
            restore(&mut fixture.core, RestoreCmd::Capture, |_| {}),
            failure(&format!(
                "restore.capture requires rerun mode; configured restore mode is {name}"
            ))
        );
    }
}

/// A pane's terminal on the fakes, carrying `token` and reporting foreground group `pgid`.
fn terminal(token: &str, pgid: Option<u32>) -> FakeSurface {
    let surface = FakeSurface::new(token);
    surface.probe().set_realized(true);
    surface.probe().set_foreground_pid(pgid);
    surface
}

fn install(core: &mut Core, session: Uuid, pgid: Option<u32>) -> String {
    let window = core.library().window_holding_session(session).expect("a window");
    let target = core
        .library_mut()
        .store_mut(window)
        .and_then(|store| store.session_mut(session))
        .expect("the session");
    let token = wire(target.pane_identity);
    let _ = target.surface.replace(Box::new(terminal(&token, pgid)));
    token
}

/// Three panes: `cat` typed into a shell, `--command cat` and a shell at its prompt.
fn processes() -> FakeProcessTable {
    let table = FakeProcessTable::default();
    table.insert(stat(100, 1, 100, 100), Some(&["sh"]));
    table.insert(stat(101, 100, 101, 100), Some(&["cat"]));
    table.insert(stat(200, 1, 200, 200), Some(&["cat"]));
    table.insert(stat(300, 1, 300, 300), Some(&["sh"]));
    table
}

#[test]
fn a_capture_counts_the_programs_typed_into_shells_and_a_clear_drops_them() {
    let mut fixture = Fixture::with_settings(2, settings(RestoreMode::Rerun));
    fixture
        .core
        .set_process_table(Box::new(processes()), Some("sh".to_owned()));
    let ids = fixture.sessions.clone();
    for (id, pgid) in ids.iter().zip([101, 200, 300]) {
        install(&mut fixture.core, *id, Some(pgid));
    }
    let response = restore(&mut fixture.core, RestoreCmd::Capture, |_| {});
    assert_eq!(response.result, Some(json!({"count": 1, "text": "captured 1 pane"})));
    let captured = |fixture: &Fixture| {
        ids.iter()
            .map(|id| {
                fixture
                    .store()
                    .session(*id)
                    .expect("a session")
                    .restore
                    .foreground
                    .clone()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(captured(&fixture), vec![Some(vec!["cat".to_owned()]), None, None]);
    // a pin is not a capture: it stays
    fixture
        .store_mut()
        .and_then(|store| store.session_mut(ids[0]))
        .expect("a session")
        .restore
        .command = Some("htop".to_owned());
    assert_eq!(
        restore(&mut fixture.core, RestoreCmd::Clear, |_| {}),
        Response::success()
    );
    assert_eq!(captured(&fixture), vec![None, None, None]);
    assert_eq!(
        fixture
            .store()
            .session(ids[0])
            .expect("a session")
            .restore
            .command
            .as_deref(),
        Some("htop")
    );
}

fn new_session(core: &mut Core, command: Option<&str>, wait: bool) -> Uuid {
    let response = send(
        core,
        Command::Session(SessionCmd::New),
        None,
        args(|a| {
            a.command = command.map(str::to_owned);
            a.wait = wait.then_some(true);
        }),
    );
    answered_id(&response).expect("a new session")
}

/// The whole chain on the fakes: the quit captures what runs, saves it, and the next launch spawns each pane with its
/// seed: the `--command` sessions re-run their creation command on the exec path (with and without the hold), the job
/// typed into a shell is typed again once, and only the selected session spawns at once while the others wait their
/// paced turn.
#[test]
fn a_rerun_relaunch_respawns_commands_and_replays_the_typed_job_once() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut core = persistent_core(dir.path(), settings(RestoreMode::Rerun));
    let seeded = core
        .library()
        .all_open_sessions()
        .first()
        .map(|session| session.id())
        .expect("the seeded session");
    let held = new_session(&mut core, Some("cat"), true);
    let plain = new_session(&mut core, Some("cat"), false);
    let typed = new_session(&mut core, None, false);
    let table = FakeProcessTable::default();
    table.insert(stat(100, 1, 100, 100), Some(&["cat"]));
    table.insert(stat(110, 1, 110, 110), Some(&["cat"]));
    table.insert(stat(120, 1, 120, 120), Some(&["sh"]));
    table.insert(stat(130, 120, 130, 120), Some(&["sleep", "600"]));
    core.set_process_table(Box::new(table), Some("sh".to_owned()));
    for (id, pgid) in [(held, 100), (plain, 110), (typed, 130)] {
        install(&mut core, id, Some(pgid));
    }
    core.capture_on_exit();
    assert!(core.save_all());

    let mut core = persistent_core(dir.path(), settings(RestoreMode::Rerun));
    core.set_restore_launch(RestoreLaunchDecision::new(RestoreMode::Rerun, None));
    let start = Instant::now();
    core.arm_launch_spawns(start);
    let tokens: Vec<String> = [seeded, held, plain, typed]
        .into_iter()
        .map(|id| install(&mut core, id, None))
        .collect();
    let replay = LaunchSeed {
        initial_input: Some("'sleep' '600'\n".to_owned()),
        ..LaunchSeed::default()
    };
    assert_eq!(
        core.request_pane_spawn(typed, &tokens[3], start),
        Some(replay),
        "the selected one, at once"
    );
    assert_eq!(
        core.request_pane_spawn(seeded, &tokens[0], start),
        Some(LaunchSeed::default())
    );
    assert_eq!(core.request_pane_spawn(held, &tokens[1], start), None);
    assert_eq!(core.request_pane_spawn(plain, &tokens[2], start), None);
    let interval = Duration::from_millis(120);
    assert_eq!(core.take_spawn_wake(start), Some(interval));
    core.spawn_wake(start + interval);
    let cat = |wait| LaunchSeed {
        command: Some("cat".to_owned()),
        initial_input: None,
        wait_after_command: wait,
    };
    let grants = core.take_spawn_grants();
    assert_eq!(
        grants
            .iter()
            .map(|grant| (grant.session, grant.seed.clone()))
            .collect::<Vec<_>>(),
        vec![(held, cat(true))]
    );
    assert_eq!(grants[0].token, tokens[1]);
    assert!(core.take_spawn_wake(start + interval).is_some());
    core.spawn_wake(start + interval * 2);
    let grants = core.take_spawn_grants();
    assert_eq!(grants.len(), 1);
    assert_eq!((grants[0].session, grants[0].seed.clone()), (plain, cat(false)));
    // consumed once, and stripped from the file at launch
    let window = core.library().window_holding_session(typed).expect("a window");
    let session = core
        .library()
        .store(window)
        .and_then(|store| store.session(typed))
        .expect("typed");
    assert_eq!(session.restore.pending_foreground, None);
    let file = std::fs::read_to_string(dir.path().join(format!("windows/{}.json", wire(window)))).expect("the file");
    assert!(!file.contains("sleep"), "{file}");
}

#[test]
fn a_fresh_shells_relaunch_neither_reruns_commands_nor_paces() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut core = persistent_core(dir.path(), settings(RestoreMode::None));
    let held = new_session(&mut core, Some("cat"), true);
    assert!(core.save_all());
    let mut core = persistent_core(dir.path(), settings(RestoreMode::None));
    core.set_restore_launch(RestoreLaunchDecision::new(RestoreMode::None, None));
    let start = Instant::now();
    core.arm_launch_spawns(start);
    let token = install(&mut core, held, None);
    assert_eq!(
        core.request_pane_spawn(held, &token, start),
        Some(LaunchSeed::default())
    );
    assert_eq!(core.take_spawn_wake(start), None);
}

/// Upstream's `willClose` arms: a window closed while another stays open leaves no argv in its file, since a launch
/// could not tell it from one open at exit; the last window's close captures as the quit would.
#[test]
fn a_window_close_clears_its_captures_unless_it_is_the_last() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut core = persistent_core(dir.path(), settings(RestoreMode::Rerun));
    let table = FakeProcessTable::default();
    table.insert(stat(120, 1, 120, 120), Some(&["sh"]));
    table.insert(stat(130, 120, 130, 120), Some(&["sleep", "600"]));
    core.set_process_table(Box::new(table), None);
    let first = core.library().active_window_id().expect("a window");
    let id = core.library().all_open_sessions()[0].id();
    install(&mut core, id, Some(130));
    let second = core.library_mut().new_window(None);
    let other = core
        .library()
        .store(second)
        .expect("open")
        .sessions()
        .next()
        .expect("a session")
        .id();
    install(&mut core, other, Some(130));
    let file = |window: Uuid| {
        std::fs::read_to_string(dir.path().join(format!("windows/{}.json", wire(window)))).expect("the window file")
    };
    core.library_mut()
        .store_mut(first)
        .and_then(|store| store.session_mut(id))
        .expect("a session")
        .restore
        .foreground = Some(vec!["vim".to_owned()]);
    core.close_window(first);
    let _ = core.commit();
    assert!(!file(first).contains("vim"), "{}", file(first));
    core.close_window(second);
    let _ = core.commit();
    assert!(file(second).contains("sleep"), "{}", file(second));
}
