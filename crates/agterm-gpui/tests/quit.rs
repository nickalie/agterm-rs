//! SIGTERM's deadline under load (`conformance/docs/rust-build.md`: quit without a prompt within 10 seconds): the
//! GPUI build in Live mode on a private Xvfb (lavapipe) with three live sessions, one of them flooding its pane, and an
//! HTML page over two of them, one of whose engines spins in a script, quits on SIGTERM with status 0 within the
//! deadline, its quit save written (every session in the saved state), the Live sessions and their one server
//! surviving it and nothing else of the app's left running (the `WebKit` worker and web processes; the panes attach
//! in process). The time the quit took is printed (seen with `--nocapture`).
//!
//! Everything runs in a [`support::Sandbox`] (private bus, state, live-session namespace and Xvfb); its live sessions
//! are killed through their own namespace's server at the end. The app runs the cargo-built `agterm-session` beside
//! it, as the staged layout has it; without one the test fails. Without Xvfb, lavapipe and Ghostty's shell integration
//! it skips (fails under `AGTERM_REQUIRE_DISPLAY_TESTS`).

#[macro_use]
#[expect(dead_code, reason = "the sandbox serves the process tests too")]
mod support;

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use agterm_host::SessionHost as _;
use serde_json::{Value, json};
use support::{DEADLINE, Sandbox, TestResult, have, lavapipe_icd};

/// The deadline rust-build.md gives a SIGTERM quit.
const QUIT_DEADLINE: Duration = Duration::from_secs(10);

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

/// A script of the repository, run for its output (`ghostty_vt_prefix`, say).
fn script(args: &[&str]) -> Option<String> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("bash").args(args).current_dir(repo).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Ghostty's resources (with its shell integration), as `scripts/stage.sh` stages them.
fn ghostty_resources() -> Option<PathBuf> {
    let ghostty = script(&["-c", "source scripts/lib/native.sh && ghostty_vt_prefix"])?;
    let resources = PathBuf::from(ghostty).join("share/ghostty");
    resources.join("shell-integration").is_dir().then_some(resources)
}

/// The cargo-built `agterm-session` beside the app's binary (`target/<profile>/`), where the app looks for it as in
/// the staged layout's `bin/`; `cargo test` builds it with the workspace, or with `-p agterm-session`.
fn session_executable() -> TestResult<PathBuf> {
    let executable = Path::new(support::GPUI).with_file_name("agterm-session");
    if !executable.is_file() {
        return Err(format!(
            "no cargo-built {} (build it with `cargo build -p agterm-session`, or test with the workspace)",
            executable.display()
        )
        .into());
    }
    Ok(executable)
}

/// The session host of the sandbox's own namespace (never the user's): its server alone is asked.
fn session_host(executable: &Path, dir: &str) -> agterm_session::host::ServerHost {
    agterm_session::host::ServerHost::new(executable, dir)
}

/// Removes a sandbox's live-session namespace `dir` (in `/tmp`, outside the sandbox's root) once its server stopped:
/// it stops by itself within its idle grace after its last session went, and lets go of the namespace's lock then
/// (`server.lock`, held while it listens; a lock file removed under a listening server would let a second one bind).
/// Its server still listening at the deadline, the directory is left as it is.
fn remove_live_namespace(dir: &Path) {
    let deadline = Instant::now() + agterm_session::server::lifecycle::IDLE_GRACE + DEADLINE;
    let lock = dir.join(agterm_session::endpoint::LOCK_FILE_NAME);
    loop {
        let free = match std::fs::File::open(&lock) {
            Ok(file) => file.try_lock().is_ok(),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        };
        if free {
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kills every live session of the sandbox's namespace through its own server, however the test ended; the server
/// then stops by itself, and the namespace goes with it.
struct Sessions {
    executable: PathBuf,
    dir: String,
}

impl Sessions {
    /// The names of the namespace's sessions.
    fn names(&self) -> TestResult<Vec<String>> {
        Ok(session_host(&self.executable, &self.dir)
            .list()?
            .into_iter()
            .map(|record| record.name)
            .collect())
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        if let Ok(names) = self.names() {
            session_host(&self.executable, &self.dir).kill_batch(&names);
        }
        remove_live_namespace(Path::new(&self.dir));
    }
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

/// Opens a page titled `title` with `body` over session `sid` and waits for its load.
fn load_page(sandbox: &Sandbox, sid: &str, title: &str, body: &str) -> TestResult {
    let file = sandbox.path(&format!("home/{title}.html"));
    std::fs::write(&file, format!("<!doctype html><title>{title}</title>{body}"))?;
    call(
        sandbox,
        "session.overlay.open",
        Some(sid),
        json!({ "html": file.to_string_lossy() }),
    )?;
    eventually(&format!("the page {title} loaded"), || {
        call(sandbox, "tree", None, json!({})).ok().filter(|tree| {
            find(tree, sid).is_some_and(|session| {
                session["htmlOverlays"].as_array().is_some_and(|pages| {
                    pages
                        .iter()
                        .any(|page| page["state"] == json!("loaded") && page["title"] == json!(title))
                })
            })
        })
    })
    .map_err(|err| format!("{err}\n{}", sandbox.log("primary.log")))?;
    Ok(())
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

/// The live sessions' server of the app's sessions, each once: the parents of the shells `live.list` names; and how
/// many shells it names.
fn live_processes(sandbox: &Sandbox) -> TestResult<(Vec<u32>, usize)> {
    let entries = call(sandbox, "live.list", None, json!({}))?;
    let rows = entries["live"]["entries"].as_array().ok_or("no live.list entries")?;
    let shells: Vec<u32> = rows
        .iter()
        .filter_map(|row| u32::try_from(row["leaderPID"].as_u64()?).ok())
        .collect();
    let mut servers: Vec<u32> = shells.iter().copied().filter_map(parent_of).collect();
    servers.sort_unstable();
    servers.dedup();
    Ok((servers, shells.len()))
}

/// Every process under `pid`, its children's children included.
fn descendants(pid: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let mut queue = vec![pid];
    while let Some(next) = queue.pop() {
        for child in children(next) {
            found.push(child);
            queue.push(child);
        }
    }
    found
}

/// Whether process `pid` still runs (a zombie counts as gone).
fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rfind(')')
            .and_then(|end| stat.get(end + 2..end + 3))
            .is_some_and(|state| state != "Z")
    })
}

/// Every file under `dir`, read as text.
fn texts(dir: &Path) -> String {
    let mut all = String::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return all;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            all.push_str(&texts(&path));
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            all.push_str(&text);
        }
    }
    all
}

/// A primary in Live mode on a private Xvfb (lavapipe), its live sessions killed when the returned guard drops.
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

/// Three Live sessions: a page whose engine spins in a script once loaded, a quiet page, a pane flooding its output;
/// their ids, once the flood shows.
fn load_up(sandbox: &Sandbox) -> TestResult<[String; 3]> {
    let busy = live_session(sandbox)?;
    let spin = "<p>busy</p><script>setTimeout(() => { for (;;) {} }, 300)</script>";
    load_page(sandbox, &busy, "busy", spin)?;
    let quiet = live_session(sandbox)?;
    load_page(sandbox, &quiet, "quiet", "<p>quiet</p>")?;
    let flooding = live_session(sandbox)?;
    let flood = "yes 'agterm quit test: output while the app quits'\n";
    call(sandbox, "session.type", Some(&flooding), json!({ "text": flood }))?;
    eventually("the flood on screen", || {
        let text = call(sandbox, "session.text", Some(&flooding), json!({})).ok()?;
        (text["text"].as_str()?.matches("while the app quits").count() > 5).then_some(())
    })?;
    // the spinning script started meanwhile
    std::thread::sleep(Duration::from_millis(500));
    Ok([busy, quiet, flooding])
}

/// The app's processes outside its Live sessions (the session server the first pane's attach started and what runs in
/// its sessions stay); the server; and how many sessions' shells it runs.
fn app_processes(sandbox: &Sandbox, app: u32) -> TestResult<(Vec<u32>, Vec<u32>, usize)> {
    let (live, shells) = live_processes(sandbox)?;
    let kept: Vec<u32> = live
        .iter()
        .flat_map(|server| std::iter::once(*server).chain(descendants(*server)))
        .collect();
    let helpers = descendants(app).into_iter().filter(|pid| !kept.contains(pid)).collect();
    Ok((helpers, live, shells))
}

/// Waits for every one of `pids` to end; the error names the ones left.
fn wait_gone(pids: &[u32]) -> TestResult {
    eventually("the app's processes gone", || {
        pids.iter().all(|pid| !running(*pid)).then_some(())
    })
    .map_err(|err| {
        let left: Vec<String> = pids
            .iter()
            .filter(|pid| running(**pid))
            .map(|pid| {
                let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
                format!("{pid} {}", name.trim())
            })
            .collect();
        let stuck: Vec<u32> = pids.iter().copied().filter(|pid| running(*pid)).collect();
        format!("{err}: still running {left:?}\n{}", stuck_report(&stuck)).into()
    })
}

/// What holds processes `pids` up: each one's state, parent, wait channel, threads and descriptors, then the unix
/// sockets they hold (`ss -xp`) with whoever holds each one's peer.
fn stuck_report(pids: &[u32]) -> String {
    let read = |path: String| std::fs::read_to_string(path).unwrap_or_default();
    let mut report = String::new();
    for pid in pids {
        let status: Vec<String> = read(format!("/proc/{pid}/status"))
            .lines()
            .filter(|line| ["State:", "PPid:", "Threads:"].iter().any(|key| line.starts_with(key)))
            .map(str::to_owned)
            .collect();
        let threads: Vec<String> = std::fs::read_dir(format!("/proc/{pid}/task"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|task| {
                let path = task.path();
                let name = std::fs::read_to_string(path.join("comm")).unwrap_or_default();
                let wchan = std::fs::read_to_string(path.join("wchan")).unwrap_or_default();
                format!("{}:{}", name.trim(), wchan.trim())
            })
            .collect();
        let fds: Vec<String> = std::fs::read_dir(format!("/proc/{pid}/fd"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|fd| {
                let target = std::fs::read_link(fd.path()).ok()?;
                Some(format!("{}={}", fd.file_name().to_string_lossy(), target.display()))
            })
            .collect();
        let _ = writeln!(report, "{pid}: {status:?} threads {threads:?} fds {fds:?}");
    }
    let sockets = Command::new("ss")
        .args(["-xpaH"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let fields = |line: &str| line.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
    let theirs: Vec<&str> = sockets
        .lines()
        .filter(|line| pids.iter().any(|pid| line.contains(&format!("pid={pid},"))))
        .collect();
    let peers: Vec<String> = theirs.iter().filter_map(|line| fields(line).get(7).cloned()).collect();
    let holders: Vec<&str> = sockets
        .lines()
        .filter(|line| fields(line).get(5).is_some_and(|inode| peers.contains(inode)))
        .collect();
    let _ = writeln!(
        report,
        "their sockets:\n{}\ntheir peers:\n{}",
        theirs.join("\n"),
        holders.join("\n")
    );
    report
}

#[test]
fn sigterm_saves_and_quits_within_its_deadline_with_live_sessions_busy_and_pages_open() -> TestResult {
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
    let (mut sandbox, primary, sessions) = start_live(&lavapipe, &executable, &resources)?;
    let app = sandbox.child(primary).id();
    let sids = load_up(&sandbox)?;
    let (helpers, live, shells) = app_processes(&sandbox, app)?;
    assert_eq!(shells, 4, "a live session per session: the launch's own and the three");
    assert_eq!(
        live.len(),
        1,
        "one server holds every live session of the instance: {live:?}"
    );
    assert!(
        !helpers.is_empty(),
        "the app runs its panes' clients and the page engine"
    );

    sandbox.terminate(primary)?;
    let (status, took) = sandbox.wait_exit(primary, QUIT_DEADLINE)?;
    println!(
        "{}",
        json!({ "sigterm_quit_seconds": took.as_secs_f64(), "helpers": helpers.len() })
    );
    let log = sandbox.log("primary.log");
    assert_eq!(status.code(), Some(0), "{status:?}\n{log}");
    assert!(
        log.contains("SIGTERM: quitting") && log.contains("quit: the state is saved"),
        "{log}"
    );
    assert!(!sandbox.path("state/ngterm.sock").exists(), "the socket is removed");
    let saved = texts(&sandbox.path("state"));
    for sid in &sids {
        assert!(saved.contains(sid.as_str()), "session {sid} is in the saved state");
    }
    // the Live sessions and their server outlive the app; its own processes do not
    let listed = sessions.names()?;
    if listed.len() != shells || !live.iter().all(|server| running(*server)) {
        let servers: Vec<String> = live
            .iter()
            .map(|server| format!("{server} {}", if running(*server) { "running" } else { "gone" }))
            .collect();
        return Err(format!(
            "every Live session survives: listed {listed:?}, sessions {sids:?}, server {servers:?}\n{log}"
        )
        .into());
    }
    wait_gone(&helpers)?;
    drop(sessions);
    Ok(())
}
