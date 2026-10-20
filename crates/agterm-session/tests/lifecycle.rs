//! The server's lifecycle with the real `agterm-session` binary, each test in a private namespace: a client that finds
//! no server starts one, a second finds it, racing starts leave one, the server outlives its launcher (on Windows,
//! also the launcher's kill-on-close job, through breakaway or WMI), keeps nothing of the launcher's, and stops by
//! itself once it holds no client.
//!
//! Tests that need a launcher of their own run this test binary again as a child that runs only that test, in a
//! mode its environment names ([`child_mode`]). Every server a test started is waited for at its end, and only one
//! still running then whose command line names the test's own namespace is stopped (`Namespace`'s drop).
//!
//! herdr: the tests of `src/server/autodetect.rs` and of `launch_server_daemon_with_wmi` and
//! `current_process_is_detached_server_daemon` in `src/platform/windows.rs` at `HERDR_REV` (`build/pins.env`),
//! licensed under the Apache License 2.0, modified: agterm's server binary and namespace, launchers in jobs.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use agterm_session::endpoint::Endpoint;
use agterm_session::protocol::{ErrorCode, Hello, Role};
use agterm_session::server::lifecycle::IDLE_GRACE;
use agterm_session::server::spawn::{
    Connected, SpawnPath, StartError, StartOptions, connect_existing, connect_or_start,
};
use agterm_session::server::{CAPABILITY_DETACHED, HANDSHAKE_TIMEOUT};
use serde_json::{Value, json};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The server binary under test.
const EXE: &str = env!("CARGO_BIN_EXE_agterm-session");

/// The child's mode: `launcher`, `sleep`, `descriptors` (Unix) or `racers` (Windows).
const MODE: &str = "AGLS_TEST_MODE";
/// The namespace directory a child works in.
const DIRECTORY: &str = "AGLS_TEST_DIRECTORY";
/// Where a child writes what it found, as JSON.
const CAPTURE: &str = "AGLS_TEST_CAPTURE";
/// How long a launcher holds its connection before it ends by itself, in seconds.
const HOLD: &str = "AGLS_TEST_HOLD";
/// The jobs a Windows launcher enters, outermost first, comma-separated: `k` kills on close, `b` allows breakaway.
#[cfg(windows)]
const JOBS: &str = "AGLS_TEST_JOBS";

/// How long a server may take to go once it is due to.
const EXIT_SLACK: Duration = Duration::from_secs(10);

fn hello(role: Role) -> Hello {
    Hello {
        role,
        build: "lifecycle-test".to_owned(),
        capabilities: Vec::new(),
    }
}

fn detached(connected: &Connected) -> bool {
    connected
        .welcome
        .capabilities
        .iter()
        .any(|capability| capability == CAPABILITY_DETACHED)
}

/// A private namespace, and the servers seen in it.
struct Namespace {
    root: TempDir,
    endpoint: Endpoint,
    servers: Vec<u32>,
}

impl Namespace {
    fn new() -> TestResult<Self> {
        // short on Unix, where a socket path holds about a hundred bytes
        #[cfg(unix)]
        let root = tempfile::Builder::new().prefix("agls-").tempdir_in("/tmp")?;
        #[cfg(windows)]
        let root = tempfile::tempdir()?;
        let endpoint = Endpoint::new(root.path().join("live"));
        Ok(Self {
            root,
            endpoint,
            servers: Vec::new(),
        })
    }

    fn directory(&self) -> &Path {
        self.endpoint.directory()
    }

    /// Connects in `role`, starting a server when none answers; every server seen is remembered.
    fn start(&mut self, role: Role) -> Result<Connected, StartError> {
        let result = connect_or_start(&self.endpoint, &hello(role), &StartOptions::new(EXE));
        match &result {
            Ok(connected) => self.remember(connected),
            Err(StartError::Refused { welcome, .. }) => self.servers.push(welcome.pid),
            Err(_) => {}
        }
        result
    }

    fn remember(&mut self, connected: &Connected) {
        self.servers.push(connected.welcome.pid);
        self.servers.extend(connected.spawned.iter().map(|started| started.pid));
    }

    /// The running server, without starting one.
    fn find(&mut self) -> TestResult<Option<Connected>> {
        let found = connect_existing(&self.endpoint, &hello(Role::Control), Duration::from_secs(10))?;
        if let Some(connected) = &found {
            self.servers.push(connected.welcome.pid);
        }
        Ok(found)
    }

    /// This test binary again, as a child running only `test` in `mode` in this namespace, and where it reports.
    fn child(&self, test: &str, mode: &str, extra: &[(&str, &OsStr)]) -> TestResult<(Command, PathBuf)> {
        let capture = self.root.path().join(format!("{mode}.json"));
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", test, "--test-threads=1"])
            .env_clear()
            .envs(scrubbed_environment())
            .env(MODE, mode)
            .env(DIRECTORY, self.directory())
            .env(CAPTURE, &capture)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in extra {
            command.env(name, value);
        }
        Ok((command, capture))
    }
}

impl Drop for Namespace {
    fn drop(&mut self) {
        let deadline = Instant::now() + IDLE_GRACE + EXIT_SLACK + EXIT_SLACK;
        for &pid in &self.servers {
            if !process::gone_by(pid, deadline) {
                process::stop_if_of(pid, self.endpoint.directory());
            }
        }
    }
}

/// This process's environment without the variables of a live session it may run in (CLAUDE.md: never reach the
/// user's running agterm).
fn scrubbed_environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| {
            let name = name.to_string_lossy().to_ascii_uppercase();
            !name.starts_with("AGTERM_") && !name.starts_with("ZMX_")
        })
        .collect()
}

/// Waits for `capture` to hold a child's JSON report, or for the child to end without one.
fn read_capture(capture: &Path, child: &mut Child) -> TestResult<Value> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(report) = fs::read_to_string(capture)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
        {
            return Ok(report);
        }
        if let Some(status) = child.try_wait()? {
            let output = child_output(child);
            return Err(format!("the child ended ({status}) without a report:\n{output}").into());
        }
        if Instant::now() >= deadline {
            return Err("the child wrote no report in time".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// What an ended child printed.
fn child_output(child: &mut Child) -> String {
    use std::io::Read as _;
    let mut output = String::new();
    if let Some(stdout) = child.stdout.as_mut() {
        let _ = stdout.read_to_string(&mut output);
    }
    if let Some(stderr) = child.stderr.as_mut() {
        let _ = stderr.read_to_string(&mut output);
    }
    output
}

fn report_pid(report: &Value) -> TestResult<u32> {
    let pid = report["pid"].as_u64().ok_or_else(|| format!("no pid in {report}"))?;
    Ok(u32::try_from(pid)?)
}

fn env_path(name: &str) -> TestResult<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os(name).ok_or_else(|| format!("{name} is not set"))?,
    ))
}

/// What a child of this test binary does, when it is one: true then, and the test returns at once.
fn child_mode() -> TestResult<bool> {
    let Some(mode) = std::env::var_os(MODE) else {
        return Ok(false);
    };
    let directory = env_path(DIRECTORY)?;
    let report = match mode.to_string_lossy().as_ref() {
        "launcher" => launch(&directory)?,
        #[cfg(windows)]
        "racers" => jobs::race(&directory)?,
        "sleep" => {
            thread::sleep(Duration::from_secs(120));
            json!({})
        }
        #[cfg(unix)]
        "descriptors" => descriptors::check(&directory)?,
        other => return Err(format!("unknown child mode {other}").into()),
    };
    fs::write(env_path(CAPTURE)?, report.to_string())?;
    Ok(true)
}

/// A launcher: (on Windows, inside the jobs `JOBS` names, beside a canary that sleeps in them) starts the server,
/// reports it, and holds its connection until it is stopped or `HOLD` runs out.
fn launch(directory: &Path) -> TestResult<Value> {
    #[cfg(windows)]
    let (was_in_job, canary) = jobs::enter_and_start_canary()?;
    let connected = connect_or_start(
        &Endpoint::new(directory),
        &hello(Role::Control),
        &StartOptions::new(EXE),
    );
    let mut report = match &connected {
        Ok(connected) => json!({
            "pid": connected.welcome.pid,
            "path": connected.started_here().map(|started| format!("{:?}", started.path)),
            "detached": detached(connected),
        }),
        Err(error) => json!({ "error": error.to_string() }),
    };
    #[cfg(windows)]
    {
        report["was_in_job"] = was_in_job.into();
        report["canary"] = canary.into();
    }
    #[cfg(unix)]
    {
        // SAFETY: getpgrp takes no arguments and cannot fail.
        report["group"] = unsafe { libc::getpgrp() }.into();
    }
    // the report goes out first: the parent may stop this process at any moment from then on
    fs::write(env_path(CAPTURE)?, report.to_string())?;
    if let Some(hold) = std::env::var_os(HOLD) {
        thread::sleep(Duration::from_secs(hold.to_string_lossy().parse()?));
    }
    drop(connected);
    Ok(report)
}

/// How a start from this test process goes: detached, or on Windows from a test run inside a job (a CI runner's),
/// by breakaway or, where that job refuses it, by WMI.
fn plain_paths() -> Vec<SpawnPath> {
    #[cfg(windows)]
    if jobs::current_process_is_in_job() {
        return vec![SpawnPath::Breakaway, SpawnPath::Wmi];
    }
    vec![SpawnPath::Detached]
}

#[test]
fn a_client_that_finds_no_server_starts_one_and_the_next_finds_it() {
    if child_mode().unwrap() {
        return;
    }
    let mut namespace = Namespace::new().unwrap();
    let first = namespace.start(Role::Control).unwrap();
    let started = first.started_here().expect("the first client started the server");
    assert!(plain_paths().contains(&started.path), "{started:?}");
    assert!(detached(&first), "{:?}", first.welcome);
    let second = namespace.start(Role::Control).unwrap();
    assert_eq!(second.welcome.pid, first.welcome.pid);
    assert!(second.spawned.is_empty(), "{:?}", second.spawned);
    assert_eq!(namespace.find().unwrap().unwrap().welcome.pid, first.welcome.pid);
}

#[test]
fn two_racing_starts_leave_one_server() {
    if child_mode().unwrap() {
        return;
    }
    let mut namespace = Namespace::new().unwrap();
    let racers = 4;
    let barrier = Arc::new(Barrier::new(racers));
    let threads: Vec<_> = (0..racers)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let endpoint = namespace.endpoint.clone();
            thread::spawn(move || {
                barrier.wait();
                connect_or_start(&endpoint, &hello(Role::Control), &StartOptions::new(EXE))
            })
        })
        .collect();
    let results: Vec<Connected> = threads
        .into_iter()
        .map(|racer| racer.join().unwrap().unwrap())
        .collect();
    for connected in &results {
        namespace.remember(connected);
    }
    let winner = results[0].welcome.pid;
    assert!(
        results.iter().all(|connected| connected.welcome.pid == winner),
        "every racer is connected to one server"
    );
    let starters = results
        .iter()
        .filter(|connected| connected.started_here().is_some())
        .count();
    assert_eq!(starters, 1, "exactly one racer started the server everyone uses");
    // every other server started lost the namespace and is gone
    let deadline = Instant::now() + EXIT_SLACK;
    for started in results.iter().flat_map(|connected| &connected.spawned) {
        if started.pid != winner {
            assert!(
                process::gone_by(started.pid, deadline),
                "the loser {} lives on",
                started.pid
            );
        }
    }
    assert!(process::alive(winner));
}

#[test]
fn the_empty_server_exits_and_a_held_one_does_not() {
    if child_mode().unwrap() {
        return;
    }
    let mut namespace = Namespace::new().unwrap();
    let held = namespace.start(Role::Control).unwrap();
    let pid = held.welcome.pid;
    // a client holds it past the idle grace
    thread::sleep(IDLE_GRACE + Duration::from_secs(1));
    assert!(process::alive(pid));
    drop(held);
    // within its grace the server is still there for the next client
    let again = namespace.find().unwrap().expect("the server waits out its grace");
    assert_eq!(again.welcome.pid, pid);
    drop(again);
    let left = Instant::now();
    assert!(
        process::gone_by(pid, left + IDLE_GRACE + EXIT_SLACK),
        "the empty server is still running"
    );
    assert!(left.elapsed() >= IDLE_GRACE.saturating_sub(Duration::from_millis(500)));
    assert!(namespace.find().unwrap().is_none(), "its endpoint is gone with it");
    #[cfg(unix)]
    assert!(!namespace.endpoint.socket_path().exists());
}

#[test]
fn a_role_the_server_does_not_know_is_refused_with_its_welcome() {
    if child_mode().unwrap() {
        return;
    }
    let mut namespace = Namespace::new().unwrap();
    // a newer client's role: it goes on the wire under a name this server does not know
    let error = namespace.start(Role::Unknown).unwrap_err();
    let StartError::Refused { error, welcome, .. } = error else {
        panic!("{error}");
    };
    assert_eq!(error.code, ErrorCode::UnsupportedRole);
    assert!(welcome.pid > 0);
}

#[test]
fn the_launcher_ends_and_the_server_lives() {
    if child_mode().unwrap() {
        return;
    }
    let mut namespace = Namespace::new().unwrap();
    let (mut command, capture) = namespace
        .child(
            "the_launcher_ends_and_the_server_lives",
            "launcher",
            &[(HOLD, OsStr::new("120"))],
        )
        .unwrap();
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut launcher = command.spawn().unwrap();
    let report = read_capture(&capture, &mut launcher).unwrap();
    let pid = report_pid(&report).unwrap();
    namespace.servers.push(pid);
    assert_eq!(report["detached"], true, "{report}");
    // the launcher's whole process group goes, as a closing terminal takes it
    #[cfg(unix)]
    {
        let group = i32::try_from(report["group"].as_i64().unwrap()).unwrap();
        // SAFETY: kill takes no pointers; the group is the launcher's own, made for this test.
        assert_eq!(unsafe { libc::kill(-group, libc::SIGKILL) }, 0);
    }
    #[cfg(windows)]
    launcher.kill().unwrap();
    launcher.wait().unwrap();
    let found = namespace.find().unwrap().expect("the server outlived its launcher");
    assert_eq!(found.welcome.pid, pid);
}

#[test]
fn a_relative_directory_names_one_namespace_in_the_client_and_its_server() {
    if child_mode().unwrap() {
        return;
    }
    let mut namespace = Namespace::new().unwrap();
    // `sub` is there for Unix, whose kernel walks `sub/..`; Windows resolves it from the path alone
    fs::create_dir_all(namespace.root.path().join("sub")).unwrap();
    let relative = Path::new("sub").join("..").join("live");
    let (mut command, capture) = namespace
        .child(
            "a_relative_directory_names_one_namespace_in_the_client_and_its_server",
            "launcher",
            &[(DIRECTORY, relative.as_os_str()), (HOLD, OsStr::new("120"))],
        )
        .unwrap();
    // the launcher works where the relative directory starts; the server it starts works elsewhere
    command.current_dir(namespace.root.path());
    let mut launcher = command.spawn().unwrap();
    let report = read_capture(&capture, &mut launcher).unwrap();
    assert!(report["error"].is_null(), "{report}");
    let pid = report_pid(&report).unwrap();
    namespace.servers.push(pid);
    let found = namespace
        .find()
        .unwrap()
        .expect("the server serves the namespace its launcher named");
    assert_eq!(found.welcome.pid, pid);
    launcher.kill().unwrap();
    launcher.wait().unwrap();
}

#[test]
fn a_server_that_never_answers_ends_the_wait_at_the_deadline() {
    if child_mode().unwrap() {
        return;
    }
    let namespace = Namespace::new().unwrap();
    // a hung server, or another process holding the endpoint: it accepts and never answers
    let listener = namespace.endpoint.bind().unwrap();
    thread::spawn(move || {
        let mut held = Vec::new();
        while let Ok(stream) = listener.accept() {
            held.push(stream);
        }
    });
    let endpoint = namespace.endpoint.clone();
    let (send, waited) = mpsc::channel();
    let timeout = Duration::from_secs(1);
    thread::spawn(move || {
        let begun = Instant::now();
        let found = connect_existing(&endpoint, &hello(Role::Control), timeout).map(|found| found.is_some());
        let found_after = begun.elapsed();
        let options = StartOptions {
            timeout,
            ..StartOptions::new(EXE)
        };
        let begun = Instant::now();
        let started = connect_or_start(&endpoint, &hello(Role::Control), &options).map(|connected| connected.spawned);
        let _ = send.send((found, found_after, started, begun.elapsed()));
    });
    let (found, found_after, started, started_after) = waited
        .recv_timeout(HANDSHAKE_TIMEOUT * 2 + EXIT_SLACK)
        .expect("the waits did not end");
    assert!(matches!(found, Err(StartError::NotReady { .. })), "{found:?}");
    // a busy endpoint is not a missing server: nothing was started
    assert!(matches!(started, Err(StartError::NotReady { .. })), "{started:?}");
    // each wait runs to its deadline and no further: the handshake is cut to the time left, not given its own timeout
    for waited in [found_after, started_after] {
        assert!(
            waited >= timeout && waited < HANDSHAKE_TIMEOUT,
            "waited {waited:?} for a {timeout:?} timeout"
        );
    }
}

/// The descriptor check, in a child of its own: no other test's descriptors can be inherited there.
#[cfg(unix)]
mod descriptors {
    use super::*;

    /// Starts a server while this process holds a pipe whose write end is not close-on-exec, then closes that end:
    /// the read end sees the end of the stream at once only if the server did not inherit it.
    pub(super) fn check(directory: &Path) -> TestResult<Value> {
        let mut ends = [0; 2];
        // SAFETY: `ends` is writable for the two descriptors pipe(2) makes, without O_CLOEXEC.
        if unsafe { libc::pipe(ends.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let [read, write] = ends;
        let mut spawned = agterm_session::server::spawn::spawn_server(Path::new(EXE), directory)?;
        let connected = connect_within(directory, Duration::from_secs(15))?;
        // SAFETY: the write end is this process's, closed once.
        unsafe { libc::close(write) };
        let mut poll = libc::pollfd {
            fd: read,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the call.
        let ready = unsafe { libc::poll(&raw mut poll, 1, 5000) };
        let mut byte = 0_u8;
        let read_result = if ready > 0 {
            // SAFETY: the read end is open and `byte` is writable for one byte.
            unsafe { libc::read(read, (&raw mut byte).cast(), 1) }
        } else {
            -1
        };
        let pid = spawned.pid();
        let report = json!({
            "pid": pid,
            "inherited": read_result != 0,
            "detached": detached(&connected),
            "session_leader": session_leader(pid),
            "responsible_for_itself": responsible_for_itself(pid),
            "exited": spawned.try_exit().is_some(),
        });
        drop(connected);
        Ok(report)
    }

    fn connect_within(directory: &Path, timeout: Duration) -> TestResult<Connected> {
        let deadline = Instant::now() + timeout;
        loop {
            let endpoint = Endpoint::new(directory);
            if let Some(connected) = connect_existing(&endpoint, &hello(Role::Control), Duration::from_secs(5))? {
                return Ok(connected);
            }
            if Instant::now() >= deadline {
                return Err("the server never answered".into());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Whether `pid` leads its own session, where `/proc` tells.
    fn session_leader(pid: u32) -> Option<bool> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // pid (comm) state ppid pgrp session ...
        let after_comm = stat.get(stat.rfind(')')? + 2..)?;
        let session: u32 = after_comm.split(' ').nth(3)?.parse().ok()?;
        Some(session == pid)
    }

    #[cfg(target_os = "macos")]
    fn responsible_for_itself(pid: u32) -> Option<bool> {
        let pid = i32::try_from(pid).ok()?;
        agterm_session::server::spawn::responsible_pid(pid).map(|responsible| responsible == pid)
    }

    #[cfg(not(target_os = "macos"))]
    fn responsible_for_itself(_pid: u32) -> Option<bool> {
        None
    }
}

#[cfg(unix)]
#[test]
fn the_server_leads_its_own_session_and_inherits_no_descriptor() {
    if child_mode().unwrap() {
        return;
    }
    let mut namespace = Namespace::new().unwrap();
    let (mut command, capture) = namespace
        .child(
            "the_server_leads_its_own_session_and_inherits_no_descriptor",
            "descriptors",
            &[],
        )
        .unwrap();
    let mut child = command.spawn().unwrap();
    let report = read_capture(&capture, &mut child).unwrap();
    namespace.servers.push(report_pid(&report).unwrap());
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}: {}", child_output(&mut child));
    assert_eq!(report["inherited"], false, "{report}");
    assert_eq!(report["detached"], true, "{report}");
    assert_eq!(report["exited"], false, "{report}");
    #[cfg(target_os = "linux")]
    assert_eq!(report["session_leader"], true, "{report}");
    // the spawn attributes: the server checks its session and its responsibility for its `detached` report; the
    // responsibility is read here too
    #[cfg(target_os = "macos")]
    assert_eq!(report["responsible_for_itself"], true, "{report}");
}

/// Process checks by pid, for the servers the tests started.
mod process {
    use std::path::Path;
    use std::thread;
    use std::time::{Duration, Instant};

    /// Whether `pid` is gone by `deadline`.
    pub(super) fn gone_by(pid: u32, deadline: Instant) -> bool {
        while alive(pid) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
        true
    }

    #[cfg(target_os = "linux")]
    pub(super) fn alive(pid: u32) -> bool {
        // a zombie has ended; its parent (a launcher gone, then a subreaper) has not reaped it yet
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                let end = stat.rfind(')')?;
                stat.get(end + 2..end + 3).map(|state| state != "Z")
            })
            .unwrap_or(false)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn alive(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: kill with signal 0 only checks; it takes no pointers.
        let checked = unsafe { libc::kill(pid, 0) };
        checked == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(windows)]
    pub(super) fn alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        // SAFETY: OpenProcess takes no pointers; a null result is checked.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        // SAFETY: the handle was just opened with SYNCHRONIZE.
        let running = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT;
        // SAFETY: the handle was opened above and is closed once.
        unsafe { CloseHandle(handle) };
        running
    }

    /// Stops `pid` if it still runs and its command line names `directory`: a server of this test's namespace, never
    /// anything else that took its pid.
    pub(super) fn stop_if_of(pid: u32, directory: &Path) {
        let Some(command_line) = command_line(pid) else {
            return;
        };
        if !command_line.contains(&*directory.to_string_lossy()) {
            return;
        }
        eprintln!("stopping the test's leftover server {pid}: {command_line}");
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(pid) {
            // SAFETY: kill takes no pointers; the pid's command line names this test's namespace.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output();
        }
    }

    #[cfg(target_os = "linux")]
    fn command_line(pid: u32) -> Option<String> {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(String::from_utf8_lossy(&raw).replace('\0', " "))
    }

    #[cfg(target_os = "macos")]
    fn command_line(pid: u32) -> Option<String> {
        let output = std::process::Command::new("ps")
            .args(["-o", "args=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[cfg(windows)]
    fn command_line(pid: u32) -> Option<String> {
        let query = format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine");
        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &query])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Windows jobs: a launcher inside kill-on-close jobs must leave a server that outlives them.
#[cfg(windows)]
mod jobs {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    use super::*;

    pub(super) fn current_process_is_in_job() -> bool {
        let mut in_job = 0;
        // SAFETY: GetCurrentProcess returns an always valid pseudo-handle.
        let current = unsafe { GetCurrentProcess() };
        // SAFETY: a valid process handle, a null job for any job, and a writable result.
        let asked = unsafe { IsProcessInJob(current, null_mut(), &raw mut in_job) };
        asked != 0 && in_job != 0
    }

    /// Enters the jobs `JOBS` names, outermost first, then starts a canary in them that only sleeps. Returns whether
    /// this process was in a job before, and the canary's pid (0 without `JOBS`). The job handles stay open until this
    /// process ends, which closes the jobs and so kills what is left in them.
    pub(super) fn enter_and_start_canary() -> TestResult<(bool, u32)> {
        let was_in_job = current_process_is_in_job();
        let Ok(jobs) = std::env::var(JOBS) else {
            return Ok((was_in_job, 0));
        };
        for limits in jobs.split(',') {
            enter(limits.contains('k'), limits.contains('b'))?;
        }
        let test = std::env::args().nth(2).ok_or("no test name in the arguments")?;
        let canary = Command::new(std::env::current_exe()?)
            .args(["--exact", &test, "--test-threads=1"])
            .env_clear()
            .envs(scrubbed_environment())
            .env(MODE, "sleep")
            .env(DIRECTORY, env_path(DIRECTORY)?)
            .env(CAPTURE, env_path(CAPTURE)?.with_extension("canary.json"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok((was_in_job, canary.id()))
    }

    /// Racing clients in one launcher inside the jobs `JOBS` names: they start the server at once, and the
    /// report has every one's server or error and every server they started.
    pub(super) fn race(directory: &Path) -> TestResult<Value> {
        let (was_in_job, canary) = enter_and_start_canary()?;
        let racers = 4;
        let barrier = Arc::new(Barrier::new(racers));
        let threads: Vec<_> = (0..racers)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let endpoint = Endpoint::new(directory);
                thread::spawn(move || {
                    barrier.wait();
                    connect_or_start(&endpoint, &hello(Role::Control), &StartOptions::new(EXE))
                })
            })
            .collect();
        let (mut pids, mut errors, mut spawned, mut held) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for racer in threads {
            match racer.join().map_err(|_| "a racer panicked")? {
                Ok(connected) => {
                    pids.push(connected.welcome.pid);
                    spawned.extend(
                        connected
                            .spawned
                            .iter()
                            .map(|started| json!({ "pid": started.pid, "path": format!("{:?}", started.path) })),
                    );
                    held.push(connected);
                }
                Err(error) => errors.push(error.to_string()),
            }
        }
        let report = json!({
            "pids": pids,
            "errors": errors,
            "spawned": spawned,
            "was_in_job": was_in_job,
            "canary": canary,
        });
        drop(held);
        Ok(report)
    }

    /// Puts this process in a new job, nested in any it is in. The handle is left open on purpose: the job closes
    /// when this process ends.
    fn enter(kill_on_close: bool, breakaway: bool) -> TestResult {
        // SAFETY: no attributes and no name: an anonymous job with default security.
        let job = unsafe { CreateJobObjectW(null(), null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        if kill_on_close {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        if breakaway {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        }
        let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())?;
        // SAFETY: the job is open, and `limits` is valid for its size.
        let set = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast::<c_void>(),
                size,
            )
        };
        if set == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: GetCurrentProcess returns an always valid pseudo-handle.
        let current = unsafe { GetCurrentProcess() };
        // SAFETY: both handles are valid.
        if unsafe { AssignProcessToJobObject(job, current) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    /// A launcher in the jobs `jobs` starts a server and ends; the server outlives the jobs' close, which kills the
    /// canary beside it. Returns the launcher's report.
    fn launch_in(test: &str, jobs: &str) -> TestResult<Value> {
        let mut namespace = Namespace::new()?;
        let (mut command, capture) = namespace.child(test, "launcher", &[(JOBS, OsStr::new(jobs))])?;
        let mut launcher = command.spawn()?;
        let report = read_capture(&capture, &mut launcher)?;
        let status = launcher.wait()?;
        assert!(status.success(), "{status}: {}", child_output(&mut launcher));
        assert!(report["error"].is_null(), "{report}");
        let pid = report_pid(&report)?;
        namespace.servers.push(pid);
        let canary = u32::try_from(report["canary"].as_u64().ok_or("no canary")?)?;
        assert!(
            process::gone_by(canary, Instant::now() + EXIT_SLACK),
            "the launcher's job did not kill on close: {report}"
        );
        assert_eq!(report["detached"], true, "{report}");
        let found = namespace
            .find()?
            .ok_or("the server did not outlive its launcher's job")?;
        assert_eq!(found.welcome.pid, pid);
        Ok(report)
    }

    /// Breakaway, or WMI where a job this test runs in already (a runner's) refuses it.
    fn assert_breakaway_unless_refused(report: &Value) {
        let path = report["path"].as_str().unwrap_or_default();
        if report["was_in_job"] == true {
            assert!(["Breakaway", "Wmi"].contains(&path), "{report}");
        } else {
            assert_eq!(path, "Breakaway", "{report}");
        }
    }

    #[test]
    fn a_server_started_inside_a_kill_on_close_job_survives_its_close() {
        if child_mode().unwrap() {
            return;
        }
        let report = launch_in(
            "jobs::a_server_started_inside_a_kill_on_close_job_survives_its_close",
            "kb",
        )
        .unwrap();
        assert_breakaway_unless_refused(&report);
    }

    #[test]
    fn a_job_that_refuses_breakaway_still_gets_a_server_through_wmi() {
        if child_mode().unwrap() {
            return;
        }
        let report = launch_in(
            "jobs::a_job_that_refuses_breakaway_still_gets_a_server_through_wmi",
            "k",
        )
        .unwrap();
        assert_eq!(report["path"], "Wmi", "{report}");
    }

    #[test]
    fn nested_jobs_that_allow_breakaway_are_all_left() {
        if child_mode().unwrap() {
            return;
        }
        // the inner job does not kill on close, the outer one does: only the inner one's limits show from inside
        let report = launch_in("jobs::nested_jobs_that_allow_breakaway_are_all_left", "kb,b").unwrap();
        assert_breakaway_unless_refused(&report);
    }

    #[test]
    fn a_nested_job_that_refuses_breakaway_inside_a_killing_one_goes_through_wmi() {
        if child_mode().unwrap() {
            return;
        }
        let report = launch_in(
            "jobs::a_nested_job_that_refuses_breakaway_inside_a_killing_one_goes_through_wmi",
            "kb,",
        )
        .unwrap();
        assert_eq!(report["path"], "Wmi", "{report}");
    }

    /// Every server a racing start launched through WMI but the winner loses the namespace and exits within
    /// milliseconds, often before WMI's answer reaches its client: that client probes again and finds the winner.
    #[test]
    fn racing_starts_through_wmi_leave_one_server() {
        if child_mode().unwrap() {
            return;
        }
        let mut namespace = Namespace::new().unwrap();
        let (mut command, capture) = namespace
            .child(
                "jobs::racing_starts_through_wmi_leave_one_server",
                "racers",
                &[(JOBS, OsStr::new("k"))],
            )
            .unwrap();
        let mut launcher = command.spawn().unwrap();
        let report = read_capture(&capture, &mut launcher).unwrap();
        let status = launcher.wait().unwrap();
        assert!(status.success(), "{status}: {}", child_output(&mut launcher));
        let spawned = report["spawned"].as_array().unwrap();
        namespace.servers.extend(
            spawned
                .iter()
                .filter_map(|started| started["pid"].as_u64())
                .map(|pid| u32::try_from(pid).unwrap()),
        );
        assert_eq!(report["errors"], json!([]), "{report}");
        let pids = report["pids"].as_array().unwrap();
        assert_eq!(pids.len(), 4, "{report}");
        assert!(
            pids.iter().all(|pid| *pid == pids[0]),
            "every racer is connected to one server: {report}"
        );
        assert!(spawned.iter().all(|started| started["path"] == "Wmi"), "{report}");
        let winner = u32::try_from(pids[0].as_u64().unwrap()).unwrap();
        let found = namespace
            .find()
            .unwrap()
            .expect("the server outlived its launcher's job");
        assert_eq!(found.welcome.pid, winner);
    }
}
