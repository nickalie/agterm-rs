//! The GPUI build keeps the user's privileges while its HTML pages run. A vendored WPE `WebKit` runs only inside an
//! overlay of `/usr/lib` (`bwrap`: a user namespace with `PR_SET_NO_NEW_PRIVS`), so its engine runs in a worker there
//! (`agterm_wpe::worker`) and nothing else may: the app itself, the live sessions' server (`agterm-session`), a Live
//! pane's shell and what the shell starts keep `NoNewPrivs: 0`, this test's uid and gid maps and user namespace, and
//! see root's files as root's (so `sudo` and every setuid program work in a pane), while a page loads, and again after
//! the app quit, in a command the surviving session runs. Under the vendored package the worker is the one process
//! with the overlay's flag and namespace, which shows the test tells them apart. Nothing runs `sudo`: `/proc` says what it would need.
//!
//! Everything runs in a [`support::Sandbox`] (private bus, state, live-session namespace and Xvfb, lavapipe); its live
//! sessions are killed through their own namespace's server at the end. The app runs the cargo-built `agterm-session`
//! beside it, as the staged layout has it; without one the test fails. Without Xvfb, lavapipe and Ghostty's shell
//! integration, or with a login shell Live sessions do not run in, it skips (fails under
//! `AGTERM_REQUIRE_DISPLAY_TESTS`).

#[macro_use]
#[expect(dead_code, reason = "the sandbox serves the process tests too")]
mod support;

use std::path::Path;
use std::time::Duration;

use agterm_host::SessionHost as _;
use serde_json::json;
use support::live::{Sessions, call, eventually, find, ghostty_resources, session_executable, session_host};
use support::{Sandbox, TestResult, have, lavapipe_icd};

/// What makes a process privileged the way the user's login is: read from `/proc`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    no_new_privs: String,
    uid_map: String,
    gid_map: String,
    user_namespace: String,
}

impl Identity {
    /// The identity of process `pid` (`self` for this one).
    fn of(pid: &str) -> TestResult<Self> {
        let proc = Path::new("/proc").join(pid);
        let status = std::fs::read_to_string(proc.join("status"))?;
        let no_new_privs = status
            .lines()
            .find_map(|line| line.strip_prefix("NoNewPrivs:"))
            .ok_or("no NoNewPrivs line")?
            .trim()
            .to_owned();
        Ok(Self {
            no_new_privs,
            uid_map: normalized(&std::fs::read_to_string(proc.join("uid_map"))?),
            gid_map: normalized(&std::fs::read_to_string(proc.join("gid_map"))?),
            user_namespace: std::fs::read_link(proc.join("ns/user"))?.to_string_lossy().into_owned(),
        })
    }

    /// The identity a pane's command reported (the lines `REPORT` writes).
    fn reported(text: &str) -> TestResult<(Self, String)> {
        let field = |name: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(name))
                .map(|value| value.trim().to_owned())
                .ok_or_else(|| format!("no {name} in the report:\n{text}"))
        };
        let identity = Self {
            no_new_privs: field("NoNewPrivs:")?,
            uid_map: normalized(&field("uid_map:")?),
            gid_map: normalized(&field("gid_map:")?),
            user_namespace: field("userns:")?,
        };
        Ok((identity, field("root_owner:")?))
    }
}

/// A map's fields, whitespace collapsed.
fn normalized(map: &str) -> String {
    map.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The command a pane runs to report its identity into `path` (one line per field, every shell's syntax).
fn report_command(path: &Path) -> String {
    format!(
        "sh -c 'grep NoNewPrivs /proc/self/status; printf \"uid_map: %s\\n\" \"$(cat /proc/self/uid_map)\"; printf \
         \"gid_map: %s\\n\" \"$(cat /proc/self/gid_map)\"; printf \"userns: %s\\n\" \"$(readlink /proc/self/ns/user)\"; \
         printf \"root_owner: %s\\n\" \"$(stat -c %u /)\"; echo done' > {}.part && mv {}.part {}\n",
        path.display(),
        path.display(),
        path.display()
    )
}

/// The pids whose parent is `parent`.
fn children(parent: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    // the fields after the command, which may hold spaces and parentheses itself
                    let rest = &stat[stat.rfind(')')? + 2..];
                    rest.split_whitespace().nth(1)?.parse::<u32>().ok()
                })
                == Some(parent)
        })
        .collect()
}

/// The parent of `pid`.
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// A primary on a private Xvfb in Live mode, its live sessions killed when the returned guard drops.
fn start_live(lavapipe: &Path, executable: &Path, resources: &Path) -> TestResult<(Sandbox, usize, Sessions)> {
    let mut sandbox = Sandbox::new()?;
    sandbox.start_xvfb()?;
    sandbox.use_software_rendering(lavapipe);
    sandbox.set_env("GHOSTTY_RESOURCES_DIR", resources.to_string_lossy());
    std::fs::write(sandbox.path("state/settings.json"), r#"{"restoreMode":"live"}"#)?;
    let sessions = Sessions {
        executable: executable.to_owned(),
        dir: agterm_core::live::socket_directory(&sandbox.path("state")),
    };
    let primary = sandbox.start_gpui_primary("primary.log")?;
    Ok((sandbox, primary, sessions))
}

/// A new session, once its pane runs in a live session; its id.
fn live_session(sandbox: &Sandbox) -> TestResult<String> {
    let sid = call(sandbox, "session.new", None, json!({}))?["id"]
        .as_str()
        .ok_or("no session id")?
        .to_owned();
    eventually("the session live", || {
        call(sandbox, "tree", None, json!({}))
            .ok()
            .filter(|tree| find(tree, &sid).is_some_and(|session| session["backedByLive"] == json!(true)))
    })
    .map_err(|err| format!("{err}\n{}", sandbox.log("primary.log")))?;
    Ok(sid)
}

/// Opens a page over session `sid` and waits for its load.
fn load_page(sandbox: &Sandbox, sid: &str) -> TestResult {
    let file = sandbox.path("home/page.html");
    std::fs::write(&file, "<!doctype html><title>privileges</title><p>a page</p>")?;
    call(
        sandbox,
        "session.overlay.open",
        Some(sid),
        json!({ "html": file.to_string_lossy() }),
    )?;
    eventually("the page loaded", || {
        call(sandbox, "tree", None, json!({})).ok().filter(|tree| {
            find(tree, sid).is_some_and(|session| {
                session["htmlOverlays"][0]["state"] == json!("loaded")
                    && session["htmlOverlays"][0]["title"] == json!("privileges")
            })
        })
    })
    .map_err(|err| format!("{err}\n{}", sandbox.log("primary.log")))?;
    Ok(())
}

/// What a command reported into `path` once it wrote it all.
fn report(path: &Path, what: &str) -> TestResult<(Identity, String)> {
    let text = eventually(what, || {
        std::fs::read_to_string(path).ok().filter(|text| text.contains("done"))
    })?;
    Identity::reported(&text)
}

/// The pid of the shell the live session of session `sid` runs.
fn shell_of(sandbox: &Sandbox, sid: &str) -> TestResult<u32> {
    let entries = call(sandbox, "live.list", None, json!({}))?;
    entries["live"]["entries"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["sessionID"] == json!(sid)))
        .and_then(|row| row["leaderPID"].as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| "no shell pid in live.list".into())
}

/// The app's processes (its children and theirs) in another user namespace than `outside`'s: under the vendored
/// package the worker alone (bwrap's child: bwrap itself stays in this namespace, the worker's own children are
/// `WebKit`'s), with bwrap's `no_new_privs`; none with the system's.
fn check_overlaid(app: u32, outside: &Identity, vendored: bool) -> TestResult {
    let overlaid: Vec<u32> = children(app)
        .into_iter()
        .flat_map(|child| std::iter::once(child).chain(children(child)))
        .filter(|pid| {
            Identity::of(&pid.to_string()).is_ok_and(|identity| identity.user_namespace != outside.user_namespace)
        })
        .collect();
    if !vendored {
        assert_eq!(
            overlaid,
            [] as [u32; 0],
            "nothing of the app's runs in another user namespace"
        );
        return Ok(());
    }
    let [worker] = overlaid[..] else {
        return Err(format!("not one process under the overlay among the app's: {overlaid:?}").into());
    };
    let identity = Identity::of(&worker.to_string())?;
    assert_eq!(identity.no_new_privs, "1", "the worker runs under bwrap's no_new_privs");
    Ok(())
}

#[test]
fn panes_and_their_sessions_keep_the_users_privileges_while_pages_run() -> TestResult {
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
    let outside = Identity::of("self")?;
    assert_eq!(
        outside.no_new_privs, "0",
        "this test itself runs with no_new_privs: run it outside any sandbox"
    );
    let vendored = matches!(
        agterm_wpe::launcher::provider(),
        agterm_wpe::launcher::Provider::Vendored { .. }
    );
    println!("WPE WebKit: {}", if vendored { "vendored" } else { "the system's" });
    let (mut sandbox, primary, sessions) = start_live(&lavapipe, &executable, &resources)?;
    let app = sandbox.child(primary).id();
    let sid = live_session(&sandbox)?;
    // a page loads meanwhile
    load_page(&sandbox, &sid)?;

    // the pane's command, the shell, the session server and the app: all as this test
    let before = sandbox.path("home/before.txt");
    call(
        &sandbox,
        "session.type",
        Some(&sid),
        json!({ "text": report_command(&before) }),
    )?;
    let (command, root_owner) = report(&before, "the pane's report")?;
    assert_eq!(command, outside, "a pane's command runs as the user's login does");
    assert_eq!(root_owner, "0", "root's files are root's in a pane");
    let shell = shell_of(&sandbox, &sid)?;
    let server = parent_of(shell).ok_or("the shell has no parent")?;
    for (what, pid) in [
        ("the app", app),
        ("the session server", server),
        ("the pane's shell", shell),
    ] {
        assert_eq!(Identity::of(&pid.to_string())?, outside, "{what} ({pid})");
    }
    check_overlaid(app, &outside, vendored)?;

    // the quit: SIGTERM, status 0, the quit save written; the server and the shell live on as the user's
    sandbox.terminate(primary)?;
    let (status, _) = sandbox.wait_exit(primary, Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(0), "{}", sandbox.log("primary.log"));
    assert!(sandbox.path("state/windows.json").is_file(), "the quit save ran");
    assert_eq!(Identity::of(&server.to_string())?, outside, "the surviving server");
    let after = sandbox.path("home/after.txt");
    let name = sessions
        .names()?
        .into_iter()
        .next()
        .ok_or("no surviving live session")?;
    session_host(&executable, &sessions.dir).type_input(&name, report_command(&after).as_bytes())?;
    let (survivor, root_owner) = report(&after, "the surviving session's report")?;
    assert_eq!(survivor, outside, "a command the surviving session runs after the quit");
    assert_eq!(root_owner, "0");
    drop(sessions);
    Ok(())
}
