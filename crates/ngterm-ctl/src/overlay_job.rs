//! `ngtermctl session overlay run-job <job>` (`agtermctlKit/OverlayRunJob.swift`): the helper that supervises one
//! remote overlay job on the origin. It claims the job over the app's socket (`session.overlay.job.run`), runs the
//! program under the pty it inherited from the viewer's ssh, reports how it ended, and stops it when cancelled.
//! Everything that decides the outcome is [`JobRunner`]; the command only wires signals.
//!
//! The frames after the claim's ordinary reply are [`OverlayJobFrame`] lines: the app sends `context`, then maybe
//! `cancel`; the helper reports `started`, then one of `exited`, `canceled` or `launch-failed`.

use std::ffi::OsString;
use std::io::{self, ErrorKind};
use std::net::Shutdown;
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, ExitCode};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ngterm_core::overlay_capture::OverlayLaunchContext;
use ngterm_core::overlay_jobs::OverlayJobFrame;
use ngterm_core::paths::Env;
use ngterm_proto::command::SessionCmd;
use ngterm_proto::{Command, MAX_REQUEST_LINE_BYTES, Request, Response};

use crate::client::{self, os_message};
use crate::{BasicOptions, Failure, RequestCommand};

/// How long a cancelled program gets to exit before it is killed.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(3);
/// Cap on a frame line (upstream's `PresentationCodec.maxFrameBytes`).
const MAX_FRAME_BYTES: usize = 256 * 1024;
/// The helper's status when the program could not be launched.
const LAUNCH_FAILED_STATUS: i32 = 127;
/// How often the supervisor looks at the program, the terminal and the cancel deadline.
const SUPERVISE_TICK: Duration = Duration::from_millis(20);
/// The shell that runs the program. `eval` keeps the command's own exit status as the shell's; the command rides
/// `NGTERM_OVL_CMD` in the context's environment, so nothing interpolates it.
const SHELL: &str = "/bin/sh";
const SHELL_SCRIPT: &str = r#"eval "$NGTERM_OVL_CMD""#;
/// The signals the helper changes, put back to their defaults in the program.
const RESET_SIGNALS: [libc::c_int; 7] = [
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGTSTP,
    libc::SIGTTOU,
    libc::SIGTERM,
    libc::SIGHUP,
    libc::SIGPIPE,
];
/// The signals that cancel the program as a `cancel` frame does: losing the terminal, or being told to stop.
const STOP_SIGNALS: [libc::c_int; 2] = [libc::SIGHUP, libc::SIGTERM];
/// The terminal's keys are the program's, and taking the terminal back from its group happens from the background.
const IGNORED_SIGNALS: [libc::c_int; 4] = [libc::SIGINT, libc::SIGQUIT, libc::SIGTSTP, libc::SIGTTOU];

/// The program's process group and the deadline a cancel set, shared with the threads that cancel.
#[derive(Debug, Default)]
struct Stop {
    /// The program's pid, which leads its group; 0 before it started.
    group: libc::pid_t,
    /// When a cancelled program is killed; `None` until cancelled.
    kill_at: Option<Instant>,
}

#[derive(Debug)]
struct Shared {
    grace: Duration,
    stop: Mutex<Stop>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Stop> {
        self.stop.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// SIGTERM to the program's group now, SIGKILL once the grace has passed; before the program started, it is
    /// stopped as soon as it does.
    fn cancel(&self) {
        let group = {
            let mut stop = self.lock();
            if stop.kill_at.is_none() {
                stop.kill_at = Some(Instant::now() + self.grace);
            }
            stop.group
        };
        if group > 0 {
            signal_group(group, libc::SIGTERM);
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.lock().kill_at
    }
}

/// Stops the job's program from any thread.
#[derive(Debug, Clone)]
pub struct Canceler(Arc<Shared>);

impl Canceler {
    pub fn cancel(&self) {
        self.0.cancel();
    }
}

/// Supervises one claimed job over its connection to the app (upstream's `OverlayJobRunner`).
#[derive(Debug)]
pub struct JobRunner {
    socket: Arc<UnixStream>,
    /// The pty the program runs under, the helper's own stdin in production.
    terminal: RawFd,
    shared: Arc<Shared>,
}

impl JobRunner {
    /// A runner over `socket`, the app's connection. It is close-on-exec (std opens every descriptor so), so the
    /// program never holds it: only the helper ending closes it, which is how the app learns an unreported job is over.
    #[must_use]
    pub fn new(socket: UnixStream, terminal: RawFd, grace: Duration) -> Self {
        Self {
            socket: Arc::new(socket),
            terminal,
            shared: Arc::new(Shared {
                grace,
                stop: Mutex::new(Stop::default()),
            }),
        }
    }

    #[must_use]
    pub fn canceler(&self) -> Canceler {
        Canceler(Arc::clone(&self.shared))
    }

    /// Sends the claim and returns the launch context, or the app's refusal, having launched nothing.
    pub fn claim(&self, job: &str) -> Result<OverlayLaunchContext, String> {
        let mut line = serde_json::to_vec(&claim_request(job))
            .map_err(|error| format!("could not encode the request: {error}"))?;
        line.push(b'\n');
        client::send_all(&self.socket, &line).map_err(|_| "could not send the request".to_owned())?;
        let reply = client::read_socket_line(&self.socket, MAX_REQUEST_LINE_BYTES)
            .ok_or_else(|| "the app closed the connection without answering".to_owned())?;
        let response: Response =
            serde_json::from_slice(&reply).map_err(|error| format!("could not decode the reply: {error}"))?;
        if !response.ok {
            return Err(response
                .error
                .unwrap_or_else(|| "the app refused the stream".to_owned()));
        }
        match client::read_socket_line(&self.socket, MAX_FRAME_BYTES).map(|line| OverlayJobFrame::parse(&line)) {
            Some(Ok(OverlayJobFrame::Context { context })) => Ok(context),
            _ => Err("the app sent no launch context".to_owned()),
        }
    }

    /// Runs `context` to the end and returns the helper's own exit status: the program's, 128 plus the signal that
    /// ended it, or 127 when it could not be launched. The program starts from `base`, the helper's own environment,
    /// with the context's over it, so it keeps HOME, PATH and TERM. A report the app never receives changes nothing.
    pub fn run(&self, context: &OverlayLaunchContext, base: &[(OsString, OsString)]) -> i32 {
        let tty = is_terminal(self.terminal).then_some(self.terminal);
        let spawned = spawn(context, base, tty).and_then(|child| {
            let pid = libc::pid_t::try_from(child.id()).map_err(|_| io::Error::from(ErrorKind::InvalidData))?;
            Ok((child, pid))
        });
        let (mut child, pid) = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                // the child takes the terminal before its exec, which can still fail
                if let Some(tty) = tty {
                    // SAFETY: `getpgrp` only reads the calling process's group.
                    give_terminal(tty, unsafe { libc::getpgrp() });
                }
                self.report(&OverlayJobFrame::LaunchFailed {
                    reason: format!("could not start the program: {}", os_message(&error)),
                });
                self.hang_up();
                return LAUNCH_FAILED_STATUS;
            }
        };
        let canceled_early = {
            let mut stop = self.shared.lock();
            stop.group = pid;
            stop.kill_at.is_some()
        };
        self.report(&OverlayJobFrame::Started);
        if canceled_early {
            signal_group(pid, libc::SIGTERM);
        }
        let listener = self.listen();
        let status = self.supervise(&mut child, pid, tty);
        if let Some(tty) = tty {
            // SAFETY: `getpgrp` only reads the calling process's group.
            let helper = unsafe { libc::getpgrp() };
            give_terminal(tty, helper);
        }
        if self.shared.deadline().is_some() {
            self.report(&OverlayJobFrame::Canceled);
        } else {
            self.report(&OverlayJobFrame::Exited { code: status });
        }
        self.hang_up();
        if let Some(listener) = listener {
            // the listener only reads, and the hang-up just ended its read
            let _ = listener.join();
        }
        status
    }

    /// Ends the connection: the run reported all it will, and the listener's read returns.
    fn hang_up(&self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }

    /// Waits for the program, watching the terminal for a hangup, which cancels as a cancel frame does. After a cancel
    /// it also waits for the rest of the program's group, killing what outlives the grace, so no descendant is left
    /// running behind a `canceled` report.
    fn supervise(&self, child: &mut Child, pid: libc::pid_t, tty: Option<RawFd>) -> i32 {
        let mut status = None;
        let mut killed = false;
        loop {
            if status.is_none() {
                status = reap(child);
            }
            // after the reap: a program whose read got the hangup's end-of-file has already exited, and that exit must
            // not pass for an ordinary one while its group lives on
            if let Some(tty) = tty
                && self.shared.deadline().is_none()
                && hung_up(tty)
            {
                self.shared.cancel();
            }
            let deadline = self.shared.deadline();
            if status.is_some() && (deadline.is_none() || !group_alive(pid)) {
                return status.unwrap_or_default();
            }
            if let Some(deadline) = deadline
                && !killed
                && Instant::now() >= deadline
            {
                signal_group(pid, libc::SIGKILL);
                killed = true;
            }
            thread::sleep(SUPERVISE_TICK);
        }
    }

    /// Reads the app's frames until the connection ends, cancelling on a `cancel`. The app going away does not stop
    /// the program: it runs on under the viewer's pty, only unreported. `None` when no thread could be started.
    fn listen(&self) -> Option<JoinHandle<()>> {
        let socket = Arc::clone(&self.socket);
        let shared = Arc::clone(&self.shared);
        thread::Builder::new()
            .name("ngtermctl.run-job.listen".to_owned())
            .spawn(move || {
                while let Some(line) = client::read_socket_line(&socket, MAX_FRAME_BYTES) {
                    if matches!(OverlayJobFrame::parse(&line), Ok(OverlayJobFrame::Cancel)) {
                        shared.cancel();
                    }
                }
            })
            .ok()
    }

    /// Best effort: a report the app never receives changes nothing here.
    fn report(&self, frame: &OverlayJobFrame) {
        if let Ok(line) = frame.line() {
            let _ = client::send_all(&self.socket, &line);
        }
    }
}

/// The claim: the job id rides as the target.
fn claim_request(job: &str) -> Request {
    Request {
        cmd: Command::Session(SessionCmd::OverlayJobRun),
        target: Some(job.to_owned()),
        args: None,
    }
}

fn is_terminal(fd: RawFd) -> bool {
    // SAFETY: `isatty` only inspects the descriptor; an invalid one is an error, not undefined behavior.
    (unsafe { libc::isatty(fd) }) == 1
}

/// Sends `signal` to the whole process group `group` leads.
fn signal_group(group: libc::pid_t, signal: libc::c_int) {
    // SAFETY: `kill` takes any pid and signal; a group that is gone is an error, ignored here.
    unsafe { libc::kill(-group, signal) };
}

/// Whether any process of the group `group` leads still exists.
fn group_alive(group: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the group exists.
    (unsafe { libc::kill(-group, 0) }) == 0
}

/// Makes `group` the terminal's foreground with SIGTTOU held, which a background caller would otherwise get. Only
/// async-signal-safe calls, so the child may make it between fork and exec. Returns `tcsetpgrp`'s result.
fn give_terminal(tty: RawFd, group: libc::pid_t) -> libc::c_int {
    // SAFETY: an all-zero `sigset_t` is a valid (empty) set on Linux, which `sigemptyset` then initializes.
    let mut held: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `held` is a valid, writable set.
    unsafe { libc::sigemptyset(&raw mut held) };
    // SAFETY: as above, and SIGTTOU is a valid signal.
    unsafe { libc::sigaddset(&raw mut held, libc::SIGTTOU) };
    // SAFETY: `previous` is written by `pthread_sigmask` before it is read.
    let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: changes only this thread's mask; both sets are valid.
    unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const held, &raw mut previous) };
    // SAFETY: `tcsetpgrp` takes any descriptor and group; a refusal is its result.
    let result = unsafe { libc::tcsetpgrp(tty, group) };
    // SAFETY: restores the mask read above.
    unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &raw const previous, std::ptr::null_mut()) };
    result
}

/// In the child between fork and exec: take the terminal before the program runs a single instruction, or its first
/// read would stop it, then put back the signal dispositions the helper changed and unblock every signal: the child
/// inherits the forking thread's mask, which holds the stop signals for the helper's waiter.
fn prepare_child(tty: Option<RawFd>) -> io::Result<()> {
    // SAFETY: `getpid` only reads the calling process's id.
    let program = unsafe { libc::getpid() };
    if let Some(tty) = tty
        && give_terminal(tty, program) != 0
    {
        return Err(io::Error::last_os_error());
    }
    for signal in RESET_SIGNALS {
        // SAFETY: installs the default action, no handler code, for a valid signal.
        unsafe { libc::signal(signal, libc::SIG_DFL) };
    }
    // SAFETY: an all-zero `sigset_t` is a valid (empty) set on Linux, which `sigemptyset` then initializes.
    let mut unblocked: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `unblocked` is a valid, writable set.
    unsafe { libc::sigemptyset(&raw mut unblocked) };
    // SAFETY: replaces only this thread's mask with a valid set.
    let result = unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &raw const unblocked, std::ptr::null_mut()) };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    Ok(())
}

/// Starts the program as the leader of a process group of its own, which a cancel ends as a whole and which becomes
/// the terminal's foreground, so it gets the keys and SIGWINCH directly.
fn spawn(context: &OverlayLaunchContext, base: &[(OsString, OsString)], tty: Option<RawFd>) -> io::Result<Child> {
    let mut command = std::process::Command::new(SHELL);
    command
        .arg("-c")
        .arg(SHELL_SCRIPT)
        .env_clear()
        .envs(base.iter().map(|(name, value)| (name, value)))
        .envs(&context.environment)
        .current_dir(&context.cwd)
        .process_group(0);
    // SAFETY: the hook runs in the forked child after std put it in its own group (std leaves it the forking thread's
    // signal mask, which the hook clears), and calls only async-signal-safe functions, allocating nothing.
    unsafe { command.pre_exec(move || prepare_child(tty)) };
    command.spawn()
}

/// The program's status once it ended: its exit code, or 128 plus the signal that ended it. `None` while it runs.
fn reap(child: &mut Child) -> Option<i32> {
    let status = child.try_wait().ok()??;
    status.code().or_else(|| status.signal().map(|signal| 128 + signal))
}

/// Whether the terminal hung up, asked without reading, so the program's input is never taken.
fn hung_up(tty: RawFd) -> bool {
    let mut probe = libc::pollfd {
        fd: tty,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `probe` is one valid `pollfd`, and a zero timeout never blocks.
    if (unsafe { libc::poll(&raw mut probe, 1, 0) }) <= 0 {
        return false;
    }
    probe.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// Ignores the terminal's keys and SIGTTOU, and turns SIGHUP and SIGTERM into a cancel: like a shell running a
/// foreground command. The stop signals are blocked in the calling thread before any other starts, so every later
/// thread inherits the block and only the waiting thread takes them.
fn route_signals(canceler: Canceler) {
    for signal in IGNORED_SIGNALS {
        // SAFETY: ignoring a valid signal installs no handler code.
        unsafe { libc::signal(signal, libc::SIG_IGN) };
    }
    // SAFETY: an all-zero `sigset_t` is a valid (empty) set on Linux, which `sigemptyset` then initializes.
    let mut stops: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `stops` is a valid, writable set.
    unsafe { libc::sigemptyset(&raw mut stops) };
    for signal in STOP_SIGNALS {
        // SAFETY: as above, for a valid signal.
        unsafe { libc::sigaddset(&raw mut stops, signal) };
    }
    // SAFETY: changes only this thread's mask, inherited by the threads it starts next.
    unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const stops, std::ptr::null_mut()) };
    let waiter = thread::Builder::new()
        .name("ngtermctl.run-job.signals".to_owned())
        .spawn(move || {
            loop {
                let mut received: libc::c_int = 0;
                // SAFETY: `stops` is a valid set of signals this thread blocks, and `received` is writable.
                if unsafe { libc::sigwait(&raw const stops, &raw mut received) } == 0 {
                    canceler.cancel();
                }
            }
        });
    // without the waiter the stop signals stay blocked: the program still ends with the terminal's hangup
    drop(waiter);
}

/// `ngtermctl session overlay run-job <job>`.
#[derive(Debug, clap::Args)]
#[command(
    name = "run-job",
    about = "Run a remote overlay job this app handed to another Mac.",
    long_about = "Run on this Mac by the ngterm on another Mac, over ssh, when it shows one of this app's overlays: it \
                  claims the job, runs its program under that ssh terminal, and reports how it ended. It is not meant \
                  to be run by hand. A job that cannot be claimed exits 1 having launched nothing, one whose program \
                  cannot be launched exits 127, and otherwise it exits with the program's status."
)]
pub struct RunJob {
    #[arg(help = "The job id the other Mac was handed.")]
    job: String,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for RunJob {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    /// The claim the run sends first.
    fn make_request(&self) -> Request {
        claim_request(&self.job)
    }

    fn invoke(&self, env: &dyn Env) -> Result<(), Failure> {
        let socket = client::connect(&self.options.socket_path(env))?;
        let runner = JobRunner::new(socket, libc::STDIN_FILENO, DEFAULT_GRACE);
        let context = match runner.claim(&self.job) {
            Ok(context) => context,
            Err(message) => {
                eprintln!("error: {message}");
                return Err(Failure::Exit(ExitCode::FAILURE));
            }
        };
        route_signals(runner.canceler());
        let base: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        match u8::try_from(runner.run(&context, &base)) {
            Ok(0) => Ok(()),
            Ok(status) => Err(Failure::Exit(ExitCode::from(status))),
            Err(_) => Err(Failure::Exit(ExitCode::FAILURE)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::path::Path;

    use ngterm_core::pane_env::PaneEnv;

    use super::*;

    const OK_REPLY: &str = r#"{"ok":true,"result":{"id":"job"}}"#;

    /// The app's end of a helper connection: it answers the claim with `reply`, sends `context`, and records every
    /// frame the helper reports until the connection ends.
    struct FakeOrigin {
        /// Where a test sends frames from; `None` once the origin hung up.
        writer: Option<UnixStream>,
        served: JoinHandle<(Vec<u8>, Vec<OverlayJobFrame>)>,
    }

    impl FakeOrigin {
        /// A connected pair: the helper's end, and the origin serving the other. With `hang_up`, the origin goes away
        /// right after the context.
        fn serve(reply: &str, context: Option<OverlayLaunchContext>, hang_up: bool) -> (UnixStream, Self) {
            let (helper, origin) = UnixStream::pair().unwrap();
            let writer = (!hang_up).then(|| origin.try_clone().unwrap());
            let reply = format!("{reply}\n");
            let served = thread::spawn(move || {
                let claim = client::read_socket_line(&origin, MAX_REQUEST_LINE_BYTES).unwrap_or_default();
                client::send_all(&origin, reply.as_bytes()).unwrap();
                if let Some(context) = context {
                    let line = OverlayJobFrame::Context { context }.line().unwrap();
                    client::send_all(&origin, &line).unwrap();
                }
                if hang_up {
                    return (claim, Vec::new());
                }
                let mut frames = Vec::new();
                while let Some(line) = client::read_socket_line(&origin, MAX_FRAME_BYTES) {
                    frames.push(OverlayJobFrame::parse(&line).unwrap());
                }
                (claim, frames)
            });
            (helper, Self { writer, served })
        }

        fn send(&self, frame: &OverlayJobFrame) {
            if let Some(writer) = &self.writer {
                client::send_all(writer, &frame.line().unwrap()).unwrap();
            }
        }

        /// The claim line the origin read and the frames it got, once the helper's end is closed.
        fn finish(self) -> (Vec<u8>, Vec<OverlayJobFrame>) {
            drop(self.writer);
            self.served.join().unwrap()
        }

        fn frames(self) -> Vec<OverlayJobFrame> {
            self.finish().1
        }
    }

    fn runner(helper: UnixStream, grace: Duration) -> JobRunner {
        JobRunner::new(helper, -1, grace)
    }

    fn context(command: &str) -> OverlayLaunchContext {
        let environment = PaneEnv::from([("NGTERM_ENABLED".to_owned(), "1".to_owned())]);
        OverlayLaunchContext::new(command, "/tmp", environment)
    }

    fn base(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    fn plain_base() -> Vec<(OsString, OsString)> {
        base(&[("PATH", "/usr/bin:/bin"), ("HOME", "/tmp")])
    }

    /// Runs `command` to the end against a fresh origin: the helper's status and the frames the origin got.
    fn run_to_end(command: &str, base: &[(OsString, OsString)]) -> (i32, Vec<OverlayJobFrame>) {
        let (helper, origin) = FakeOrigin::serve(OK_REPLY, Some(context(command)), false);
        let runner = runner(helper, DEFAULT_GRACE);
        let claimed = runner.claim("job").unwrap();
        let status = runner.run(&claimed, base);
        drop(runner);
        (status, origin.frames())
    }

    /// The pid written to `path`, waiting for it to appear.
    fn read_pid(path: &Path) -> libc::pid_t {
        for _ in 0..250 {
            if let Ok(pid) = std::fs::read_to_string(path).unwrap_or_default().trim().parse() {
                return pid;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("no pid in {}", path.display())
    }

    /// Whether `pid` is gone, waiting up to two seconds.
    fn gone(pid: libc::pid_t) -> bool {
        for _ in 0..100 {
            // SAFETY: signal 0 only checks that the process exists.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_program_exiting_three_reports_three_and_the_helper_exits_three() {
        let (helper, origin) = FakeOrigin::serve(OK_REPLY, Some(context("exit 3")), false);
        let runner = runner(helper, DEFAULT_GRACE);
        let status = runner.run(&runner.claim("job-id").unwrap(), &plain_base());
        drop(runner);
        assert_eq!(status, 3);
        let (claim, frames) = origin.finish();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&claim).unwrap(),
            serde_json::json!({"cmd": "session.overlay.job.run", "target": "job-id"})
        );
        assert_eq!(frames, [OverlayJobFrame::Started, OverlayJobFrame::Exited { code: 3 }]);
    }

    #[test]
    fn a_refused_claim_launches_nothing() {
        let (helper, origin) = FakeOrigin::serve(r#"{"ok":false,"error":"job not claimable"}"#, None, false);
        let runner = runner(helper, DEFAULT_GRACE);
        assert_eq!(runner.claim("job"), Err("job not claimable".to_owned()));
        drop(runner);
        assert!(origin.frames().is_empty());
    }

    #[test]
    fn a_claim_answered_without_a_context_launches_nothing() {
        let (helper, origin) = FakeOrigin::serve(OK_REPLY, None, true);
        let runner = runner(helper, DEFAULT_GRACE);
        assert_eq!(runner.claim("job"), Err("the app sent no launch context".to_owned()));
        drop(runner);
        assert!(origin.frames().is_empty());
    }

    #[test]
    fn a_cancel_from_the_app_ends_the_program_canceled() {
        let (helper, origin) = FakeOrigin::serve(OK_REPLY, Some(context("sleep 30")), false);
        let runner = runner(helper, Duration::from_millis(300));
        let claimed = runner.claim("job").unwrap();
        let status = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(300));
                origin.send(&OverlayJobFrame::Cancel);
            });
            runner.run(&claimed, &plain_base())
        });
        drop(runner);
        assert_eq!(status, 128 + libc::SIGTERM);
        assert_eq!(origin.frames(), [OverlayJobFrame::Started, OverlayJobFrame::Canceled]);
    }

    #[test]
    fn a_cancel_kills_a_program_that_ignores_the_first_signal() {
        let (helper, origin) = FakeOrigin::serve(OK_REPLY, Some(context(r#"trap "" TERM; sleep 30"#)), false);
        let runner = runner(helper, Duration::from_millis(300));
        let claimed = runner.claim("job").unwrap();
        let canceler = runner.canceler();
        let started = Instant::now();
        let status = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(300));
                canceler.cancel();
            });
            runner.run(&claimed, &plain_base())
        });
        drop(runner);
        assert_eq!(status, 128 + libc::SIGKILL);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(origin.frames(), [OverlayJobFrame::Started, OverlayJobFrame::Canceled]);
    }

    /// Blocks SIGTERM in the calling thread, as `route_signals` does in run-job's, until dropped.
    struct SigtermBlocked(libc::sigset_t);

    impl SigtermBlocked {
        fn new() -> Self {
            // SAFETY: zeroed sets are initialized by `sigemptyset` or written by `pthread_sigmask` before use.
            let (mut term, mut previous): (libc::sigset_t, libc::sigset_t) = unsafe { std::mem::zeroed() };
            // SAFETY: `term` is a valid, writable set.
            unsafe { libc::sigemptyset(&raw mut term) };
            // SAFETY: as above, and SIGTERM is a valid signal.
            unsafe { libc::sigaddset(&raw mut term, libc::SIGTERM) };
            // SAFETY: changes only this thread's mask; both sets are valid.
            let result = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const term, &raw mut previous) };
            assert_eq!(result, 0);
            Self(previous)
        }
    }

    impl Drop for SigtermBlocked {
        fn drop(&mut self) {
            // SAFETY: restores the mask read in `new`.
            unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &raw const self.0, std::ptr::null_mut()) };
        }
    }

    #[test]
    fn the_program_gets_the_stop_signals_the_helpers_thread_blocks() {
        let _blocked = SigtermBlocked::new();
        let dir = tempfile::tempdir().unwrap();

        let mask_file = dir.path().join("mask");
        // the shell reads its own mask by becoming the reader: a forked `grep` reading `/proc/$$/status` races dash,
        // which blocks every signal around its (v)fork until the child execs (seen on ubuntu-26.04 as a full mask)
        let probe = format!("exec grep '^SigBlk:' /proc/self/status > {}", mask_file.display());
        let (status, _) = run_to_end(&probe, &plain_base());
        assert_eq!(status, 0);
        let line = std::fs::read_to_string(&mask_file).unwrap();
        let mask = u64::from_str_radix(line.trim_start_matches("SigBlk:").trim(), 16).unwrap();
        assert_eq!(
            mask & (1 << (libc::SIGTERM - 1)),
            0,
            "the shell started with SIGTERM blocked: {line}"
        );

        let ready = dir.path().join("ready");
        let command = format!("trap 'exit 0' TERM; : > {}; while :; do :; done", ready.display());
        let grace = Duration::from_secs(2);
        let (helper, origin) = FakeOrigin::serve(OK_REPLY, Some(context(&command)), false);
        let runner = runner(helper, grace);
        let claimed = runner.claim("job").unwrap();
        let canceler = runner.canceler();
        let (status, canceled_at) = thread::scope(|scope| {
            let canceling = scope.spawn(|| {
                while !ready.exists() {
                    thread::sleep(Duration::from_millis(20));
                }
                canceler.cancel();
                Instant::now()
            });
            let status = runner.run(&claimed, &plain_base());
            (status, canceling.join().unwrap())
        });
        drop(runner);
        assert_eq!(status, 0, "the trap never ran");
        assert!(canceled_at.elapsed() < grace);
        assert_eq!(origin.frames(), [OverlayJobFrame::Started, OverlayJobFrame::Canceled]);
    }

    #[test]
    fn a_report_the_app_never_gets_leaves_the_helpers_status_alone() {
        let (helper, origin) = FakeOrigin::serve(OK_REPLY, Some(context("sleep 0.3; exit 5")), true);
        let runner = runner(helper, DEFAULT_GRACE);
        let claimed = runner.claim("job").unwrap();
        assert!(origin.frames().is_empty());
        assert_eq!(runner.run(&claimed, &plain_base()), 5);
    }

    #[test]
    fn the_program_does_not_inherit_the_app_connection() {
        let (helper, origin) = FakeOrigin::serve(OK_REPLY, Some(context("exit 9")), false);
        let fd = helper.as_raw_fd();
        let runner = runner(helper, DEFAULT_GRACE);
        let mut claimed = runner.claim("job").unwrap();
        // the context is the app's to choose, so the test may aim it at the descriptor it now knows
        let probe = format!("test ! -e /dev/fd/{fd}");
        claimed.environment.insert("NGTERM_OVL_CMD".to_owned(), probe.clone());
        claimed.command = probe;
        let status = runner.run(&claimed, &plain_base());
        drop(runner);
        assert_eq!(status, 0, "the connection's descriptor reached the program");
        assert_eq!(
            origin.frames(),
            [OverlayJobFrame::Started, OverlayJobFrame::Exited { code: 0 }]
        );
    }

    #[test]
    fn the_terminal_type_comes_from_the_helpers_own_environment() {
        let base = base(&[("PATH", "/usr/bin:/bin"), ("HOME", "/tmp"), ("TERM", "xterm-kitty")]);
        let (status, frames) = run_to_end(r#"test "$TERM" = xterm-kitty"#, &base);
        assert_eq!(status, 0);
        assert_eq!(frames, [OverlayJobFrame::Started, OverlayJobFrame::Exited { code: 0 }]);
    }

    #[test]
    fn the_program_gets_the_helpers_environment_under_the_context() {
        let bin = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/bin/true", bin.path().join("ngterm-fixture-tool")).unwrap();
        let session = "0F2C7A4E-9B1D-4C3A-8E5F-1A2B3C4D5E6F";
        let environment = PaneEnv::from([("NGTERM_SESSION_ID".to_owned(), session.to_owned())]);
        let command = format!(r#"ngterm-fixture-tool && test -n "$HOME" && test "$NGTERM_SESSION_ID" = "{session}""#);
        let (helper, origin) = FakeOrigin::serve(
            OK_REPLY,
            Some(OverlayLaunchContext::new(&command, "/tmp", environment)),
            false,
        );
        let runner = runner(helper, DEFAULT_GRACE);
        let path = format!("{}:/usr/bin:/bin", bin.path().display());
        let base = base(&[("PATH", &path), ("HOME", "/home/x"), ("NGTERM_SESSION_ID", "stale")]);
        let status = runner.run(&runner.claim("job").unwrap(), &base);
        drop(runner);
        assert_eq!(status, 0);
        assert_eq!(
            origin.frames(),
            [OverlayJobFrame::Started, OverlayJobFrame::Exited { code: 0 }]
        );
    }

    #[test]
    fn a_cancel_ends_the_programs_descendants_too() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("descendant.pid");
        let command = format!("/bin/sleep 60 & echo $! > {}; wait", pid_file.display());
        let (helper, origin) = FakeOrigin::serve(OK_REPLY, Some(context(&command)), false);
        let runner = runner(helper, Duration::from_millis(300));
        let claimed = runner.claim("job").unwrap();
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(300));
                origin.send(&OverlayJobFrame::Cancel);
            });
            runner.run(&claimed, &plain_base())
        });
        drop(runner);
        assert!(gone(read_pid(&pid_file)), "the descendant outlived the cancel");
        assert_eq!(origin.frames(), [OverlayJobFrame::Started, OverlayJobFrame::Canceled]);
    }

    #[test]
    fn a_program_that_cannot_start_reports_launch_failed() {
        let context = OverlayLaunchContext::new("true", "/nonexistent-ngterm-run-job", PaneEnv::new());
        let (helper, origin) = FakeOrigin::serve(OK_REPLY, Some(context), false);
        let runner = runner(helper, DEFAULT_GRACE);
        let status = runner.run(&runner.claim("job").unwrap(), &plain_base());
        drop(runner);
        assert_eq!(status, 127);
        let frames = origin.frames();
        assert!(
            matches!(frames.as_slice(), [OverlayJobFrame::LaunchFailed { .. }]),
            "{frames:?}"
        );
    }
}
