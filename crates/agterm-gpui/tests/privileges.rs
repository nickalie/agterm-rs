//! The GPUI build keeps the user's privileges while its HTML pages run. A vendored WPE `WebKit` runs only inside an
//! overlay of `/usr/lib` (`bwrap`: a user namespace with `PR_SET_NO_NEW_PRIVS`), so its engine runs in a worker there
//! (`agterm_wpe::worker`) and nothing else may: the app itself, a Live pane's zmx daemon, its shell and what the shell
//! starts keep `NoNewPrivs: 0`, this test's uid and gid maps and user namespace, and see root's files as root's (so
//! `sudo` and every setuid program work in a pane), while a page loads, and again after the app quit, in a command the
//! surviving session runs. Under the vendored package the worker is the one process with the overlay's flag and
//! namespace, which shows the test tells them apart. Nothing runs `sudo`: `/proc` says what it would need.
//!
//! Everything runs in a [`support::Sandbox`] (private bus, state, zmx directory and Xvfb, lavapipe); its zmx sessions
//! are killed through their own zmx directory at the end. Without Xvfb, lavapipe, the vendored zmx and Ghostty's
//! shell integration, or with a login shell Live sessions do not run in, the test skips (fails under
//! `AGTERM_REQUIRE_DISPLAY_TESTS`).

#[macro_use]
#[expect(dead_code, reason = "the sandbox serves the process tests too")]
mod support;

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::{DEADLINE, Sandbox, TestResult, have, lavapipe_icd};

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

/// One control request; its result, or its error as the test's.
fn call(sandbox: &Sandbox, cmd: &str, target: Option<&str>, args: Value) -> TestResult<Value> {
    let mut socket = std::os::unix::net::UnixStream::connect(sandbox.path("state/ngterm.sock"))?;
    socket.set_read_timeout(Some(DEADLINE))?;
    let mut request = json!({ "cmd": cmd });
    if let Some(target) = target {
        request["target"] = json!(target);
    }
    if args.as_object().is_some_and(|args| !args.is_empty()) {
        request["args"] = args;
    }
    writeln!(socket, "{request}")?;
    let mut reply = String::new();
    BufReader::new(socket).read_line(&mut reply)?;
    let reply: Value = serde_json::from_str(&reply)?;
    if reply["ok"] == json!(true) {
        Ok(reply["result"].clone())
    } else {
        Err(format!("{cmd}: {reply}").into())
    }
}

/// The first object under `value` whose `id` is `id`.
fn find<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) if map.get("id").and_then(Value::as_str) == Some(id) => Some(value),
        Value::Object(map) => map.values().find_map(|child| find(child, id)),
        Value::Array(items) => items.iter().find_map(|child| find(child, id)),
        _ => None,
    }
}

/// Polls `probe` until it gives a value, for [`DEADLINE`].
fn eventually<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> TestResult<T> {
    let started = Instant::now();
    loop {
        if let Some(value) = probe() {
            return Ok(value);
        }
        if started.elapsed() > DEADLINE {
            return Err(format!("{what}: not within {DEADLINE:?}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A script of the repository, run for its output (`scripts/build-zmx.sh --print-prefix`, say).
fn script(args: &[&str]) -> Option<String> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("bash").args(args).current_dir(repo).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The vendored zmx and Ghostty's resources (with its shell integration), as `scripts/stage.sh` stages them.
fn live_inputs() -> Option<(PathBuf, PathBuf)> {
    script(&["scripts/build-zmx.sh", "--verify"])?;
    let zmx = PathBuf::from(script(&["scripts/build-zmx.sh", "--print-prefix"])?).join("bin/zmx");
    let ghostty = script(&["-c", "source scripts/lib/native.sh && ghostty_vt_prefix"])?;
    let resources = PathBuf::from(ghostty).join("share/ghostty");
    (zmx.is_file() && resources.join("shell-integration").is_dir()).then_some((zmx, resources))
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

/// zmx with the sandbox's own zmx directory (never the user's).
fn zmx(zmx: &Path, dir: &str, args: &[&str]) -> TestResult<String> {
    let output = Command::new(zmx)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("ZMX_DIR", dir)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Kills every zmx session of the sandbox's zmx directory, however the test ended.
struct Sessions {
    zmx: PathBuf,
    dir: String,
}

impl Drop for Sessions {
    fn drop(&mut self) {
        let Ok(listing) = zmx(&self.zmx, &self.dir, &["list", "--short"]) else {
            return;
        };
        for name in listing.split_whitespace() {
            let _ = zmx(&self.zmx, &self.dir, &["kill", name, "--force"]);
        }
        // the sandbox's own: named after its state dir, which goes with it
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A primary on a private Xvfb in Live mode, its zmx sessions killed when the returned guard drops.
fn start_live(lavapipe: &Path, zmx_bin: &Path, resources: &Path) -> TestResult<(Sandbox, usize, Sessions)> {
    let mut sandbox = Sandbox::new()?;
    sandbox.start_xvfb()?;
    sandbox.use_software_rendering(lavapipe);
    sandbox.set_env("AGTERM_ZMX_PATH", zmx_bin.to_string_lossy());
    sandbox.set_env("GHOSTTY_RESOURCES_DIR", resources.to_string_lossy());
    std::fs::write(sandbox.path("state/settings.json"), r#"{"restoreMode":"live"}"#)?;
    let sessions = Sessions {
        zmx: zmx_bin.to_owned(),
        dir: agterm_core::zmx::socket_directory(&sandbox.path("state")),
    };
    let primary = sandbox.start_gpui_primary("primary.log")?;
    Ok((sandbox, primary, sessions))
}

/// A new session, once its pane runs in zmx; its id.
fn live_session(sandbox: &Sandbox) -> TestResult<String> {
    let sid = call(sandbox, "session.new", None, json!({}))?["id"]
        .as_str()
        .ok_or("no session id")?
        .to_owned();
    eventually("the session in zmx", || {
        call(sandbox, "tree", None, json!({}))
            .ok()
            .filter(|tree| find(tree, &sid).is_some_and(|session| session["backedByZmx"] == json!(true)))
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

/// The pid of the shell zmx runs for session `sid`.
fn shell_of(sandbox: &Sandbox, sid: &str) -> TestResult<u32> {
    let entries = call(sandbox, "zmx.list", None, json!({}))?;
    entries["zmx"]["entries"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["sessionID"] == json!(sid)))
        .and_then(|row| row["leaderPID"].as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| "no shell pid in zmx.list".into())
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
    let Some((zmx_bin, resources)) = live_inputs() else {
        skip!("no vendored zmx (scripts/build-zmx.sh) or Ghostty shell integration");
    };
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
    let (mut sandbox, primary, sessions) = start_live(&lavapipe, &zmx_bin, &resources)?;
    let app = sandbox.child(primary).id();
    let sid = live_session(&sandbox)?;
    // a page loads meanwhile
    load_page(&sandbox, &sid)?;

    // the pane's command, the shell, its zmx daemon and the app: all as this test
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
    let daemon = parent_of(shell).ok_or("the shell has no parent")?;
    for (what, pid) in [
        ("the app", app),
        ("the zmx daemon", daemon),
        ("the pane's shell", shell),
    ] {
        assert_eq!(Identity::of(&pid.to_string())?, outside, "{what} ({pid})");
    }
    check_overlaid(app, &outside, vendored)?;

    // the quit: SIGTERM, status 0, the quit save written; the daemon and its shell live on as the user's
    sandbox.terminate(primary)?;
    let (status, _) = sandbox.wait_exit(primary, Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(0), "{}", sandbox.log("primary.log"));
    assert!(sandbox.path("state/windows.json").is_file(), "the quit save ran");
    assert_eq!(Identity::of(&daemon.to_string())?, outside, "the surviving daemon");
    let after = sandbox.path("home/after.txt");
    let name = zmx(&zmx_bin, &sessions.dir, &["list", "--short"])?
        .split_whitespace()
        .next()
        .ok_or("no surviving zmx session")?
        .to_owned();
    zmx(&zmx_bin, &sessions.dir, &["send", &name, &report_command(&after)])?;
    let (survivor, root_owner) = report(&after, "the surviving session's report")?;
    assert_eq!(survivor, outside, "a command the surviving session runs after the quit");
    assert_eq!(root_owner, "0");
    drop(sessions);
    Ok(())
}
