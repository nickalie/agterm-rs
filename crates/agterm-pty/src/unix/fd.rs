//! Descriptor plumbing of the Unix actor: the master's flags, the wake pipe, `poll` over both, the window size and
//! the terminal's foreground process group.
//!
//! herdr: `src/pty/fd.rs` (`set_cloexec`, `set_nonblocking`, `WakeWriter`, `create_wake_pipe`, `drain_wake_fd`,
//! `poll_pty_and_wake`, `resize_pty_fd`) at `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0,
//! modified: the wake pipe from `pipe2` where the libc has it (not Apple's, where `pipe` and `fcntl` remain, as in
//! herdr); duplicates through std's `try_clone_to_owned` (`F_DUPFD_CLOEXEC`, one call) in place of `dup` and
//! `fcntl`; borrowed and owned descriptors in place of raw ones.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use crate::Size;

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Add `flag` to the descriptor flags (`F_GETFD`/`F_SETFD`) or the status flags (`F_GETFL`/`F_SETFL`) of `fd`.
fn add_flag(fd: BorrowedFd<'_>, get: libc::c_int, set: libc::c_int, flag: libc::c_int) -> io::Result<()> {
    // SAFETY: `fcntl` reads the flags of a descriptor the borrow keeps open; no memory is passed.
    let flags = check(unsafe { libc::fcntl(fd.as_raw_fd(), get) })?;
    if flags & flag == 0 {
        // SAFETY: as above, setting them.
        check(unsafe { libc::fcntl(fd.as_raw_fd(), set, flags | flag) })?;
    }
    Ok(())
}

pub(crate) fn set_cloexec(fd: BorrowedFd<'_>) -> io::Result<()> {
    add_flag(fd, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC)
}

pub(crate) fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    add_flag(fd, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK)
}

/// A pipe whose write end wakes the actor out of `poll`; both ends close-on-exec and non-blocking. Returns (read,
/// write).
pub(crate) fn wake_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1 as libc::c_int; 2];
    #[cfg(not(target_vendor = "apple"))]
    // SAFETY: `fds` has room for the two descriptors `pipe2` writes on success.
    check(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) })?;
    #[cfg(target_vendor = "apple")]
    // SAFETY: `fds` has room for the two descriptors `pipe` writes on success.
    check(unsafe { libc::pipe(fds.as_mut_ptr()) })?;
    // SAFETY: both descriptors were just created and nothing else owns them.
    let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: as above.
    let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    #[cfg(target_vendor = "apple")]
    for fd in [&read, &write] {
        use std::os::fd::AsFd;
        set_cloexec(fd.as_fd())?;
        set_nonblocking(fd.as_fd())?;
    }
    Ok((read, write))
}

/// Write one byte to the wake pipe; a full pipe is already awake.
pub(crate) fn wake(fd: BorrowedFd<'_>) -> io::Result<()> {
    let byte = 1_u8;
    loop {
        // SAFETY: writes one byte from a live local to a descriptor the borrow keeps open.
        if unsafe { libc::write(fd.as_raw_fd(), (&raw const byte).cast(), 1) } >= 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::WouldBlock => return Ok(()),
            io::ErrorKind::Interrupted => {}
            _ => return Err(err),
        }
    }
}

/// Empty the wake pipe.
pub(crate) fn drain(fd: BorrowedFd<'_>) -> io::Result<()> {
    let mut buffer = [0_u8; 64];
    loop {
        // SAFETY: reads into a live local buffer of the length passed, from a descriptor the borrow keeps open.
        let read = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if read == 0 {
            return Ok(());
        }
        if read > 0 {
            continue;
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::WouldBlock => return Ok(()),
            io::ErrorKind::Interrupted => {}
            _ => return Err(err),
        }
    }
}

/// What `poll` reported.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ready {
    /// The PTY has bytes, or hung up (the next read says which).
    pub(crate) read: bool,
    /// The PTY takes bytes (or hung up).
    pub(crate) write: bool,
    /// The wake pipe was written.
    pub(crate) wake: bool,
}

/// Wait up to `timeout_ms` (-1: forever) for the PTY to be readable (`read`) or writable (`write`), or for the wake
/// pipe. With neither interest the PTY is not polled at all, so a hangup does not wake a quiesced actor.
pub(crate) fn poll(
    pty: BorrowedFd<'_>,
    wake: BorrowedFd<'_>,
    read: bool,
    write: bool,
    timeout_ms: i32,
) -> io::Result<Ready> {
    let mut events = 0;
    if read {
        events |= libc::POLLIN;
    }
    if write {
        events |= libc::POLLOUT;
    }
    let polled = read || write;
    let mut fds = [
        libc::pollfd {
            fd: if polled { pty.as_raw_fd() } else { -1 },
            events,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let deadline = u64::try_from(timeout_ms)
        .ok()
        .map(|ms| Instant::now() + Duration::from_millis(ms));
    let mut timeout = timeout_ms;
    loop {
        for fd in &mut fds {
            fd.revents = 0;
        }
        // SAFETY: `fds` is a live array of two `pollfd`s whose descriptors the borrows keep open (or -1, ignored).
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) } >= 0 {
            break;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
        if let Some(deadline) = deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(Ready::default());
            }
            timeout = i32::try_from(left.as_millis()).unwrap_or(i32::MAX).max(1);
        }
    }
    let (pty, wake) = (fds[0].revents, fds[1].revents);
    if (pty | wake) & libc::POLLNVAL != 0 {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "poll found an invalid PTY actor descriptor",
        ));
    }
    if pty & libc::POLLERR != 0 {
        return Err(io::Error::new(io::ErrorKind::BrokenPipe, "poll found the PTY in error"));
    }
    Ok(Ready {
        read: pty & (libc::POLLIN | libc::POLLHUP) != 0,
        write: pty & (libc::POLLOUT | libc::POLLHUP) != 0,
        wake: wake & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0,
    })
}

/// Set the PTY's window size (`TIOCSWINSZ`), which signals the child's foreground process group.
pub(crate) fn resize(fd: BorrowedFd<'_>, size: Size) -> io::Result<()> {
    let (width, height) = size.pixels();
    let size = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: width,
        ws_ypixel: height,
    };
    // SAFETY: `TIOCSWINSZ` reads one `winsize` from the live local passed, on a descriptor the borrow keeps open.
    check(unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) })?;
    Ok(())
}

/// The terminal's foreground process group, if it has one.
pub(crate) fn foreground_process_group(fd: BorrowedFd<'_>) -> Option<u32> {
    // SAFETY: `tcgetpgrp` reads the foreground group of a descriptor the borrow keeps open; no memory is passed.
    let group = unsafe { libc::tcgetpgrp(fd.as_raw_fd()) };
    u32::try_from(group).ok().filter(|group| *group > 0)
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use super::*;

    #[test]
    fn the_wake_pipe_is_close_on_exec_and_non_blocking() {
        let (read, write) = wake_pipe().unwrap();
        for fd in [&read, &write] {
            // SAFETY: reads the flags of a descriptor owned here.
            let fd_flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
            // SAFETY: as above.
            let status = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
            assert_ne!(fd_flags & libc::FD_CLOEXEC, 0);
            assert_ne!(status & libc::O_NONBLOCK, 0);
        }
    }

    #[test]
    fn a_wake_is_seen_by_poll_and_drained() {
        let (read, write) = wake_pipe().unwrap();
        let (pty, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        assert_eq!(
            poll(pty.as_fd(), read.as_fd(), true, false, 0).unwrap(),
            Ready::default()
        );
        // a full pipe is still awake: many wakes never fail
        for _ in 0..100_000 {
            wake(write.as_fd()).unwrap();
        }
        let ready = poll(pty.as_fd(), read.as_fd(), true, false, 1000).unwrap();
        assert!(ready.wake && !ready.read);
        drain(read.as_fd()).unwrap();
        assert!(!poll(pty.as_fd(), read.as_fd(), true, false, 0).unwrap().wake);
    }

    #[test]
    fn a_hangup_is_ignored_without_pty_interest() {
        let (read, _write) = wake_pipe().unwrap();
        let (pty, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(peer);
        assert_eq!(
            poll(pty.as_fd(), read.as_fd(), false, false, 0).unwrap(),
            Ready::default()
        );
        assert!(poll(pty.as_fd(), read.as_fd(), true, false, 0).unwrap().read);
    }

    #[test]
    fn flags_are_added_once() {
        let (pty, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        set_nonblocking(pty.as_fd()).unwrap();
        set_nonblocking(pty.as_fd()).unwrap();
        set_cloexec(pty.as_fd()).unwrap();
        // SAFETY: reads the status flags of a descriptor the stream owns.
        let status = unsafe { libc::fcntl(pty.as_raw_fd(), libc::F_GETFL) };
        assert_ne!(status & libc::O_NONBLOCK, 0);
    }
}
