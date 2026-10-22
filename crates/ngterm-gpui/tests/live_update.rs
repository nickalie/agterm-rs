//! Live sessions through the app across its relaunches: an update handoff, a prune and a confirmed reset. The GPUI
//! build in Live mode on a private Xvfb (lavapipe) runs a staged `ngterm-session` (a copy of the cargo-built one with
//! a build id staged beside it, as `scripts/stage.sh` stages it, named by the debug build's `NGTERM_SESSION_PATH`):
//!
//! - **The update**: the app quits with a live session running under build A and is relaunched on build B. Its launch
//!   asks A's server to hand the sessions over (`session_host::update`): B's server runs the same shell (its leader
//!   pid), the pane reattaches with its screen and takes input, the build record says the sessions were handed over,
//!   so a reset finds none predating the build. Before the quit, an orphan session no pane claims is pruned and the
//!   claimed one stays.
//! - **The reset**: with every session predating the build's record (seeded: a handoff that failed leaves sessions so),
//!   `live.reset --force` quits the app, which reopens itself, and the reopened instance ends the old session and
//!   starts the pane on a new one.
//!
//! Everything runs in a [`support::Sandbox`] (private bus, state, live-session namespace and Xvfb); the live sessions
//! are killed through their own namespace's server at the end, and a reopened instance is stopped. Without Xvfb,
//! lavapipe and Ghostty's shell integration it skips (fails under `NGTERM_REQUIRE_DISPLAY_TESTS`).

#[macro_use]
#[expect(dead_code, reason = "the sandbox serves the process tests too")]
mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use ngterm_session::build_id::Staged;
use ngterm_session::client::Client;
use ngterm_session::protocol::{AttachRequest, GridSize, ReplyMetadata, SessionSpec};
use ngterm_session::server::spawn::StartOptions;
use serde_json::{Value, json};
use support::live::{Sessions, call, eventually, find, ghostty_resources, session_executable};
use support::{GPUI, Sandbox, TestResult, have, lavapipe_icd};

/// How long a quit may take.
const QUIT_DEADLINE: Duration = Duration::from_secs(10);

/// A staged root `<sandbox>/<name>` holding a copy of `executable` as `bin/ngterm-session` with the build `id` staged
/// beside it, renamed into place as `scripts/stage.sh` does; the staged executable.
fn stage(sandbox: &Sandbox, executable: &Path, name: &str, id: &str) -> TestResult<PathBuf> {
    let staged = sandbox.path(name).join("bin/ngterm-session");
    std::fs::create_dir_all(staged.parent().ok_or("no parent")?)?;
    let temporary = staged.with_extension("new");
    std::fs::copy(executable, &temporary)?;
    std::fs::rename(&temporary, &staged)?;
    Staged::write_beside(&staged, id)?;
    Ok(staged)
}

/// The build id and pid of the namespace's running server.
fn server(dir: &str) -> Option<(String, u32)> {
    let api = Client::new(dir).api().ok()??;
    Some((api.welcome.build, api.welcome.pid))
}

/// A sandbox ready for a Live primary on a private Xvfb (lavapipe) running the staged session server `staged`, and
/// the guard of its namespace's sessions.
fn live_sandbox(lavapipe: &Path, resources: &Path, staged: &Path, sandbox: &mut Sandbox) -> TestResult<Sessions> {
    sandbox.start_xvfb()?;
    sandbox.use_software_rendering(lavapipe);
    sandbox.set_env("GHOSTTY_RESOURCES_DIR", resources.to_string_lossy());
    sandbox.set_env(ngterm_session::launch::EXECUTABLE_OVERRIDE, staged.to_string_lossy());
    std::fs::write(sandbox.path("state/settings.json"), r#"{"restoreMode":"live"}"#)?;
    Ok(Sessions {
        executable: staged.to_owned(),
        dir: ngterm_core::live::socket_directory(&sandbox.path("state")),
    })
}

/// A new session, once its pane runs in a live session; its id.
fn live_session(sandbox: &Sandbox, log: &str) -> TestResult<String> {
    let sid = call(sandbox, "session.new", None, json!({}))?["id"]
        .as_str()
        .ok_or("no session id")?
        .to_owned();
    backed(sandbox, &sid).map_err(|err| format!("{err}\n{}", sandbox.log(log)))?;
    Ok(sid)
}

/// Waits until session `sid`'s panes run in live sessions.
fn backed(sandbox: &Sandbox, sid: &str) -> TestResult {
    eventually("the session live", || {
        call(sandbox, "tree", None, json!({}))
            .ok()
            .filter(|tree| find(tree, sid).is_some_and(|session| session["backedByLive"] == json!(true)))
            .map(|_| ())
    })
}

/// Session `sid`'s left pane's row of `live.list`, once its daemon is observed running.
fn running_row(sandbox: &Sandbox, sid: &str) -> TestResult<Value> {
    eventually("the pane's running daemon", || {
        let listing = call(sandbox, "live.list", None, json!({})).ok()?;
        listing["live"]["entries"]
            .as_array()?
            .iter()
            .find(|row| {
                row["sessionID"] == json!(sid) && row["pane"] == json!("left") && row["observation"] == json!("running")
            })
            .cloned()
    })
}

/// Types a line printing `marker` (split, so the echoed line never matches) into session `sid` and waits for it.
fn print_marker(sandbox: &Sandbox, sid: &str, marker: &str) -> TestResult {
    let (head, tail) = marker.split_at(marker.len() / 2);
    let line = format!("printf '%s%s\\n' {head} {tail}\n");
    call(sandbox, "session.type", Some(sid), json!({ "text": line }))?;
    shows(sandbox, sid, marker)
}

/// Waits until session `sid`'s screen or scrollback holds a line that is `marker`.
fn shows(sandbox: &Sandbox, sid: &str, marker: &str) -> TestResult {
    eventually(&format!("{marker} on the screen of {sid}"), || {
        let text = call(sandbox, "session.text", Some(sid), json!({ "all": true })).ok()?;
        text["text"]
            .as_str()?
            .lines()
            .any(|line| line.trim_end() == marker)
            .then_some(())
    })
}

/// Creates the session `name` running `sleep` in the namespace `dir` through the client library and detaches: a
/// session no pane claims.
fn orphan(dir: &str, executable: &Path, name: &str) -> TestResult {
    let attachment = Client::new(dir)
        .starting(StartOptions::new(executable))
        .attach(AttachRequest {
            name: name.to_owned(),
            create: Some(SessionSpec {
                argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "exec sleep 600".to_owned()],
                env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
                cwd: None,
                tracked_env: Vec::new(),
            }),
            size: GridSize { cols: 80, rows: 24 },
            managed: None,
            metadata: ReplyMetadata::default(),
        })?;
    attachment.writer.detach()?;
    Ok(())
}

/// The process `pid` runs (a zombie counts as gone).
fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .is_ok_and(|stat| stat.rsplit_once(") ").is_some_and(|(_, rest)| !rest.starts_with('Z')))
}

/// SIGTERMs the primary at `index`, waits for its exit within the quit deadline and checks it quit cleanly.
fn quit(sandbox: &mut Sandbox, index: usize, log: &str) -> TestResult {
    sandbox.terminate(index)?;
    let (status, _) = sandbox.wait_exit(index, QUIT_DEADLINE)?;
    assert_eq!(status.code(), Some(0), "{status:?}\n{}", sandbox.log(log));
    Ok(())
}

#[test]
fn an_update_hands_the_live_sessions_over_to_the_new_build_across_a_relaunch() -> TestResult {
    if !have("Xvfb") {
        skip!("no Xvfb");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let Some(resources) = ghostty_resources() else {
        skip!("no Ghostty shell integration (scripts/build-ghostty-vt.sh)");
    };
    let executable = session_executable()?;
    let mut sandbox = Sandbox::new()?;
    let build_a = stage(&sandbox, &executable, "stage-a", "live-update-test build a")?;
    let build_b = stage(&sandbox, &executable, "stage-b", "live-update-test build b")?;
    let mut sessions = live_sandbox(&lavapipe, &resources, &build_a, &mut sandbox)?;
    let first = sandbox.start_gpui_primary("first.log")?;
    let sid = live_session(&sandbox, "first.log")?;
    print_marker(&sandbox, &sid, "beforeupdate")?;
    let before = running_row(&sandbox, &sid)?;
    let (build, old_server) = server(&sessions.dir).ok_or("no server runs")?;
    assert_eq!(build, "live-update-test build a");
    prune_an_orphan(&sandbox, &sessions, &before)?;
    quit(&mut sandbox, first, "first.log")?;

    sandbox.set_env(ngterm_session::launch::EXECUTABLE_OVERRIDE, build_b.to_string_lossy());
    sessions.executable.clone_from(&build_b);
    let second = sandbox.start_gpui_primary("second.log")?;
    sandbox.wait_log("second.log", "live sessions handed over to this build")?;
    let (build, new_server) = server(&sessions.dir).ok_or("no server runs after the update")?;
    assert_eq!(build, "live-update-test build b");
    assert_ne!(new_server, old_server, "a server of the new build runs the sessions");
    backed(&sandbox, &sid)?;
    let after = running_row(&sandbox, &sid)?;
    assert_eq!(
        (&after["daemon"], &after["leaderPID"]),
        (&before["daemon"], &before["leaderPID"]),
        "the same session and shell came through the update"
    );
    shows(&sandbox, &sid, "beforeupdate")?;
    print_marker(&sandbox, &sid, "afterupdate")?;
    let record: Value = serde_json::from_slice(&std::fs::read(sandbox.path("state/session-build.json"))?)?;
    assert_eq!(
        (&record["id"], &record["handedOver"]),
        (&json!("live-update-test build b"), &json!(true))
    );
    let refused = call(&sandbox, "live.reset", None, json!({ "force": true }))
        .err()
        .ok_or("a reset after a committed handoff found sessions to reset")?;
    assert!(
        refused
            .to_string()
            .contains("live.reset found no live session to reset"),
        "{refused}"
    );
    quit(&mut sandbox, second, "second.log")?;
    drop(sessions);
    Ok(())
}

/// A session no pane claims, in the app's namespace and named as its panes' are, is pruned; the claimed session of
/// `claimed` (a `live.list` row) stays.
fn prune_an_orphan(sandbox: &Sandbox, sessions: &Sessions, claimed: &Value) -> TestResult {
    let name = ngterm_core::live::daemon_name(uuid::Uuid::new_v4());
    orphan(&sessions.dir, &sessions.executable, &name)?;
    eventually("the orphan listed", || {
        let listing = call(sandbox, "live.list", None, json!({})).ok()?;
        listing["live"]["entries"]
            .as_array()?
            .iter()
            .any(|row| row["daemon"] == json!(name) && row["state"] == json!("orphan"))
            .then_some(())
    })?;
    let pruned = call(sandbox, "live.prune", None, json!({}))?;
    assert_eq!(pruned, json!({ "text": format!("killed {name}"), "affected": 1 }));
    let names = sessions.names()?;
    assert!(!names.contains(&name), "{names:?}");
    let daemon = claimed["daemon"].as_str().ok_or("no daemon")?;
    assert!(names.iter().any(|listed| listed == daemon), "{names:?}");
    Ok(())
}

/// The reopened instance of the sandbox's app: the process running the app's executable on the sandbox's state
/// directory, other than `old`; stopped when the guard drops, and only while it still is that process.
struct Reopened {
    pid: u32,
    app: PathBuf,
    state: String,
}

impl Reopened {
    fn find(sandbox: &Sandbox, old: u32) -> TestResult<Self> {
        let app = std::fs::canonicalize(GPUI)?;
        let state = format!("NGTERM_STATE_DIR={}", sandbox.path("state").display());
        let pid = eventually("the reopened app", || {
            let pids = std::fs::read_dir("/proc")
                .ok()?
                .flatten()
                .filter_map(|entry| entry.file_name().to_str()?.parse().ok());
            reopened_pid(pids, &app, &state, old)
        })?;
        Ok(Self { pid, app, state })
    }
}

impl Drop for Reopened {
    fn drop(&mut self) {
        // never a process that is not the sandbox's app, nor one its pid was reused for
        if !runs_app(self.pid, &self.app, &self.state) {
            return;
        }
        if let Ok(pid) = libc::pid_t::try_from(self.pid) {
            // SAFETY: `kill` sends a signal only; the pid ran the sandbox's app a moment ago.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        let deadline = Instant::now() + QUIT_DEADLINE;
        while running(self.pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Whether `pid` runs `app` with `state` (the sandbox's `NGTERM_STATE_DIR=` pair) in its environment.
fn runs_app(pid: u32, app: &Path, state: &str) -> bool {
    let (Ok(exe), Ok(environ)) = (
        std::fs::read_link(format!("/proc/{pid}/exe")),
        std::fs::read(format!("/proc/{pid}/environ")),
    ) else {
        return false;
    };
    exe == app && environ.split(|byte| *byte == 0).any(|pair| pair == state.as_bytes()) && running(pid)
}

/// The first of `pids` other than `old` that runs `app` with `state`. A plain pid, never a [`Reopened`]: a guard
/// built for every process the scan reads and dropped for each one that does not match signals each of them.
fn reopened_pid(pids: impl IntoIterator<Item = u32>, app: &Path, state: &str, old: u32) -> Option<u32> {
    pids.into_iter().find(|&pid| pid != old && runs_app(pid, app, state))
}

/// A `sleep` of the test's own, with `state` in its environment when given; the scan's and the guard's checks read
/// only it, so a regression can reach no other process.
fn bystander(state: Option<&str>) -> TestResult<(std::process::Child, PathBuf)> {
    let mut command = Command::new("sleep");
    command.arg("30");
    if let Some((key, value)) = state.and_then(|state| state.split_once('=')) {
        command.env(key, value);
    }
    let child = command.spawn()?;
    // `exe` names the new program before its environment is in place: wait for both
    let exe = eventually("the bystander's executable", || {
        let exe = std::fs::read_link(format!("/proc/{}/exe", child.id())).ok()?;
        let environ = std::fs::read(format!("/proc/{}/environ", child.id())).ok()?;
        let settled = state.map_or(!environ.is_empty(), |state| {
            environ.split(|byte| *byte == 0).any(|pair| pair == state.as_bytes())
        });
        (exe.ends_with("sleep") && settled).then_some(exe)
    })?;
    Ok((child, exe))
}

#[test]
fn the_scan_for_the_reopened_app_finds_only_the_sandboxs_app_and_signals_nothing() -> TestResult {
    let state = "NGTERM_STATE_DIR=/nonexistent/live-update-scan";
    let (mut stranger, exe) = bystander(None)?;
    assert_eq!(reopened_pid([stranger.id()], &exe, state, 0), None);
    assert!(running(stranger.id()), "the scan signalled a process it did not match");
    let (mut app, exe) = bystander(Some(state))?;
    assert_eq!(reopened_pid([stranger.id(), app.id()], &exe, state, 0), Some(app.id()));
    assert_eq!(
        reopened_pid([app.id()], &exe, state, app.id()),
        None,
        "the old instance is not the reopened one"
    );
    assert!(running(stranger.id()) && running(app.id()));
    for child in [&mut stranger, &mut app] {
        child.kill()?;
        child.wait()?;
    }
    Ok(())
}

#[test]
fn the_reopened_guard_stops_only_the_sandboxs_app() -> TestResult {
    let state = "NGTERM_STATE_DIR=/nonexistent/live-update-guard";
    let (mut stranger, exe) = bystander(None)?;
    drop(Reopened {
        pid: stranger.id(),
        app: exe.clone(),
        state: state.to_owned(),
    });
    assert!(
        running(stranger.id()),
        "the guard signalled a process that is not the sandbox's app"
    );
    stranger.kill()?;
    stranger.wait()?;
    let (mut app, exe) = bystander(Some(state))?;
    drop(Reopened {
        pid: app.id(),
        app: exe,
        state: state.to_owned(),
    });
    assert!(!running(app.id()), "the guard left the sandbox's app running");
    app.wait()?;
    Ok(())
}

#[test]
fn a_confirmed_reset_reopens_the_app_on_new_sessions() -> TestResult {
    if !have("Xvfb") {
        skip!("no Xvfb");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let Some(resources) = ghostty_resources() else {
        skip!("no Ghostty shell integration (scripts/build-ghostty-vt.sh)");
    };
    let executable = session_executable()?;
    let mut sandbox = Sandbox::new()?;
    let id = "live-reset-test build";
    let staged = stage(&sandbox, &executable, "stage", id)?;
    let sessions = live_sandbox(&lavapipe, &resources, &staged, &mut sandbox)?;
    // this build's record dated after every session the test starts: they predate it, as a failed handoff leaves them
    let changed_at = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs() + 3600;
    std::fs::write(
        sandbox.path("state/session-build.json"),
        json!({ "id": id, "changedAt": changed_at }).to_string(),
    )?;
    let primary = sandbox.start_gpui_primary("primary.log")?;
    let app = sandbox.child(primary).id();
    let sid = live_session(&sandbox, "primary.log")?;
    let before = running_row(&sandbox, &sid)?;
    let old_leader = before["leaderPID"].as_u64().ok_or("no leader")?;
    let reset = call(&sandbox, "live.reset", None, json!({ "force": true }))?;
    assert_eq!(reset["liveReset"]["pending"], json!(true), "{reset}");
    let (status, _) = sandbox.wait_exit(primary, QUIT_DEADLINE)?;
    assert_eq!(status.code(), Some(0), "{status:?}\n{}", sandbox.log("primary.log"));

    let reopened = Reopened::find(&sandbox, app)?;
    support::wait_for_socket(&sandbox.path("state/ngterm.sock"))?;
    backed(&sandbox, &sid)?;
    let after = eventually("the pane on a new session", || {
        running_row(&sandbox, &sid)
            .ok()
            .filter(|row| row["leaderPID"].as_u64().is_some_and(|leader| leader != old_leader))
    })?;
    assert_eq!(after["daemon"], before["daemon"], "the pane keeps its session's name");
    assert!(
        !running(u32::try_from(old_leader)?),
        "the reset ended the old session's shell"
    );
    print_marker(&sandbox, &sid, "afterreset")?;
    drop(reopened);
    drop(sessions);
    Ok(())
}
