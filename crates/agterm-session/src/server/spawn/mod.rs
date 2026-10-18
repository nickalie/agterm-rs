//! Starting a namespace's server on demand: a client that finds no server starts one detached from itself and waits
//! for its endpoint; of clients that race to start one, every one ends up connected to the same server.
//!
//! herdr: `src/server/autodetect.rs` (`is_server_listening_at`, `spawn_server_daemon`, `build_server_daemon_command`,
//! `wait_for_server_socket`, `auto_detect_launch`) and `src/platform/mod.rs` (`detach_server_daemon_command`,
//! `current_process_is_detached_server_daemon`) at `HERDR_REV` (`build/pins.env`), licensed under the Apache License
//! 2.0, modified. Kept: probe the endpoint, spawn `<exe> server` with stdio on the null device and detached per OS,
//! then poll the endpoint until it answers within a deadline; a stale endpoint counts as no server; the second of two
//! racing servers loses the bind and exits. Changed: the probe is a full handshake on the connection the caller then
//! keeps, so a server that stops between the probe and the caller's first request cannot be hit (herdr probes, then
//! reconnects); the server's argv names its namespace directory (`<exe> server <directory>`), so a namespace's
//! server is told from any other by its command line; a server this call started that exits before it is ready is
//! started again only when it lost the namespace to another server ([`super::EXIT_TAKEN`]) or stopped by itself, and
//! reported otherwise; once ready, the server this call started must report itself detached
//! ([`super::CAPABILITY_DETACHED`]) or it is stopped and the start fails, since it would end with its launcher; the
//! child's environment drops the variables of the live session the launcher may run in (`AGTERM_*`, `ZMX_*`, the
//! isolation rule of `docs/plans/20261018-zig-free-core.md`); and the spawn itself is per OS (the `linux`, `macos` and
//! `windows` modules).

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(all(unix, not(target_os = "macos")))]
use linux as sys;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as sys;
#[cfg(target_os = "macos")]
pub use macos::responsible_pid;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::thread;
use std::time::{Duration, Instant};

use crate::endpoint::{Endpoint, Stream, is_connection_closed_error};
use crate::protocol::{Control, ControlError, FramingError, Hello, Welcome, read_frame, write_control};

use super::{CAPABILITY_DETACHED, EXIT_STOPPED, EXIT_TAKEN, HANDSHAKE_TIMEOUT};

/// How long a client waits for a server to become ready (herdr's `SERVER_READY_TIMEOUT`).
pub const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// How often the endpoint is probed while a server starts (herdr's `SOCKET_POLL_INTERVAL`).
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long one probe waits for a busy endpoint (every Windows pipe instance taken).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// The prefixes of the variables a server's environment drops: the attach contract's and the session variables of
/// whatever live session the launcher runs in, which mean nothing to the server and must not reach its sessions.
const STRIPPED_PREFIXES: [&str; 2] = ["AGTERM_", "ZMX_"];

/// How to start a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartOptions {
    /// The `agterm-session` executable.
    pub executable: PathBuf,
    /// How long to wait for a server to become ready.
    pub timeout: Duration,
}

impl StartOptions {
    /// Start `executable`, waiting up to [`READY_TIMEOUT`].
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            timeout: READY_TIMEOUT,
        }
    }
}

/// How a server was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnPath {
    /// A plain detached child (`setsid` on Unix; on Windows, outside any job).
    Detached,
    /// On Windows, inside a job: a child breaking away from it.
    Breakaway,
    /// On Windows, inside a job that refuses breakaway: created by WMI's `Win32_Process.Create`, outside the job.
    Wmi,
}

impl fmt::Display for SpawnPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Detached => "detached",
            Self::Breakaway => "breakaway from the launcher's job",
            Self::Wmi => "WMI Win32_Process.Create",
        })
    }
}

/// A server a call started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Started {
    pub pid: u32,
    pub path: SpawnPath,
}

/// A connection to a namespace's server, past its handshake.
#[derive(Debug)]
pub struct Connected {
    /// The connection, in the role its hello asked for.
    pub stream: Stream,
    pub welcome: Welcome,
    /// Every server this call started, in order: the one connected to among them when [`Connected::started_here`],
    /// the others lost the namespace to it or to a server another client started.
    pub spawned: Vec<Started>,
}

impl Connected {
    /// The server connected to, when this call started it.
    #[must_use]
    pub fn started_here(&self) -> Option<Started> {
        self.spawned
            .iter()
            .copied()
            .find(|started| started.pid == self.welcome.pid)
    }
}

/// Why no connection to a server could be made.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// Connecting failed for a reason other than no server listening (another user's server, an elevated one).
    #[error("could not connect to the agterm-session server at {endpoint}: {source}")]
    Connect { endpoint: String, source: io::Error },
    /// The server answered something other than a welcome, or the connection broke in a way no stopping server causes.
    #[error("the agterm-session server at {endpoint} broke the handshake: {source}")]
    Handshake { endpoint: String, source: FramingError },
    /// The server refused the hello (another protocol for a role that needs it equal, a role it does not serve).
    #[error("the agterm-session server at {endpoint} (build {}, pid {}) refused the connection: {error}", .welcome.build, .welcome.pid)]
    Refused {
        endpoint: String,
        error: ControlError,
        welcome: Box<Welcome>,
    },
    /// The executable could not be started.
    #[error("could not start the agterm-session server {executable}: {source}")]
    Spawn { executable: String, source: io::Error },
    /// The server this call started exited before it was ready, and not because another server holds the namespace.
    #[error(
        "the agterm-session server started for {endpoint} exited before it was ready ({status}); \
         run `agterm-session server <directory>` to see why"
    )]
    Exited { endpoint: String, status: String },
    /// The server this call started does not run detached from it, so it would end with its launcher; it was stopped.
    #[error(
        "the agterm-session server started for {endpoint} (pid {pid}, {path}) is not detached from its launcher and \
         would end with it; it was stopped"
    )]
    NotDetached {
        endpoint: String,
        pid: u32,
        path: SpawnPath,
    },
    /// No server became ready in time.
    #[error("no agterm-session server became ready at {endpoint} within {}s{last}", .timeout.as_secs_f32())]
    NotReady {
        endpoint: String,
        timeout: Duration,
        /// What the last probe found, as `: <reason>`, or empty.
        last: String,
    },
}

/// Connects to the server of `endpoint`'s namespace with `hello`, starting one when none answers, and waits up to
/// the options' timeout for it.
///
/// # Errors
///
/// Every [`StartError`]; a [`StartError::Refused`] carries the server's welcome.
pub fn connect_or_start(endpoint: &Endpoint, hello: &Hello, options: &StartOptions) -> Result<Connected, StartError> {
    let deadline = Instant::now() + options.timeout;
    let mut spawned: Vec<Started> = Vec::new();
    let mut child: Option<Spawned> = None;
    loop {
        let last = match probe(endpoint, hello) {
            Probe::Ready(stream, welcome) => {
                return ready(endpoint, stream, *welcome, child, spawned);
            }
            Probe::Absent(reason) => {
                if needs_start(endpoint, child.as_mut())? {
                    let started = spawn_server(&options.executable, endpoint.directory()).map_err(|source| {
                        StartError::Spawn {
                            executable: options.executable.display().to_string(),
                            source,
                        }
                    })?;
                    spawned.push(Started {
                        pid: started.pid(),
                        path: started.path(),
                    });
                    child = Some(started);
                }
                reason
            }
            Probe::Busy(reason) => reason,
            Probe::Failed(error) => return Err(*error),
        };
        if Instant::now() >= deadline {
            return Err(StartError::NotReady {
                endpoint: endpoint.to_string(),
                timeout: options.timeout,
                last: format!(": {last}"),
            });
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Connects to the server of `endpoint`'s namespace with `hello` if one runs, without starting one; `None` when none
/// answers.
///
/// # Errors
///
/// As [`connect_or_start`], but never a start's error; [`StartError::NotReady`] when the server stays busy (stopping,
/// or every pipe instance taken) past `timeout`.
pub fn connect_existing(
    endpoint: &Endpoint,
    hello: &Hello,
    timeout: Duration,
) -> Result<Option<Connected>, StartError> {
    let deadline = Instant::now() + timeout;
    loop {
        match probe(endpoint, hello) {
            Probe::Ready(stream, welcome) => {
                return Ok(Some(Connected {
                    stream,
                    welcome: *welcome,
                    spawned: Vec::new(),
                }));
            }
            Probe::Absent(_) => return Ok(None),
            Probe::Failed(error) => return Err(*error),
            Probe::Busy(last) if Instant::now() >= deadline => {
                return Err(StartError::NotReady {
                    endpoint: endpoint.to_string(),
                    timeout,
                    last: format!(": {last}"),
                });
            }
            Probe::Busy(_) => thread::sleep(POLL_INTERVAL),
        }
    }
}

/// The connection found: checks that a server this call started outlives it.
fn ready(
    endpoint: &Endpoint,
    stream: Stream,
    welcome: Welcome,
    child: Option<Spawned>,
    spawned: Vec<Started>,
) -> Result<Connected, StartError> {
    let connected = Connected {
        stream,
        welcome,
        spawned,
    };
    if let Some(mut child) = child.filter(|child| child.pid() == connected.welcome.pid)
        && !connected
            .welcome
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_DETACHED)
    {
        child.terminate();
        return Err(StartError::NotDetached {
            endpoint: endpoint.to_string(),
            pid: child.pid(),
            path: child.path(),
        });
    }
    Ok(connected)
}

/// Whether a server must be started: none was yet, or the last one exited after losing the namespace to another
/// server (or stopping by itself before anyone connected).
///
/// # Errors
///
/// The last one exited for any other reason.
fn needs_start(endpoint: &Endpoint, child: Option<&mut Spawned>) -> Result<bool, StartError> {
    let Some(child) = child else {
        return Ok(true);
    };
    match child.try_exit() {
        None => Ok(false),
        Some(Exit::Status(status)) if restartable(status) => Ok(true),
        Some(Exit::Unknown) => Ok(true),
        Some(Exit::Status(status)) => Err(StartError::Exited {
            endpoint: endpoint.to_string(),
            status: status.to_string(),
        }),
    }
}

/// An exit after which another start may succeed: the namespace was taken, or the server stopped unused.
fn restartable(status: ExitStatus) -> bool {
    status
        .code()
        .is_some_and(|code| code == i32::from(EXIT_TAKEN) || code == i32::from(EXIT_STOPPED))
}

/// What one probe of the endpoint found.
enum Probe {
    /// A server answered with a welcome without error.
    Ready(Stream, Box<Welcome>),
    /// No server listens: nothing at the endpoint, or a stale socket.
    Absent(String),
    /// A server is there but did not answer: it is stopping, or busy.
    Busy(String),
    Failed(Box<StartError>),
}

/// Connects and exchanges the hello.
fn probe(endpoint: &Endpoint, hello: &Hello) -> Probe {
    let mut stream = match endpoint.connect_timeout(CONNECT_TIMEOUT) {
        Ok(stream) => stream,
        Err(error) if is_absent(&error) => return Probe::Absent(error.to_string()),
        Err(error) if is_busy(&error) => return Probe::Busy(error.to_string()),
        Err(source) => {
            return Probe::Failed(Box::new(StartError::Connect {
                endpoint: endpoint.to_string(),
                source,
            }));
        }
    };
    match handshake(&mut stream, hello) {
        Ok(welcome) => match welcome.error.clone() {
            None => Probe::Ready(stream, Box::new(welcome)),
            Some(error) => Probe::Failed(Box::new(StartError::Refused {
                endpoint: endpoint.to_string(),
                error,
                welcome: Box::new(welcome),
            })),
        },
        Err(error) if is_stopping(&error) => Probe::Busy(format!("the server closed the connection: {error}")),
        Err(source) => Probe::Failed(Box::new(StartError::Handshake {
            endpoint: endpoint.to_string(),
            source,
        })),
    }
}

/// Sends `hello` and reads the welcome, under [`HANDSHAKE_TIMEOUT`] on Unix.
fn handshake(stream: &mut Stream, hello: &Hello) -> Result<Welcome, FramingError> {
    super::set_recv_timeout(stream, Some(HANDSHAKE_TIMEOUT));
    write_control(stream, &Control::Hello(hello.clone()))?;
    let frame = read_frame(stream)?;
    super::set_recv_timeout(stream, None);
    match frame.control()? {
        Control::Welcome(welcome) => Ok(welcome),
        other => Err(FramingError::Malformed(format!("expected welcome.v1, got {other:?}"))),
    }
}

fn is_absent(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
}

fn is_busy(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) || is_connection_closed_error(error)
}

/// A handshake that broke the way a server stopping under it breaks it: closed, reset, or silent.
fn is_stopping(error: &FramingError) -> bool {
    match error {
        FramingError::Closed | FramingError::UnexpectedEof => true,
        FramingError::Io(error) => is_busy(error),
        _ => false,
    }
}

/// How a started server ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Status(ExitStatus),
    /// It ended, but its status could not be read (its parent cannot wait for it, as with `SIGCHLD` ignored).
    Unknown,
}

/// A server process this client started. Dropping it neither stops nor waits for the server: on Unix a thread
/// reaps it whenever it exits, so it never lingers as a zombie of a long-running launcher.
#[derive(Debug)]
pub struct Spawned {
    pid: u32,
    path: SpawnPath,
    process: sys::Process,
}

impl Spawned {
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    #[must_use]
    pub fn path(&self) -> SpawnPath {
        self.path
    }

    /// How the server ended, `None` while it runs.
    pub fn try_exit(&mut self) -> Option<Exit> {
        self.process.try_exit()
    }

    /// Stops the server at once.
    pub fn terminate(&mut self) {
        self.process.terminate(self.pid);
    }
}

/// Starts `<executable> server <directory>` detached from this process, with stdio on the null device and the
/// environment of [`server_environment`]; no other descriptor or handle of this process reaches it.
///
/// # Errors
///
/// The executable could not be started (on Windows: also when breakaway from this process's job is refused and WMI
/// cannot start it either).
pub fn spawn_server(executable: &Path, directory: &Path) -> io::Result<Spawned> {
    let command = ServerCommand::new(executable, directory);
    let (pid, path, process) = sys::spawn(&command)?;
    Ok(Spawned { pid, path, process })
}

/// Whether this process runs detached from whoever started it, so that it outlives its launcher: on Unix it leads
/// its own session (and on macOS is responsible for itself); on Windows it has no console and no job that kills it
/// when the job closes (herdr's `current_process_is_detached_server_daemon`).
#[must_use]
pub fn current_process_is_detached() -> bool {
    sys::current_process_is_detached()
}

/// What starts a server.
#[derive(Debug, Clone)]
struct ServerCommand {
    executable: PathBuf,
    args: Vec<OsString>,
    /// The whole environment, in order.
    env: Vec<(OsString, OsString)>,
    /// `None`: this process's own.
    #[cfg_attr(
        not(windows),
        expect(dead_code, reason = "a server's working directory is set per OS only on Windows")
    )]
    cwd: Option<PathBuf>,
}

impl ServerCommand {
    fn new(executable: &Path, directory: &Path) -> Self {
        Self {
            executable: executable.to_path_buf(),
            args: vec![OsString::from("server"), directory.as_os_str().to_owned()],
            env: server_environment(std::env::vars_os()),
            cwd: None,
        }
    }
}

/// `inherited` without the variables of [`STRIPPED_PREFIXES`] (compared ignoring case on Windows, where names do).
fn server_environment(inherited: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    inherited
        .filter(|(name, _)| !is_stripped(name, cfg!(windows)))
        .collect()
}

fn is_stripped(name: &OsStr, ignore_case: bool) -> bool {
    let name = name.to_string_lossy();
    STRIPPED_PREFIXES.iter().any(|prefix| {
        name.get(..prefix.len()).is_some_and(|start| {
            if ignore_case {
                start.eq_ignore_ascii_case(prefix)
            } else {
                start == *prefix
            }
        })
    })
}

/// The Unix form of [`Spawned`]'s process: a thread waits for the child and hands its status over.
#[cfg(unix)]
mod reaper {
    use std::io;
    use std::process::ExitStatus;
    use std::sync::mpsc::{self, Receiver, TryRecvError};
    use std::thread;

    use super::Exit;

    #[derive(Debug)]
    pub(super) struct Process {
        exit: Receiver<io::Result<ExitStatus>>,
        ended: Option<Exit>,
    }

    impl Process {
        /// Reaps the child `wait` waits for on a thread of its own.
        pub(super) fn reap(wait: impl FnOnce() -> io::Result<ExitStatus> + Send + 'static) -> io::Result<Self> {
            let (send, exit) = mpsc::channel();
            thread::Builder::new()
                .name("agterm-session-reaper".to_owned())
                .spawn(move || {
                    let _ = send.send(wait());
                })?;
            Ok(Self { exit, ended: None })
        }

        pub(super) fn try_exit(&mut self) -> Option<Exit> {
            if self.ended.is_none() {
                self.ended = match self.exit.try_recv() {
                    Ok(Ok(status)) => Some(Exit::Status(status)),
                    Ok(Err(_)) | Err(TryRecvError::Disconnected) => Some(Exit::Unknown),
                    Err(TryRecvError::Empty) => None,
                };
            }
            self.ended
        }

        pub(super) fn terminate(&mut self, pid: u32) {
            // a child already reaped may have given its pid to another process
            if self.try_exit().is_some() {
                return;
            }
            if let Ok(pid) = i32::try_from(pid) {
                // SAFETY: kill takes no pointers; the pid is our child's, which the reaper has not reaped yet
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

/// What a breakaway start gave: the process, out of every job; or a process an enclosing job kept (it refuses
/// breakaway, while the jobs inside it allow it, so `CreateProcess` succeeds), which the caller has ended.
#[cfg(any(windows, test))]
#[derive(Debug)]
enum Breakaway<T> {
    Left(T),
    Kept,
}

/// Starting from inside a job on Windows (`windows.rs`), as a decision over the two ways to create the process, so
/// it is tested on every OS: breakaway first; where a job refuses it (`ERROR_ACCESS_DENIED` from the innermost one, or
/// the process still in a job, kept by an enclosing one), WMI; when that fails too, an error naming both. Never a
/// plain start inside the job, which would leave the server to die with it (`docs/spikes/20261018-session-server.md`).
#[cfg(any(windows, test))]
fn start_in_job<T>(
    breakaway: impl FnOnce() -> io::Result<Breakaway<T>>,
    wmi: impl FnOnce() -> io::Result<T>,
) -> io::Result<(T, SpawnPath)> {
    /// `ERROR_ACCESS_DENIED`: what `CreateProcess` gives for a breakaway the innermost job does not allow.
    const ERROR_ACCESS_DENIED: i32 = 5;
    let refused = match breakaway() {
        Ok(Breakaway::Left(process)) => return Ok((process, SpawnPath::Breakaway)),
        Ok(Breakaway::Kept) => "an enclosing job kept the process in it".to_owned(),
        Err(refused) if refused.raw_os_error() == Some(ERROR_ACCESS_DENIED) => refused.to_string(),
        Err(error) => return Err(error),
    };
    match wmi() {
        Ok(process) => Ok((process, SpawnPath::Wmi)),
        Err(failed) => Err(io::Error::new(
            failed.kind(),
            format!(
                "this process's job refuses breakaway ({refused}), and starting the server through WMI failed: {failed}"
            ),
        )),
    }
}

/// herdr's `quote_windows_command_line_arg` (`src/platform/mod.rs`): `value` quoted for `CommandLineToArgvW` (and the
/// C runtime), which reads it back unchanged. Defined on every OS so it is tested everywhere.
#[cfg(any(windows, test))]
fn quote_windows_arg(value: &str) -> String {
    if !value.is_empty() && !value.chars().any(|ch| matches!(ch, ' ' | '\t' | '\n' | '\x0b' | '"')) {
        return value.to_owned();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            backslashes += 1;
            continue;
        }
        let escaped = if ch == '"' { backslashes * 2 + 1 } else { backslashes };
        quoted.extend(std::iter::repeat_n('\\', escaped));
        backslashes = 0;
        quoted.push(ch);
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    #[test]
    fn the_server_environment_drops_the_live_session_variables() {
        let inherited = [
            ("PATH", "/usr/bin"),
            ("AGTERM_LIVE_DIR", "/tmp/agterm-live-x"),
            ("AGTERM_SESSION_ID", "1"),
            ("ZMX_SESSION", "s"),
            ("HOME", "/home/u"),
            ("XAGTERM_KEEP", "1"),
        ];
        let kept = server_environment(inherited.iter().map(|(name, value)| (os(name), os(value))));
        assert_eq!(
            kept,
            [
                (os("PATH"), os("/usr/bin")),
                (os("HOME"), os("/home/u")),
                (os("XAGTERM_KEEP"), os("1"))
            ]
        );
    }

    #[test]
    fn variable_names_are_matched_ignoring_case_where_the_os_does() {
        assert!(is_stripped(OsStr::new("agterm_live_dir"), true));
        assert!(is_stripped(OsStr::new("Zmx_Dir"), true));
        assert!(!is_stripped(OsStr::new("agterm_live_dir"), false));
        assert!(is_stripped(OsStr::new("AGTERM_LIVE_DIR"), false));
        assert!(!is_stripped(OsStr::new("AGTERM"), true));
        assert!(!is_stripped(OsStr::new(""), false));
    }

    #[test]
    fn a_server_is_started_again_only_after_losing_the_namespace_or_stopping_unused() {
        #[cfg(unix)]
        let status = |code: i32| <ExitStatus as std::os::unix::process::ExitStatusExt>::from_raw(code << 8);
        #[cfg(windows)]
        let status =
            |code: i32| <ExitStatus as std::os::windows::process::ExitStatusExt>::from_raw(code.cast_unsigned());
        assert!(restartable(status(i32::from(EXIT_TAKEN))));
        assert!(restartable(status(i32::from(EXIT_STOPPED))));
        assert!(!restartable(status(i32::from(super::super::EXIT_FAILED))));
        assert!(!restartable(status(i32::from(super::super::EXIT_USAGE))));
    }

    #[test]
    fn a_job_that_allows_breakaway_takes_it() {
        let (value, path) = start_in_job(
            || Ok(Breakaway::Left(1)),
            || -> io::Result<i32> { panic!("WMI is not tried") },
        )
        .unwrap();
        assert_eq!((value, path), (1, SpawnPath::Breakaway));
    }

    #[test]
    fn a_refused_breakaway_goes_through_wmi() {
        let (value, path) = start_in_job(|| Err(io::Error::from_raw_os_error(5)), || Ok(2)).unwrap();
        assert_eq!((value, path), (2, SpawnPath::Wmi));
    }

    #[test]
    fn a_process_an_enclosing_job_kept_goes_through_wmi() {
        let (value, path) = start_in_job(|| Ok(Breakaway::Kept), || Ok(3)).unwrap();
        assert_eq!((value, path), (3, SpawnPath::Wmi));
        let error = start_in_job::<()>(|| Ok(Breakaway::Kept), || Err(io::Error::other("no WMI"))).unwrap_err();
        assert_eq!(
            error.to_string(),
            "this process's job refuses breakaway (an enclosing job kept the process in it), and starting the server \
             through WMI failed: no WMI"
        );
    }

    #[test]
    fn a_wmi_failure_after_a_refused_breakaway_is_reported_with_both_reasons() {
        let error = start_in_job::<()>(
            || Err(io::Error::from_raw_os_error(5)),
            || Err(io::Error::other("WMI Win32_Process.Create returned error 9")),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.starts_with("this process's job refuses breakaway ("),
            "{message}"
        );
        assert!(
            message.ends_with("starting the server through WMI failed: WMI Win32_Process.Create returned error 9"),
            "{message}"
        );
    }

    #[test]
    fn another_create_failure_is_not_retried() {
        let error = start_in_job::<()>(
            || Err(io::Error::from(io::ErrorKind::NotFound)),
            || -> io::Result<()> { panic!("WMI is not tried") },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn windows_arguments_are_quoted_as_command_line_to_argv_reads_them() {
        let cases = [
            ("plain", "plain"),
            ("", "\"\""),
            ("two words", "\"two words\""),
            ("q\"uote", "\"q\\\"uote\""),
            (r"C:\dir\", r"C:\dir\"),
            (r"C:\with space\", r#""C:\with space\\""#),
            (r#"back\"slash"#, r#""back\\\"slash""#),
            ("tab\there", "\"tab\there\""),
        ];
        for (value, quoted) in cases {
            assert_eq!(quote_windows_arg(value), quoted, "{value:?}");
        }
    }

    #[test]
    fn probe_errors_are_classified() {
        assert!(is_absent(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(is_absent(&io::Error::from(io::ErrorKind::ConnectionRefused)));
        assert!(!is_absent(&io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(is_busy(&io::Error::from(io::ErrorKind::TimedOut)));
        assert!(is_busy(&io::Error::from(io::ErrorKind::ConnectionReset)));
        assert!(!is_busy(&io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(is_stopping(&FramingError::Closed));
        assert!(is_stopping(&FramingError::UnexpectedEof));
        assert!(is_stopping(&FramingError::Io(io::Error::from(
            io::ErrorKind::WouldBlock
        ))));
        assert!(!is_stopping(&FramingError::BadMagic(*b"HTTP")));
    }

    #[test]
    fn the_path_reads_as_a_phrase() {
        assert_eq!(SpawnPath::Detached.to_string(), "detached");
        assert_eq!(SpawnPath::Breakaway.to_string(), "breakaway from the launcher's job");
        assert_eq!(SpawnPath::Wmi.to_string(), "WMI Win32_Process.Create");
    }
}
