//! Entering the vendored package's overlay with the program's own signals and exit status kept.
//!
//! `bwrap` does not relay signals to the program it runs: a SIGTERM ends `bwrap`, and `--die-with-parent` then kills
//! the program outright (checked against bwrap 0.13), so an app whose SIGTERM runs an orderly quit (the GPUI frontend's
//! quit save) would lose it. [`enter`] therefore keeps the first process as a small supervisor: it starts the same
//! program, arguments, working directory and environment inside the overlay ([`supervise`]), learns the program's pid
//! from `bwrap --json-status-fd`, relays SIGTERM, SIGINT, SIGHUP and SIGQUIT to it (a signal that arrives before the pid
//! is known is delivered once it is; the pid means the program was spawned, not that it installed its handlers), and
//! exits with the program's status as `bwrap` reports it: its exit code, or 128 + n when signal n ended it (the shell's
//! convention, not a death by signal). `bwrap` runs in a process group of its own, so the signals a terminal sends its
//! foreground group (Ctrl-C) reach the supervisor alone, which relays them, rather than ending `bwrap` first.
//!
//! The program finds itself in the overlay by the overlay itself ([`super::helpers_reachable`]). The supervisor also
//! sets [`OVERLAID_ENV`] for it, which only refuses a second attempt: a program that carries it and still finds no
//! helpers exits with a diagnostic rather than start another supervisor. The program removes it from its environment
//! at once ([`enter`]), so nothing it spawns inherits it.

use std::ffi::OsString;
use std::io::{BufRead as _, BufReader};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};

use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;

use super::{Provider, helpers_reachable, overlay_args, provider};

/// Set by the supervisor for the program it starts in the overlay (module docs).
pub const OVERLAID_ENV: &str = "AGTERM_WPE_OVERLAID";

/// The signals the supervisor relays.
const RELAYED: [i32; 4] = [SIGTERM, SIGINT, SIGHUP, SIGQUIT];

/// What a program does after [`enter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// Run as usual: the WPE `WebKit` is the system's, or this process is inside the overlay.
    Proceed,
    /// The program ran under the supervisor (or could not): exit with this status.
    Exit(i32),
}

/// Runs this program inside the vendored package's overlay when it is not there yet, as the supervisor of a copy of
/// itself (module docs); [`Entry::Proceed`] when it already is, or needs none.
///
/// # Safety
///
/// Call it first thing in `main`, while the process has one thread: it changes the process's environment.
#[must_use]
pub unsafe fn enter() -> Entry {
    let marked = std::env::var_os(OVERLAID_ENV).is_some();
    if marked {
        // SAFETY: the caller guarantees the process has one thread.
        unsafe { std::env::remove_var(OVERLAID_ENV) };
    }
    let Provider::Vendored { libdir } = provider() else {
        return Entry::Proceed;
    };
    if helpers_reachable() {
        return Entry::Proceed;
    }
    if marked {
        eprintln!(
            "agterm: the vendored WPE WebKit's overlay of {} on /usr/lib did not take effect, so its web process \
             cannot start; giving up rather than starting another supervisor",
            libdir.display()
        );
        return Entry::Exit(1);
    }
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(err) => {
            eprintln!("agterm: cannot find this program to run it under the WPE WebKit overlay: {err}");
            return Entry::Exit(1);
        }
    };
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match supervise(&libdir, &program, &args) {
        Ok(status) => Entry::Exit(status),
        Err(err) => {
            eprintln!("agterm: cannot run under the WPE WebKit overlay (bwrap): {err}");
            Entry::Exit(1)
        }
    }
}

/// Runs `program` with `args` in the overlay of `libdir` on `/usr/lib`, relaying signals to it (module docs); its exit
/// status as `bwrap` reports it, or `bwrap`'s own when it failed before starting the program.
///
/// # Errors
///
/// `bwrap` cannot be started, or the signal handlers or the status pipe cannot be set up.
pub fn supervise(libdir: &Path, program: &Path, args: &[OsString]) -> std::io::Result<i32> {
    let (status_read, status_write) = status_pipe()?;
    let cwd = std::env::current_dir().ok();
    let options = [
        OsString::from("--json-status-fd"),
        OsString::from(status_write.as_raw_fd().to_string()),
    ];
    let mut command = Command::new("bwrap");
    command
        .args(overlay_args(libdir, cwd.as_deref(), &options, program))
        .args(args)
        .env(OVERLAID_ENV, "1")
        .process_group(0);
    // before the spawn: a signal from here on is relayed, not lost
    let mut signals = Signals::new(RELAYED)?;
    let handle = signals.handle();
    let relay = Arc::new(Mutex::new(Relay::default()));
    let mut child = command.spawn()?;
    drop(status_write);
    let relaying = Arc::clone(&relay);
    let signal_thread = std::thread::spawn(move || {
        for signal in signals.forever() {
            lock(&relaying).signal(signal);
        }
    });
    let reading = Arc::clone(&relay);
    let status_thread = std::thread::spawn(move || read_status(status_read, &reading));
    let waited = child.wait();
    let reported = status_thread.join().ok().flatten();
    handle.close();
    let _ = signal_thread.join();
    let status = waited?;
    Ok(reported.unwrap_or_else(|| {
        let code = status
            .code()
            .or_else(|| status.signal().map(|signal| 128 + signal))
            .unwrap_or(1);
        eprintln!("agterm: bwrap ended before it reported the program's status ({status}); exiting with {code}");
        code
    }))
}

/// A pipe whose write end `bwrap` inherits (no close-on-exec) and whose read end it does not.
fn status_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors `pipe2` writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `pipe2` just opened both descriptors, owned by nothing else.
    let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: as above.
    let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    // SAFETY: a live descriptor of ours; clearing its flags keeps it open across exec.
    if unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETFD, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((read, write))
}

/// Reads `bwrap`'s status lines until it closes them: the program's pid goes to `relay`; the exit code it reports last
/// is returned.
fn read_status(read: OwnedFd, relay: &Mutex<Relay>) -> Option<i32> {
    let reader = BufReader::new(std::fs::File::from(read));
    let mut exit = None;
    for line in reader.lines().map_while(Result::ok) {
        match parse_status(&line) {
            Some(Status::ChildPid(pid)) => lock(relay).started(pid),
            Some(Status::ExitCode(code)) => exit = Some(code),
            None => {}
        }
    }
    exit
}

/// What one of `bwrap`'s JSON status lines says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    ChildPid(i32),
    ExitCode(i32),
}

fn parse_status(line: &str) -> Option<Status> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let field = |name: &str| value.get(name)?.as_i64().and_then(|number| i32::try_from(number).ok());
    field("exit-code")
        .map(Status::ExitCode)
        .or_else(|| field("child-pid").map(Status::ChildPid))
}

/// Where relayed signals go: the program once its pid is known, queued until then.
#[derive(Debug, Default)]
struct Relay {
    pid: Option<i32>,
    pending: Vec<i32>,
}

impl Relay {
    fn signal(&mut self, signal: i32) {
        match self.pid {
            Some(pid) => deliver(pid, signal),
            None => self.pending.push(signal),
        }
    }

    fn started(&mut self, pid: i32) {
        self.pid = Some(pid);
        for signal in std::mem::take(&mut self.pending) {
            deliver(pid, signal);
        }
    }
}

fn deliver(pid: i32, signal: i32) {
    // SAFETY: `kill` only sends a signal; a program that already ended makes it fail with ESRCH, which is fine.
    let _ = unsafe { libc::kill(pid, signal) };
}

fn lock(relay: &Mutex<Relay>) -> std::sync::MutexGuard<'_, Relay> {
    relay.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines_give_the_pid_and_the_exit_code() {
        assert_eq!(
            parse_status(r#"{ "child-pid": 4242, "cgroup-namespace": 4026531835 }"#),
            Some(Status::ChildPid(4242))
        );
        assert_eq!(parse_status(r#"{ "exit-code": 143 }"#), Some(Status::ExitCode(143)));
        assert_eq!(parse_status("not json"), None);
        assert_eq!(parse_status(r#"{ "user-namespace": 1 }"#), None);
    }

    #[test]
    fn a_signal_before_the_pid_waits_for_it() {
        let mut relay = Relay::default();
        relay.signal(SIGTERM);
        relay.signal(SIGINT);
        assert_eq!(relay.pending, [SIGTERM, SIGINT]);
        // a pid that names no process: delivery fails quietly, and the queue is emptied
        relay.started(i32::MAX);
        assert!(relay.pending.is_empty());
        assert_eq!(relay.pid, Some(i32::MAX));
    }
}
