//! The spawn on Linux (and any Unix but macOS): a child that starts a session of its own before it runs the server.
//!
//! herdr: `src/platform/mod.rs` (`detach_server_daemon_command`, `current_process_is_detached_server_daemon`) at
//! `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified. Kept: `setsid` in the child
//! before `exec` (`pre_exec`), stdio on the null device; the server is detached when it leads its own session.
//! Changed: the child also marks every descriptor past stdio close-on-exec (`close_range`, or one by one where the
//! kernel or a syscall filter refuses it), so a descriptor of the launcher's that is not (a library's, or an
//! inheritable one handed to it) never reaches the server, as the macOS spawn's `POSIX_SPAWN_CLOEXEC_DEFAULT` does;
//! a descriptor that cannot be marked fails the start; and a thread reaps the child, since a long-running launcher
//! would otherwise keep the exited server as a zombie.
//!
//! The child between `fork` and `exec` runs only async-signal-safe system calls (`setsid`, `close_range`, `fcntl`)
//! and allocates nothing, never anything that could wait on a lock another thread of the launcher held at the fork.

use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use libc::{c_int, c_uint};

use super::reaper;
use super::{ServerCommand, SpawnPath};

pub(super) use reaper::Process;

/// The first descriptor past stdio.
const FIRST_INHERITED: c_int = 3;

/// The most descriptors the one-by-one fallback marks: the kernel's default `fs.nr_open`, past which no process
/// opens a descriptor unless that limit was raised.
const MAX_DESCRIPTORS: c_int = 1 << 20;

pub(super) fn spawn(command: &ServerCommand) -> io::Result<(u32, SpawnPath, Process)> {
    let end = descriptor_end();
    let mut child = Command::new(&command.executable);
    child
        .args(&command.args)
        .env_clear()
        .envs(command.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the hook runs in the forked child before exec and only makes async-signal-safe system calls, allocating
    // nothing (`detach`).
    unsafe { child.pre_exec(move || detach(end)) };
    let mut child = child.spawn()?;
    let pid = child.id();
    let process = Process::reap(move || child.wait())?;
    Ok((pid, SpawnPath::Detached, process))
}

/// In the child: a session of its own, and nothing but stdio inherited (descriptors below `end`, where `close_range`
/// is refused).
fn detach(end: c_int) -> io::Result<()> {
    rustix::process::setsid()?;
    close_on_exec_past_stdio(close_range_cloexec, FIRST_INHERITED..end)
}

/// Marks every descriptor past stdio close-on-exec: by `range`, or where that fails (a kernel before 5.11 has no
/// `CLOSE_RANGE_CLOEXEC`, a syscall filter may refuse `close_range`), each of `each`, skipping those not open.
///
/// # Errors
///
/// A descriptor that is open could not be marked.
fn close_on_exec_past_stdio(
    range: impl FnOnce() -> io::Result<()>,
    each: impl IntoIterator<Item = c_int>,
) -> io::Result<()> {
    if range().is_ok() {
        return Ok(());
    }
    for descriptor in each {
        // SAFETY: fcntl takes integers only; a descriptor that is not open fails with EBADF.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EBADF) {
                return Err(error);
            }
        }
    }
    Ok(())
}

/// `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)`.
fn close_range_cloexec() -> io::Result<()> {
    // SAFETY: close_range takes integers only, and with CLOSE_RANGE_CLOEXEC closes nothing but marks.
    let result = unsafe { libc::syscall(libc::SYS_close_range, 3_u32, c_uint::MAX, libc::CLOSE_RANGE_CLOEXEC) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The end of the descriptors the fallback marks, read before the fork: the soft `RLIMIT_NOFILE`, which no
/// descriptor opened under it reaches, capped at [`MAX_DESCRIPTORS`].
fn descriptor_end() -> c_int {
    rustix::process::getrlimit(rustix::process::Resource::Nofile)
        .current
        .and_then(|soft| c_int::try_from(soft).ok())
        .map_or(MAX_DESCRIPTORS, |soft| soft.min(MAX_DESCRIPTORS))
}

pub(super) fn current_process_is_detached() -> bool {
    rustix::process::getsid(None).is_ok_and(|session| session == rustix::process::getpid())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pipe whose ends are not close-on-exec.
    fn inheritable_pipe() -> [c_int; 2] {
        let mut ends = [0; 2];
        // SAFETY: `ends` is writable for the two descriptors pipe(2) makes, without O_CLOEXEC.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        ends
    }

    fn close_on_exec(descriptor: c_int) -> bool {
        // SAFETY: fcntl takes integers only.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        assert_ne!(flags, -1, "{}", io::Error::last_os_error());
        flags & libc::FD_CLOEXEC != 0
    }

    fn close(ends: [c_int; 2]) {
        for end in ends {
            // SAFETY: the descriptor is this test's own, closed once.
            unsafe { libc::close(end) };
        }
    }

    #[test]
    fn a_refused_close_range_falls_back_to_marking_each_descriptor() {
        let ends = inheritable_pipe();
        assert!(!close_on_exec(ends[0]) && !close_on_exec(ends[1]));
        // a descriptor past the limit is not open, and is skipped
        let refused = || Err(io::Error::from_raw_os_error(libc::ENOSYS));
        close_on_exec_past_stdio(refused, [ends[0], ends[1], MAX_DESCRIPTORS]).unwrap();
        assert!(close_on_exec(ends[0]) && close_on_exec(ends[1]));
        close(ends);
    }

    #[test]
    fn a_close_range_that_succeeds_needs_no_fallback() {
        let ends = inheritable_pipe();
        close_on_exec_past_stdio(|| Ok(()), ends).unwrap();
        assert!(!close_on_exec(ends[0]) && !close_on_exec(ends[1]));
        close(ends);
    }

    #[test]
    fn the_fallback_covers_the_descriptor_limit() {
        let end = descriptor_end();
        assert!(end > FIRST_INHERITED && end <= MAX_DESCRIPTORS, "{end}");
        let ends = inheritable_pipe();
        assert!(ends.iter().all(|&descriptor| descriptor < end), "{ends:?} past {end}");
        close(ends);
    }
}
