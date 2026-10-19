//! The zmx binary, run once per request (upstream's `agterm/Ghostty/ZmxClient.swift`): a bounded process runner and the
//! [`SessionHost`] over it. Every invocation runs in the instance's namespace (`ZMX_DIR`) and never inherits a pane's
//! own session (`ZMX_SESSION`, `ZMX_SESSION_PREFIX`), so the app's reads and kills reach its own daemons only.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use agterm_core::live::lifecycle::parse_list;
use agterm_core::live::screen::parse_screen;
use agterm_core::live::support::is_executable_file;
use agterm_host::{HostError, KillOutcome, LiveScreen, LiveSessionRecord, SessionHost};

use crate::sweep::{Signal, Sweeper};

/// How long one invocation may run before it is terminated.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
/// The bound of a listing on the exit path: a 55-daemon cold refresh measured 10-30 ms upstream, so the timeout plus
/// the termination grace stays inside the exit budget.
pub const CAPTURE_INVOCATION_TIMEOUT: Duration = Duration::from_millis(100);
/// How long a terminated process gets before SIGKILL, and how long the output may trail the exit.
pub const TERMINATION_GRACE: Duration = Duration::from_millis(250);
/// How often a running invocation is polled for its exit.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// One zmx run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    /// Variables set (`Some`) or removed (`None`) on top of the app's own environment.
    pub environment: Vec<(String, Option<String>)>,
    pub timeout: Duration,
    /// Appends stderr to the output: a kill reports on stderr, a listing must not see its notices.
    pub merges_stderr: bool,
    /// Written to the child's stdin, which is then closed; stdin is null when `None`.
    pub input: Option<Vec<u8>>,
}

/// Why an invocation produced no output.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// It outran its timeout, or its output outran the exit by more than the grace.
    #[error("timed out")]
    TimedOut,
    /// It exited nonzero, with its stdout and stderr.
    #[error("exit status {0}: {1}")]
    Failed(i32, String),
    /// It could not be started.
    #[error("could not start: {0}")]
    Launch(#[from] std::io::Error),
}

/// Runs an invocation; replaced in tests.
pub type Runner = Box<dyn Fn(&Invocation) -> Result<String, CommandError> + Send + Sync>;

/// A client of the zmx daemons in one socket directory.
pub struct ZmxClient {
    executable: PathBuf,
    socket_directory: String,
    timeout: Duration,
    runner: Runner,
    /// Hangs up a killed shell's foreground job; `None` signals nothing.
    sweeper: Option<Sweeper>,
}

impl ZmxClient {
    /// A client running `executable` against the daemons in `socket_directory`.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>, socket_directory: impl Into<String>) -> Self {
        Self::with_runner(executable, socket_directory, DEFAULT_TIMEOUT, Box::new(run))
    }

    /// A client whose invocations go through `runner`.
    #[must_use]
    pub fn with_runner(
        executable: impl Into<PathBuf>,
        socket_directory: impl Into<String>,
        timeout: Duration,
        runner: Runner,
    ) -> Self {
        Self {
            executable: executable.into(),
            socket_directory: socket_directory.into(),
            timeout,
            runner,
            sweeper: None,
        }
    }

    /// This client with `sweeper` hanging up what a confirmed kill leaves running.
    #[must_use]
    pub fn with_sweeper(mut self, sweeper: Sweeper) -> Self {
        self.sweeper = Some(sweeper);
        self
    }

    /// The foreground job of each named daemon's shell, read before a kill so a confirmed one can hang them up.
    /// `shells` saves the listing when the caller holds the leader pids. Empty without a sweeper or when the listing
    /// fails.
    pub(crate) fn foreground_jobs(
        &self,
        names: &[&str],
        shells: Option<&[(String, i32)]>,
        timeout: Option<Duration>,
    ) -> Vec<(String, Vec<agterm_host::ProcessRecord>)> {
        let Some(sweeper) = &self.sweeper else {
            return Vec::new();
        };
        let listed;
        let known = if let Some(shells) = shells {
            shells
        } else {
            let Ok(records) = self.session_records(timeout) else {
                return Vec::new();
            };
            listed = records
                .into_iter()
                .filter_map(|record| record.leader_pid.map(|pid| (record.name, pid)))
                .collect::<Vec<_>>();
            &listed
        };
        let wanted: Vec<(String, i32)> = known
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .cloned()
            .collect();
        sweeper.capture(&wanted)
    }

    /// Hangs up the jobs whose daemon the kill output confirmed; a stale-socket cleanup leaves the shell, and its
    /// job, running.
    pub(crate) fn hang_up_confirmed(&self, jobs: &[(String, Vec<agterm_host::ProcessRecord>)], output: &str) {
        let Some(sweeper) = &self.sweeper else {
            return;
        };
        for (name, job) in jobs {
            if kill_outcome(output, name) == KillOutcome::Killed {
                sweeper.send(Signal::HangUp, job);
            }
        }
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The instance's namespace, `ZMX_DIR` of every invocation.
    #[must_use]
    pub fn socket_directory(&self) -> &str {
        &self.socket_directory
    }

    pub(crate) fn invoke(
        &self,
        arguments: &[&str],
        timeout: Option<Duration>,
        merges_stderr: bool,
        input: Option<&[u8]>,
    ) -> Result<String, CommandError> {
        (self.runner)(&Invocation {
            executable: self.executable.clone(),
            arguments: arguments.iter().map(|argument| (*argument).to_owned()).collect(),
            environment: vec![
                ("ZMX_DIR".to_owned(), Some(self.socket_directory.clone())),
                ("ZMX_SESSION".to_owned(), None),
                ("ZMX_SESSION_PREFIX".to_owned(), None),
            ],
            timeout: timeout.unwrap_or(self.timeout),
            merges_stderr,
            input: input.map(<[u8]>::to_vec),
        })
    }

    /// The parsed listing, unreadable rows included, so a caller can tell an absent daemon from one zmx could not
    /// read; `timeout` overrides this invocation's bound only.
    ///
    /// # Errors
    ///
    /// The listing failed or did not parse, which is not an empty namespace.
    pub fn session_records(&self, timeout: Option<Duration>) -> Result<Vec<LiveSessionRecord>, HostError> {
        let output = self.invoke(&["list"], timeout, false, None).map_err(|error| {
            tracing::error!(%error, "zmx list failed");
            HostError(format!("zmx list failed: {error}"))
        })?;
        parse_list(&output).map_err(|error| {
            tracing::error!(%error, "zmx list output was incomplete");
            HostError(format!("zmx list output was incomplete: {error}"))
        })
    }
}

/// What a kill's merged output says it did. Exit zero can mean an unreachable daemon: zmx prints `cleaned up stale
/// session` after merely unlinking a socket it could not reach, and reports an unresponsive daemon on stderr, so only
/// an exact confirmation line is a kill.
#[must_use]
pub fn kill_outcome(output: &str, name: &str) -> KillOutcome {
    let lines: Vec<&str> = output.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    if lines.contains(&format!("killed session {name}").as_str()) {
        KillOutcome::Killed
    } else if lines.contains(&format!("cleaned up stale session {name}").as_str()) {
        KillOutcome::StaleSocket
    } else {
        KillOutcome::Failed(
            lines
                .first()
                .map_or_else(|| "no output".to_owned(), |line| (*line).to_owned()),
        )
    }
}

impl SessionHost for ZmxClient {
    fn is_available(&self) -> bool {
        is_executable_file(&self.executable)
    }

    fn list(&self) -> Result<Vec<LiveSessionRecord>, HostError> {
        self.session_records(None)
    }

    /// One `zmx kill`, judged by its exact output line. Without `force` a stale socket is left alone, which is the
    /// outcome that cannot be told from success.
    fn kill(&self, name: &str, force: bool) -> KillOutcome {
        let arguments: &[&str] = if force {
            &["kill", name, "--force"]
        } else {
            &["kill", name]
        };
        let jobs = self.foreground_jobs(&[name], None, None);
        match self.invoke(arguments, None, true, None) {
            Ok(output) => {
                self.hang_up_confirmed(&jobs, &output);
                kill_outcome(&output, name)
            }
            Err(error) => {
                tracing::error!(name, %error, "zmx kill failed");
                KillOutcome::Failed(error.to_string())
            }
        }
    }

    fn kill_batch(&self, names: &[String]) -> bool {
        ZmxClient::kill_batch(self, names, None)
    }

    fn foreground_job(&self, shell: i32) -> Option<Vec<agterm_host::ProcessRecord>> {
        match &self.sweeper {
            Some(sweeper) => sweeper.foreground_job(shell),
            None => Some(Vec::new()),
        }
    }

    fn job_running(&self, job: &[agterm_host::ProcessRecord]) -> bool {
        self.sweeper.as_ref().is_some_and(|sweeper| sweeper.is_running(job))
    }

    fn end_job(&self, job: &[agterm_host::ProcessRecord]) {
        if let Some(sweeper) = &self.sweeper {
            sweeper.send(Signal::Kill, job);
        }
    }

    fn screen(&self, name: &str, all: bool) -> Result<LiveScreen, HostError> {
        let arguments: &[&str] = if all {
            &["screen", name, "--all"]
        } else {
            &["screen", name]
        };
        let output = self.invoke(arguments, None, false, None).map_err(|error| {
            tracing::error!(name, %error, "zmx screen failed");
            HostError(format!("zmx screen failed: {error}"))
        })?;
        parse_screen(&output).ok_or_else(|| HostError("zmx screen answered no screen".to_owned()))
    }

    /// `Ok` means the daemon queued the bytes, not that the program read them.
    fn type_input(&self, name: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.invoke(&["type", name], None, false, Some(bytes))
            .map(drop)
            .map_err(|error| {
                tracing::error!(name, %error, "zmx type failed");
                HostError(format!("zmx type failed: {error}"))
            })
    }
}

/// Runs `invocation` to completion within its timeout: SIGTERM past it, SIGKILL after [`TERMINATION_GRACE`]. Output is
/// drained while the child runs (zmx writes a listing row by row, so a few daemons fill a pipe before the exit), and a
/// write end held past the exit by another process fails the run rather than shortening its output.
///
/// # Errors
///
/// [`CommandError`]: the start failed, the run or its output timed out, or the exit was nonzero.
pub fn run(invocation: &Invocation) -> Result<String, CommandError> {
    let mut command = Command::new(&invocation.executable);
    command
        .args(&invocation.arguments)
        .stdin(if invocation.input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &invocation.environment {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    let mut child = command.spawn()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), invocation.input.clone()) {
        // a writer of its own, so a child that never reads cannot block the timeout; dropping it closes stdin
        thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let Some(status) = wait_until(&mut child, Instant::now() + invocation.timeout) else {
        terminate(&mut child);
        return Err(CommandError::TimedOut);
    };
    let deadline = Instant::now() + TERMINATION_GRACE;
    let stdout = collect(&stdout, deadline)?;
    let stderr = collect(&stderr, deadline)?;
    if !status.success() {
        return Err(CommandError::Failed(status.code().unwrap_or(-1), stdout + &stderr));
    }
    Ok(if invocation.merges_stderr {
        stdout + &stderr
    } else {
        stdout
    })
}

/// Reads `pipe` to its end on a thread of its own.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = sender.send(bytes);
    });
    receiver
}

fn collect(output: &mpsc::Receiver<Vec<u8>>, deadline: Instant) -> Result<String, CommandError> {
    let bytes = output
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| CommandError::TimedOut)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The child's exit status once it exits before `deadline`; `None` past it.
fn wait_until(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            _ => return None,
        }
    }
}

/// SIGTERM, then SIGKILL after the grace; the child is reaped either way.
fn terminate(child: &mut Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: `kill` has no memory preconditions; `pid` is our own unreaped child, so it cannot name another
        // process.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    if wait_until(child, Instant::now() + TERMINATION_GRACE).is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests;
