//! The spawn on Linux (and any Unix but macOS): a child that starts a session of its own before it runs the server.
//!
//! herdr: `src/platform/mod.rs` (`detach_server_daemon_command`, `current_process_is_detached_server_daemon`) at
//! `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified. Kept: `setsid` in the child
//! before `exec` (`pre_exec`), stdio on the null device; the server is detached when it leads its own session.
//! Changed: the child also marks every descriptor past stdio close-on-exec (`close_range`), so a descriptor of the
//! launcher's that is not (a library's, or an inheritable one handed to it) never reaches the server, as the macOS
//! spawn's `POSIX_SPAWN_CLOEXEC_DEFAULT` does; and a thread reaps the child, since a long-running launcher would
//! otherwise keep the exited server as a zombie.
//!
//! The child between `fork` and `exec` runs only async-signal-safe system calls (`setsid`, `close_range`), never
//! anything that could wait on a lock another thread of the launcher held at the fork.

use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use super::reaper;
use super::{ServerCommand, SpawnPath};

pub(super) use reaper::Process;

pub(super) fn spawn(command: &ServerCommand) -> io::Result<(u32, SpawnPath, Process)> {
    let mut child = Command::new(&command.executable);
    child
        .args(&command.args)
        .env_clear()
        .envs(command.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the hook runs in the forked child before exec and only makes async-signal-safe system calls
    // (`detach`).
    unsafe { child.pre_exec(detach) };
    let mut child = child.spawn()?;
    let pid = child.id();
    let process = Process::reap(move || child.wait())?;
    Ok((pid, SpawnPath::Detached, process))
}

/// In the child: a session of its own, and nothing but stdio inherited.
fn detach() -> io::Result<()> {
    rustix::process::setsid()?;
    // best effort: a kernel before 5.11 has no CLOSE_RANGE_CLOEXEC, and then only close-on-exec descriptors stay out
    // SAFETY: close_range takes integers only, and with CLOSE_RANGE_CLOEXEC closes nothing but marks.
    let _ = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3_u32,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    Ok(())
}

pub(super) fn current_process_is_detached() -> bool {
    rustix::process::getsid(None).is_ok_and(|session| session == rustix::process::getpid())
}
