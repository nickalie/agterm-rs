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
//! isolation rule of CLAUDE.md > Never touch the user's running agterm); and the spawn itself is per OS (the `linux`,
//! `macos` and `windows` modules).
//!
//! # Builds
//!
//! A client expects the build staged beside the executable it would start ([`StartOptions::build`],
//! [`crate::build_id`]). A server of another build found, the client asks it once, on a control connection, to hand
//! its sessions over to that executable ([`crate::server::handoff`]), then connects again; [`Connected::handoff`]
//! says how that went, and a failed attempt is what the caller's reset offer stands on (`agterm-core`'s
//! `build_record`). A server whose protocol refuses the client's role is asked the same; when that fails the call
//! fails ([`StartError::HandoffFailed`]).
//!
//! # Private images on Windows
//!
//! A running executable cannot be replaced on Windows, so a server never runs from the staged binary there: each start
//! (and each handoff's replacement) runs a private copy in the namespace directory (`run\<binary's size, mtime and id>\
//! bin\agterm-session.exe`, with the staged id beside it when it is the binary's), reused while the staged binary is
//! unchanged, and swept once no server runs it. Staging a new build then never meets a file in use.

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
use crate::protocol::{
    CAPABILITY_HANDOFF_REQUEST, Control, ControlError, ErrorCode, FramingError, HandoffRequest, HandoffResult, Hello,
    PROTOCOL_VERSION, Role, Welcome, read_frame, write_control,
};

use super::{CAPABILITY_DETACHED, EXIT_STOPPED, EXIT_TAKEN, HANDSHAKE_TIMEOUT};

/// How long a client waits for a server to become ready (herdr's `SERVER_READY_TIMEOUT`).
pub const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// How often the endpoint is probed while a server starts (herdr's `SOCKET_POLL_INTERVAL`).
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long one probe waits for a busy endpoint (every Windows pipe instance taken).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// The least a probe waits to connect or for a welcome, however close its deadline: a zero wait would mean no
/// timeout at all to a Unix socket's receive timeout, and the pipe's default one to Windows' `WaitNamedPipe`.
const MIN_PROBE_WAIT: Duration = Duration::from_millis(10);

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
    /// The build a server must be, or be asked to hand its sessions over to `executable` ([`crate::build_id`]);
    /// `None` takes any server.
    pub build: Option<String>,
    /// How long a handoff's answer is waited for.
    pub handoff_timeout: Duration,
}

impl StartOptions {
    /// Start `executable`, waiting up to [`READY_TIMEOUT`], expecting the build staged beside it
    /// ([`crate::build_id::expected`]; none for an unstaged executable).
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        let executable = executable.into();
        Self {
            build: crate::build_id::expected(&executable),
            executable,
            timeout: READY_TIMEOUT,
            handoff_timeout: HANDOFF_TIMEOUT,
        }
    }
}

/// How long a client waits for a handoff's answer: a handoff runs several steps of up to
/// [`super::handoff::STEP_TIMEOUT`] each.
pub const HANDOFF_TIMEOUT: Duration = Duration::from_secs(150);

/// A handoff a call asked a server of another build for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffAttempt {
    /// The server of build `from` handed its sessions over: the connection is the replacement's.
    Committed { from: String },
    /// It did not (it takes no handoff requests, the handoff failed before its commit, or its outcome is unknown):
    /// the connection is the old server's, whose sessions predate the expected build.
    Failed { from: String, reason: String },
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
    /// The handoff this call asked for, having found a server of another build than [`StartOptions::build`].
    pub handoff: Option<HandoffAttempt>,
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
    /// The server refused the client's protocol and could not hand its sessions over to the client's build: its
    /// sessions stay with it, and predate the client's build.
    #[error(
        "the agterm-session server at {endpoint} (build {from}) speaks another protocol and could not hand its \
         sessions over to {build}: {reason}"
    )]
    HandoffFailed {
        endpoint: String,
        from: String,
        build: String,
        reason: String,
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
/// the options' timeout for it. Each probe's connect and handshake are cut to the time left before the deadline (but
/// never under 10 ms), so a server that accepts and stays silent ends the wait at the deadline; on Windows a handshake
/// cut short also waits for its pending read to be cancelled, at once in practice and for 1 s at most.
///
/// # Errors
///
/// Every [`StartError`]; a [`StartError::Refused`] carries the server's welcome.
pub fn connect_or_start(endpoint: &Endpoint, hello: &Hello, options: &StartOptions) -> Result<Connected, StartError> {
    let connected = start(endpoint, hello, options);
    let Some(build) = options.build.as_deref() else {
        return connected;
    };
    match connected {
        Ok(connected) if connected.welcome.build == build => Ok(connected),
        Ok(connected) => {
            let from = connected.welcome.build.clone();
            let takes = takes_handoff(&connected.welcome);
            let spawned = connected.spawned;
            // the old server disconnects its clients for the handoff anyway
            drop(connected.stream);
            let requested = if takes {
                request_handoff(endpoint, options, build)
            } else {
                Err(no_handoff())
            };
            let mut again = start(endpoint, hello, options)?;
            again.handoff = Some(attempt(from, build, &again.welcome, requested));
            again.spawned = spawned.into_iter().chain(again.spawned).collect();
            Ok(again)
        }
        Err(StartError::Refused {
            endpoint: at,
            error,
            welcome,
        }) if error.code == ErrorCode::ProtocolMismatch && welcome.build != build => {
            let requested = if takes_handoff(&welcome) {
                request_handoff(endpoint, options, build)
            } else {
                Err(no_handoff())
            };
            match requested {
                Ok(()) => {
                    let mut again = start(endpoint, hello, options)?;
                    again.handoff = Some(attempt(welcome.build.clone(), build, &again.welcome, Ok(())));
                    Ok(again)
                }
                Err(reason) => Err(StartError::HandoffFailed {
                    endpoint: at,
                    from: welcome.build,
                    build: build.to_owned(),
                    reason,
                }),
            }
        }
        other => other,
    }
}

/// Whether the server of `welcome` takes handoff requests.
fn takes_handoff(welcome: &Welcome) -> bool {
    welcome
        .capabilities
        .iter()
        .any(|capability| capability == CAPABILITY_HANDOFF_REQUEST)
}

fn no_handoff() -> String {
    format!("the server takes no handoff requests (no {CAPABILITY_HANDOFF_REQUEST})")
}

/// The attempt a handoff from the server of build `from` to `build` came to, by the server connected to after it
/// (`now`): the expected build serving means it committed, whatever the answer said (a connection lost before the
/// answer leaves it unknown, and the build read afterwards tells).
fn attempt(from: String, build: &str, now: &Welcome, requested: Result<(), String>) -> HandoffAttempt {
    if now.build == build {
        return HandoffAttempt::Committed { from };
    }
    let reason = match requested {
        Ok(()) => format!("the server still reports build {} after its handoff", now.build),
        Err(reason) => reason,
    };
    HandoffAttempt::Failed { from, reason }
}

/// Ask the server of `endpoint`, on a control connection, to hand its sessions over to `options.executable` of
/// `build`, and wait up to [`StartOptions::handoff_timeout`] for its answer.
///
/// # Errors
///
/// The handoff failed before its commit, or its answer did not come (the outcome is unknown), with the reason.
fn request_handoff(endpoint: &Endpoint, options: &StartOptions, build: &str) -> Result<(), String> {
    let hello = Hello {
        role: Role::Control,
        build: build.to_owned(),
        capabilities: Vec::new(),
    };
    let deadline = Instant::now() + options.timeout;
    let mut stream = loop {
        match probe(endpoint, &hello, deadline) {
            Probe::Ready(stream, _) => break stream,
            Probe::Busy(_) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Probe::Busy(reason) | Probe::Absent(reason) => return Err(format!("no server to ask: {reason}")),
            Probe::Failed(error) => return Err(error.to_string()),
        }
    };
    let request = HandoffRequest {
        import_exe: options.executable.to_string_lossy().into_owned(),
        expected_build: build.to_owned(),
        expected_protocol: PROTOCOL_VERSION,
    };
    write_control(&mut stream, &Control::HandoffRequest(request))
        .map_err(|err| format!("the handoff request could not be sent: {err}"))?;
    let answer = within(stream, options.handoff_timeout, |stream| read_frame(stream)?.control())
        .map_err(|err| format!("the handoff's outcome is unknown: {err}"))?
        .1;
    match answer {
        Control::HandoffResult(HandoffResult(result)) => result.map_err(|error| error.message),
        other => Err(format!("the server answered the handoff request with {other:?}")),
    }
}

/// The server connection for `hello`, started when none answers (the [`connect_or_start`] without its build check).
fn start(endpoint: &Endpoint, hello: &Hello, options: &StartOptions) -> Result<Connected, StartError> {
    let deadline = Instant::now() + options.timeout;
    let mut spawned: Vec<Started> = Vec::new();
    let mut child: Option<Spawned> = None;
    loop {
        let last = match probe(endpoint, hello, deadline) {
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
/// silent, or every pipe instance taken) past `timeout`, which bounds the wait as [`connect_or_start`]'s does.
pub fn connect_existing(
    endpoint: &Endpoint,
    hello: &Hello,
    timeout: Duration,
) -> Result<Option<Connected>, StartError> {
    let deadline = Instant::now() + timeout;
    loop {
        match probe(endpoint, hello, deadline) {
            Probe::Ready(stream, welcome) => {
                return Ok(Some(Connected {
                    stream,
                    welcome: *welcome,
                    spawned: Vec::new(),
                    handoff: None,
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
        handoff: None,
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

/// Connects and exchanges the hello, each step cut to the time left before `deadline` ([`probe_wait`]).
fn probe(endpoint: &Endpoint, hello: &Hello, deadline: Instant) -> Probe {
    let stream = match endpoint.connect_timeout(probe_wait(deadline, CONNECT_TIMEOUT)) {
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
    match handshake(stream, hello, probe_wait(deadline, HANDSHAKE_TIMEOUT)) {
        Ok((stream, welcome)) => match welcome.error.clone() {
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

/// How long a probe's step waits: the time left before `deadline`, at most `cap` and at least [`MIN_PROBE_WAIT`].
fn probe_wait(deadline: Instant, cap: Duration) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .clamp(MIN_PROBE_WAIT, cap)
}

/// Sends `hello` and reads the welcome, giving up on a peer silent for `timeout` (a hung server, or another process
/// holding the endpoint) with an I/O error a stopping server's also gives ([`is_stopping`]): on Unix by the socket's
/// receive timeout, on Windows, whose pipes take none, by cancelling the exchange (`windows::exchange_within`).
fn handshake(stream: Stream, hello: &Hello, timeout: Duration) -> Result<(Stream, Welcome), FramingError> {
    let hello = hello.clone();
    within(stream, timeout, move |stream| exchange_hello(stream, &hello))
}

/// Runs `exchange` on `stream`, giving up on a peer silent for `timeout` with an I/O error: on Unix by the socket's
/// receive timeout, on Windows, whose pipes take none, by cancelling the exchange (`windows::exchange_within`).
fn within<T: Send + 'static>(
    stream: Stream,
    timeout: Duration,
    exchange: impl FnOnce(&mut Stream) -> Result<T, FramingError> + Send + 'static,
) -> Result<(Stream, T), FramingError> {
    #[cfg(unix)]
    {
        let mut stream = stream;
        super::set_recv_timeout(&stream, Some(timeout));
        let value = exchange(&mut stream)?;
        super::set_recv_timeout(&stream, None);
        Ok((stream, value))
    }
    #[cfg(windows)]
    {
        sys::exchange_within(stream, timeout, exchange)
    }
}

/// Sends `hello` and reads the welcome.
fn exchange_hello(stream: &mut Stream, hello: &Hello) -> Result<Welcome, FramingError> {
    write_control(stream, &Control::Hello(hello.clone()))?;
    let frame = read_frame(stream)?;
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
    /// It ended, but its status could not be read: its parent cannot wait for it (as with `SIGCHLD` ignored), or on
    /// Windows a WMI start ended before its handle could be had.
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

    /// The server's process handle, when its start gave one: what an update handoff duplicates the sessions' handles
    /// into.
    #[cfg(windows)]
    #[must_use]
    pub fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
        self.process.handle()
    }
}

/// Starts `<executable> server <directory>` detached from this process, with stdio on the null device and the
/// environment of [`server_environment`]; no other descriptor or handle of this process reaches it. On Windows the
/// server runs a private copy of `executable` (module docs).
///
/// # Errors
///
/// The executable could not be started (on Windows: also when it could not be copied, or when breakaway from this
/// process's job is refused and WMI cannot start it either).
pub fn spawn_server(executable: &Path, directory: &Path) -> io::Result<Spawned> {
    #[cfg(windows)]
    let image = private_image(executable, directory)?;
    #[cfg(not(windows))]
    let image = executable.to_path_buf();
    let command = ServerCommand::new(&image, directory);
    let (pid, path, process) = sys::spawn(&command)?;
    Ok(Spawned { pid, path, process })
}

/// Starts an update handoff's replacement server from `executable` for the namespace `directory`, detached as
/// [`spawn_server`] starts a server: `<executable> server <directory> --handoff-import <channel> <token>`.
///
/// # Errors
///
/// As [`spawn_server`].
pub fn spawn_import(executable: &Path, directory: &Path, channel: &Path, token: &str) -> io::Result<Spawned> {
    #[cfg(windows)]
    let image = private_image(executable, directory)?;
    #[cfg(not(windows))]
    let image = executable.to_path_buf();
    let mut command = ServerCommand::new(&image, directory);
    command.args.extend([
        OsString::from(super::handoff::IMPORT_FLAG),
        channel.as_os_str().to_owned(),
        OsString::from(token),
    ]);
    let (pid, path, process) = sys::spawn(&command)?;
    Ok(Spawned { pid, path, process })
}

/// The directory of a namespace's private server images.
#[cfg(any(windows, test))]
const IMAGES: &str = "run";

/// A private copy of `executable` in the namespace `directory` (module docs): `run\<size>-<mtime>-<id hash>\bin\
/// <file>`, with the staged id beside it, written for the copy, when that id is the executable's own
/// ([`crate::build_id::expected`]).
/// An existing copy of the same binary is reused; copies no server runs any more are removed first. A copy is made
/// under a temporary name and renamed into place, so racing starts each end up with a whole one; one made while the
/// executable changed is thrown away and made again.
///
/// # Errors
///
/// The executable could not be read or copied (or kept changing).
#[cfg(any(windows, test))]
fn private_image(executable: &Path, directory: &Path) -> io::Result<PathBuf> {
    use crate::build_id::{Stat, expected};

    let images = directory.join(IMAGES);
    let file = executable
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the server executable names no file"))?;
    for _ in 0..3 {
        let id = expected(executable);
        let before = Stat::of(executable)?;
        let key = format!(
            "{}-{}-{}",
            before.size,
            before.modified,
            agterm_core::live::fnv1a64(id.as_deref().unwrap_or_default().as_bytes())
        );
        sweep_images(&images, &key);
        let root = images.join(&key);
        let image = root.join("bin").join(file);
        if image.is_file() {
            return Ok(image);
        }
        let building = images.join(format!(".{}", uuid::Uuid::new_v4().simple()));
        let copied = copy_image(executable, &building, file, id.as_deref());
        if copied.is_ok() && Stat::of(executable)? == before {
            match std::fs::rename(&building, &root) {
                Ok(()) => return Ok(image),
                // a racing start put its copy in place first
                Err(_) if image.is_file() => {
                    let _ = std::fs::remove_dir_all(&building);
                    return Ok(image);
                }
                Err(err) => {
                    let _ = std::fs::remove_dir_all(&building);
                    return Err(err);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&building);
        copied?;
    }
    Err(io::Error::other(format!(
        "{} kept changing while it was copied",
        executable.display()
    )))
}

/// `executable` copied into `root/bin/<file>`, with `id` staged beside the copy for the copy.
#[cfg(any(windows, test))]
fn copy_image(executable: &Path, root: &Path, file: &OsStr, id: Option<&str>) -> io::Result<()> {
    let image = root.join("bin").join(file);
    std::fs::create_dir_all(root.join("bin"))?;
    std::fs::copy(executable, &image)?;
    if let Some(id) = id {
        crate::build_id::Staged::write_beside(&image, id)?;
    }
    Ok(())
}

/// Remove the images in `images` no server runs (their executable can be deleted) but the one of `current` (the
/// binary being started, which a racing start may be about to run), and copies left unfinished over a minute ago.
#[cfg(any(windows, test))]
fn sweep_images(images: &Path, current: &str) {
    /// How old an unfinished copy is before it counts as abandoned rather than a racing start's.
    const ABANDONED: Duration = Duration::from_secs(60);
    let Ok(entries) = std::fs::read_dir(images) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        if name == current {
            continue;
        }
        let root = entry.path();
        if name.to_string_lossy().starts_with('.') {
            let age = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok());
            if age.is_some_and(|age| age > ABANDONED) {
                let _ = std::fs::remove_dir_all(&root);
            }
            continue;
        }
        let binaries = std::fs::read_dir(root.join("bin"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok);
        // a running image cannot be deleted on Windows: such an image stays, whole
        if binaries
            .into_iter()
            .all(|binary| std::fs::remove_file(binary.path()).is_ok())
        {
            let _ = std::fs::remove_dir_all(&root);
        }
    }
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
            ("AGTERM_LIVE_DIR", "/tmp/ngterm-live-x"),
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
    fn a_probe_waits_the_time_left_within_its_bounds() {
        let now = Instant::now();
        assert_eq!(probe_wait(now, CONNECT_TIMEOUT), MIN_PROBE_WAIT);
        let past = now.checked_sub(Duration::from_secs(1)).unwrap();
        assert_eq!(probe_wait(past, CONNECT_TIMEOUT), MIN_PROBE_WAIT);
        let far = now + Duration::from_secs(60);
        assert_eq!(probe_wait(far, HANDSHAKE_TIMEOUT), HANDSHAKE_TIMEOUT);
        let left = probe_wait(now + Duration::from_millis(500), HANDSHAKE_TIMEOUT);
        assert!(left > MIN_PROBE_WAIT && left <= Duration::from_millis(500), "{left:?}");
    }

    /// A staged root holding a binary of `contents`, with the id `id` staged for it when given.
    fn staged_root(contents: &[u8], id: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("bin").join("agterm-session");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, contents).unwrap();
        if let Some(id) = id {
            crate::build_id::Staged::write_beside(&executable, id).unwrap();
        }
        (root, executable)
    }

    #[test]
    fn a_private_image_is_a_copy_with_its_staged_id_and_is_reused_while_the_binary_stays() {
        let (_root, executable) = staged_root(b"build a", Some("a"));
        let namespace = tempfile::tempdir().unwrap();
        let image = private_image(&executable, namespace.path()).unwrap();
        assert!(image.starts_with(namespace.path().join(IMAGES)), "{}", image.display());
        assert_eq!(std::fs::read(&image).unwrap(), b"build a");
        // the copy carries the staged id, verified for the copy itself
        assert_eq!(crate::build_id::expected(&image), Some("a".to_owned()));
        assert_eq!(private_image(&executable, namespace.path()).unwrap(), image, "reused");
        // a new binary staged: a new image, and the old one (no server runs it) swept
        std::fs::write(&executable, b"build b, longer").unwrap();
        crate::build_id::Staged::write_beside(&executable, "b").unwrap();
        let next = private_image(&executable, namespace.path()).unwrap();
        assert_ne!(next, image);
        assert_eq!(crate::build_id::expected(&next), Some("b".to_owned()));
        assert!(!image.exists(), "the image no server runs was swept");
    }

    #[test]
    fn an_image_of_an_unverified_binary_carries_no_id() {
        let (_root, executable) = staged_root(b"build a", Some("a"));
        // the binary replaced, its id not
        std::fs::write(&executable, b"build b, longer").unwrap();
        let namespace = tempfile::tempdir().unwrap();
        let image = private_image(&executable, namespace.path()).unwrap();
        assert_eq!(crate::build_id::expected(&image), None);
        let (_unstaged, plain) = staged_root(b"plain", None);
        let image = private_image(&plain, namespace.path()).unwrap();
        assert_eq!(crate::build_id::expected(&image), None);
    }

    #[test]
    fn a_racing_starts_unfinished_copy_is_left_and_an_abandoned_one_swept() {
        let namespace = tempfile::tempdir().unwrap();
        let images = namespace.path().join(IMAGES);
        let fresh = images.join(".racing");
        std::fs::create_dir_all(&fresh).unwrap();
        sweep_images(&images, "current");
        assert!(fresh.exists(), "a racing start's copy stays");
        let current = images.join("current").join("bin");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(current.join("agterm-session"), b"x").unwrap();
        sweep_images(&images, "current");
        assert!(current.join("agterm-session").exists(), "the image being started stays");
    }

    #[test]
    fn a_handoff_attempt_is_judged_by_the_build_served_after_it() {
        let welcome = |build: &str| Welcome {
            build: build.to_owned(),
            capabilities: Vec::new(),
            pid: 1,
            error: None,
        };
        assert_eq!(
            attempt("old".to_owned(), "new", &welcome("new"), Err("lost".to_owned())),
            HandoffAttempt::Committed { from: "old".to_owned() },
            "the answer was lost, but the new build serves"
        );
        assert_eq!(
            attempt("old".to_owned(), "new", &welcome("old"), Err("refused".to_owned())),
            HandoffAttempt::Failed {
                from: "old".to_owned(),
                reason: "refused".to_owned()
            }
        );
        let HandoffAttempt::Failed { reason, .. } = attempt("old".to_owned(), "new", &welcome("other"), Ok(())) else {
            panic!("another build serves");
        };
        assert!(reason.contains("still reports build other"), "{reason}");
        assert!(takes_handoff(&Welcome {
            capabilities: vec![CAPABILITY_HANDOFF_REQUEST.to_owned()],
            ..welcome("x")
        }));
        assert!(!takes_handoff(&welcome("x")));
    }

    #[test]
    fn the_path_reads_as_a_phrase() {
        assert_eq!(SpawnPath::Detached.to_string(), "detached");
        assert_eq!(SpawnPath::Breakaway.to_string(), "breakaway from the launcher's job");
        assert_eq!(SpawnPath::Wmi.to_string(), "WMI Win32_Process.Create");
    }
}
