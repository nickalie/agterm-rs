//! What the Live-mode process tests (`quit.rs`, `live_update.rs`, `privileges.rs`) share: control requests and their
//! tree, the cargo-built `ngterm-session` and Ghostty's resources, and the guard that ends a sandbox's live sessions
//! through its own namespace's server and removes the namespace once that server stopped. Nothing here reaches past
//! the sandbox: its control socket, its namespace's server alone.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ngterm_host::SessionHost as _;
use serde_json::{Value, json};

use super::{DEADLINE, GPUI, Sandbox, TestResult};

/// One control request; its result, or its error as the test's.
pub fn call(sandbox: &Sandbox, cmd: &str, target: Option<&str>, args: Value) -> TestResult<Value> {
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
pub fn find<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) if map.get("id").and_then(Value::as_str) == Some(id) => Some(value),
        Value::Object(map) => map.values().find_map(|child| find(child, id)),
        Value::Array(items) => items.iter().find_map(|child| find(child, id)),
        _ => None,
    }
}

/// Polls `probe` until it gives a value, for [`DEADLINE`]; the error names `what`.
pub fn eventually<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> TestResult<T> {
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
pub fn ghostty_resources() -> Option<PathBuf> {
    let ghostty = script(&["-c", "source scripts/lib/native.sh && ghostty_vt_prefix"])?;
    let resources = PathBuf::from(ghostty).join("share/ghostty");
    resources.join("shell-integration").is_dir().then_some(resources)
}

/// The cargo-built `ngterm-session` beside the app's binary (`target/<profile>/`), where the app looks for it as in
/// the staged layout's `bin/`; `cargo test` builds it with the workspace, or with `-p ngterm-session`.
pub fn session_executable() -> TestResult<PathBuf> {
    let executable = Path::new(GPUI).with_file_name("ngterm-session");
    if !executable.is_file() {
        return Err(format!(
            "no cargo-built {} (build it with `cargo build -p ngterm-session`, or test with the workspace)",
            executable.display()
        )
        .into());
    }
    Ok(executable)
}

/// The session host of the sandbox's own namespace (never the user's): its server alone is asked.
pub fn session_host(executable: &Path, dir: &str) -> ngterm_session::host::ServerHost {
    ngterm_session::host::ServerHost::new(executable, dir)
}

/// Removes a sandbox's live-session namespace `dir` (in `/tmp`, outside the sandbox's root) once its server stopped:
/// it stops by itself within its idle grace after its last session went, and lets go of the namespace's lock then
/// (`server.lock`, held while it listens; a lock file removed under a listening server would let a second one bind).
/// Its server still listening at the deadline, the directory is left as it is.
pub fn remove_live_namespace(dir: &Path) {
    let deadline = Instant::now() + ngterm_session::server::lifecycle::IDLE_GRACE + DEADLINE;
    let lock = dir.join(ngterm_session::endpoint::LOCK_FILE_NAME);
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
pub struct Sessions {
    /// The session server the namespace runs.
    pub executable: PathBuf,
    /// The sandbox's live-session namespace (`ngterm_core::live::socket_directory` of its state dir).
    pub dir: String,
}

impl Sessions {
    /// The names of the namespace's sessions.
    pub fn names(&self) -> TestResult<Vec<String>> {
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
