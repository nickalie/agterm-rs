//! What the local-only subcommands (`clipboard set`, `terminfo install`) share: they run child processes instead of
//! talking to the socket.

use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

/// SIGPIPE ignored while it lives, the previous action restored on drop. `main` puts SIGPIPE back to its default so a
/// closed stdout ends the CLI quietly; a write into a child's stdin must instead get `EPIPE` when the child exits
/// without reading, so its own status is reported (upstream's `F_SETNOSIGPIPE` on the pipe, which Linux lacks). The
/// disposition is process-wide: the local subcommands run on the main thread, before any output is printed.
pub struct SigpipeIgnored {
    previous: libc::sighandler_t,
}

impl SigpipeIgnored {
    #[must_use]
    pub fn new() -> Self {
        // SAFETY: `signal` with a valid signal number and `SIG_IGN` installs no handler code.
        let previous = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
        Self { previous }
    }
}

impl Drop for SigpipeIgnored {
    fn drop(&mut self) {
        if self.previous != libc::SIG_ERR {
            // SAFETY: `previous` is the action `signal` returned for SIGPIPE, so it is a valid one to reinstate.
            unsafe { libc::signal(libc::SIGPIPE, self.previous) };
        }
    }
}

/// A child's exit status as Foundation's `terminationStatus` reads it: the exit code, else the signal that ended it.
#[must_use]
pub fn exit_status(status: ExitStatus) -> i32 {
    status.code().or_else(|| status.signal()).unwrap_or(-1)
}

/// Every signal unblocked and back to its default action, for a child about to exec (upstream's
/// `POSIX_SPAWN_SETSIGMASK` with an empty mask and `POSIX_SPAWN_SETSIGDEF` with every signal): the mask and ignored
/// actions survive `exec` (a handler does not), so a caller blocking or ignoring SIGTERM would otherwise leave the
/// child deaf to it. Only `sigemptyset`, `sigprocmask` and `signal` run, all async-signal-safe, so this may run
/// between `fork` and `exec`.
pub fn reset_signals() {
    // SAFETY: a zeroed set is initialized by `sigemptyset` before use.
    let mut none: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `none` is a valid set to clear.
    unsafe { libc::sigemptyset(&raw mut none) };
    // SAFETY: `none` is a valid, empty set; the old mask is not asked for.
    unsafe { libc::sigprocmask(libc::SIG_SETMASK, &raw const none, std::ptr::null_mut()) };
    for signal in 1..=64 {
        // SAFETY: `signal` with `SIG_DFL` installs no handler code; SIGKILL, SIGSTOP and the numbers glibc reserves
        // answer EINVAL, which leaves them as they are.
        unsafe { libc::signal(signal, libc::SIG_DFL) };
    }
}

/// Marks every descriptor above the standard three close-on-exec, for a child about to exec, so it inherits only
/// stdin, stdout and stderr (upstream's `POSIX_SPAWN_CLOEXEC_DEFAULT`): std opens its own descriptors close-on-exec,
/// but one the caller leaked would otherwise reach the child. Marked rather than closed, so std's own exec-error pipe
/// still reports a failed exec. `close_range` is a plain system call, safe between `fork` and `exec`; a kernel older
/// than 5.11 refuses the flag and the descriptors stay as they are.
pub fn close_inherited_descriptors() {
    // SAFETY: `close_range` takes plain integers and only changes descriptor flags.
    unsafe { libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, libc::CLOSE_RANGE_CLOEXEC) };
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn an_exit_code_reads_as_itself_and_a_signal_as_its_number() {
        let exited = Command::new("/bin/sh").args(["-c", "exit 7"]).status().unwrap();
        assert_eq!(exit_status(exited), 7);
        let killed = Command::new("/bin/sh").args(["-c", "kill -TERM $$"]).status().unwrap();
        assert_eq!(exit_status(killed), libc::SIGTERM);
    }

    #[test]
    fn sigpipe_comes_back_to_its_previous_action() {
        let read = || {
            // SAFETY: reading the action back by installing `SIG_IGN`; no handler code is involved.
            let current = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
            // SAFETY: `current` is the action `signal` just returned for SIGPIPE.
            unsafe { libc::signal(libc::SIGPIPE, current) };
            current
        };
        let before = read();
        {
            let _ignored = SigpipeIgnored::new();
            assert_eq!(read(), libc::SIG_IGN);
        }
        assert_eq!(read(), before);
    }
}
