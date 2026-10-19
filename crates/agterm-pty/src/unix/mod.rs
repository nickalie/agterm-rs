//! The PTY on Unix: `openpty` through portable-pty, the child a session and process group leader of its own (its
//! `setsid`), the master's descriptor duplicated close-on-exec for the actor.
//!
//! herdr: `src/pty/backend/unix.rs` (`spawn_with_portable_pty`) at `HERDR_REV` (`build/pins.env`), licensed under the
//! Apache License 2.0, modified: the child is waited for without reaping first (`waitid` with `WNOWAIT`), so its
//! process group is never signalled after its id could be reused; the group's hangup and kill.

mod actor;
mod fd;

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, ExitStatus, native_pty_system};

pub(crate) use actor::{Control, spawn_actor};

use crate::actor::lock;
use crate::{Size, Spawned, pty_error};

/// A PTY's master side, for [`crate::Actor::spawn`].
#[derive(Debug)]
pub struct Master {
    fd: OwnedFd,
}

impl Master {
    /// A master another process handed over (the session server's handoff): the actor sets the flags it needs.
    #[must_use]
    pub fn from_fd(fd: OwnedFd) -> Self {
        Self { fd }
    }
}

impl From<Master> for OwnedFd {
    fn from(master: Master) -> Self {
        master.fd
    }
}

impl AsFd for Master {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// The child on a PTY.
#[derive(Debug)]
pub struct Child {
    inner: Box<dyn portable_pty::Child + Send + Sync>,
    pid: u32,
    /// The child was reaped (or is being): its id, and the group's, may be reused, so nothing is signalled.
    reaped: Arc<Mutex<bool>>,
}

impl Child {
    /// The child's process id, which is also its process group's and its session's.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Wait for the child to exit and reap it; once it is reaped, its status again.
    ///
    /// # Errors
    ///
    /// Waiting failed (the child was reaped by someone else).
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if self.was_reaped() {
            return self.inner.wait();
        }
        exited(self.pid, true)?;
        self.reap()
    }

    /// The child's exit status if it has exited (and is now reaped, or was before), else `None`.
    ///
    /// # Errors
    ///
    /// As [`Child::wait`].
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.was_reaped() {
            return self.inner.try_wait();
        }
        if exited(self.pid, false)? {
            self.reap().map(Some)
        } else {
            Ok(None)
        }
    }

    /// A handle that signals the child's process group from another thread, safe while this one waits.
    #[must_use]
    pub fn killer(&self) -> Killer {
        Killer {
            group: libc::pid_t::try_from(self.pid).unwrap_or(libc::pid_t::MAX),
            reaped: Arc::clone(&self.reaped),
        }
    }

    /// The child was reaped here: its id is released (and may be reused by now), so it is never waited for by id
    /// again; the status std's child kept is the answer.
    fn was_reaped(&self) -> bool {
        *lock(&self.reaped)
    }

    /// The child has exited and is still a zombie: mark it reaped before its id is released, then reap it.
    fn reap(&mut self) -> io::Result<ExitStatus> {
        *lock(&self.reaped) = true;
        self.inner.wait()
    }
}

/// Whether `pid` has exited (with `block`, wait until it has), leaving it a zombie (`WNOWAIT`).
fn exited(pid: u32, block: bool) -> io::Result<bool> {
    let id = libc::id_t::from(pid);
    let options = libc::WEXITED | libc::WNOWAIT | if block { 0 } else { libc::WNOHANG };
    // SAFETY: `siginfo_t` is plain data the kernel fills; all zeroes is its "no child changed state" value.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `waitid` writes one `siginfo_t` into the live local passed.
        if unsafe { libc::waitid(libc::P_PID, id, &raw mut info, options) } == 0 {
            break;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    // SAFETY: `si_pid` reads the field `waitid` filled (or left zero under WNOHANG when nothing exited).
    Ok(block || unsafe { info.si_pid() } != 0)
}

/// Signals a child's process group: [`Child::killer`].
#[derive(Debug, Clone)]
pub struct Killer {
    group: libc::pid_t,
    reaped: Arc<Mutex<bool>>,
}

impl Killer {
    /// `SIGHUP` to the child's process group, as a terminal closing does.
    ///
    /// Once the child is reaped ([`Child::wait`], [`Child::try_wait`]) its group is never signalled, since the id may
    /// be reused by then, so members that outlived the child (a background job) live on: hang up or kill the tree
    /// before waiting. On Windows the job ends the tree either way.
    ///
    /// # Errors
    ///
    /// The signal could not be sent (a group already gone is not an error, nor is a reaped child, which is not
    /// signalled).
    pub fn hang_up(&self) -> io::Result<()> {
        self.signal(libc::SIGHUP)
    }

    /// `SIGKILL` to the child's process group: the child and every process of its group end, unless the child was
    /// already reaped, when nothing is signalled (see [`Killer::hang_up`]).
    ///
    /// # Errors
    ///
    /// As [`Killer::hang_up`].
    pub fn kill_tree(&self) -> io::Result<()> {
        self.signal(libc::SIGKILL)
    }

    fn signal(&self, signal: libc::c_int) -> io::Result<()> {
        // held while signalling: the child cannot be reaped (and its id reused) in between
        let reaped = lock(&self.reaped);
        if *reaped {
            return Ok(());
        }
        // SAFETY: plain `killpg(2)` on the group the unreaped child leads; no memory is passed.
        if unsafe { libc::killpg(self.group, signal) } != 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(err);
            }
        }
        drop(reaped);
        Ok(())
    }
}

pub(crate) fn spawn(builder: CommandBuilder, size: Size) -> io::Result<Spawned> {
    let pair = native_pty_system().openpty(size.pty()).map_err(|err| pty_error(&err))?;
    let raw = pair
        .master
        .as_raw_fd()
        .ok_or_else(|| io::Error::other("the PTY master has no descriptor"))?;
    // SAFETY: `raw` is the master's descriptor, which `pair` keeps open for this borrow.
    let fd = unsafe { BorrowedFd::borrow_raw(raw) }.try_clone_to_owned()?;
    let child = pair.slave.spawn_command(builder).map_err(|err| pty_error(&err))?;
    // portable-pty's own master and slave close: the duplicate is the one master descriptor left in this process
    drop(pair);
    let pid = child
        .process_id()
        .ok_or_else(|| io::Error::other("the PTY child has no process id"))?;
    Ok(Spawned {
        master: Master { fd },
        child: Child {
            inner: child,
            pid,
            reaped: Arc::new(Mutex::new(false)),
        },
    })
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use super::*;

    /// This process's descriptors on a PTY.
    #[cfg(target_os = "linux")]
    fn pty_descriptors() -> usize {
        std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .filter(|target| target.starts_with("/dev/pts/") || target.as_os_str() == "/dev/ptmx")
            .count()
    }

    /// The only unit test that opens a PTY (the actor's run on socket pairs), so the count is this test's own.
    #[test]
    fn spawning_leaves_one_close_on_exec_master_descriptor_in_this_process() {
        #[cfg(target_os = "linux")]
        let before = pty_descriptors();
        let mut builder = CommandBuilder::new("/bin/sh");
        builder.args(["-c", "exit 0"]);
        let Spawned { master, mut child } = spawn(builder, Size::cells(24, 80)).unwrap();
        #[cfg(target_os = "linux")]
        assert_eq!(pty_descriptors(), before + 1);
        child.wait().unwrap();
        // SAFETY: reads the descriptor flags of the master owned here.
        let flags = unsafe { libc::fcntl(master.as_fd().as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        drop(master);
        #[cfg(target_os = "linux")]
        assert_eq!(pty_descriptors(), before);
    }
}
