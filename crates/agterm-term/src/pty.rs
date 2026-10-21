//! The PTY side of a pane over `agterm-pty` (openpty on Unix, `ConPTY` on Windows, herdr's PTY actor moving the
//! bytes): what a pane runs, its grid as the PTY's size, the outlet the terminal's replies leave through, and what the
//! pane reads of the PTY itself (its foreground process group and its input modes).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use agterm_pty::{ActorHandle, WriteError};

use crate::PaneError;
use crate::session_io::{self, Live};

/// What a pane runs, and how: `agterm-pty`'s command (its program, arguments, working directory and environment).
pub use agterm_pty::Command as PaneCommand;

/// A pane's grid: cells and the pixel size of one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u32,
    pub cell_height: u32,
}

impl GridSize {
    pub(crate) fn pty(self) -> agterm_pty::Size {
        agterm_pty::Size {
            rows: self.rows,
            cols: self.cols,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

impl From<WriteError> for PaneError {
    fn from(err: WriteError) -> Self {
        match err {
            WriteError::Backpressure => Self::Backpressure,
            WriteError::Closed => Self::Closed,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where the terminal's replies to the program go: the PTY actor's outbox, in the order they are made (never
/// refused; the actor drops them past its hard cap, `agterm_pty::REPLY_CAP`). Replies made before the actor runs
/// (a terminal driven without a PTY) wait here, and none are taken once the pane closed it. A live pane's terminal
/// answers the program nothing but Kitty graphics commands, the server answering the rest
/// ([`crate::session_io`]): those answers are kept for the server's next barrier.
///
/// A reply is queued with [`ActorHandle::write_reply`], which waits for a read callback in progress. That is safe
/// under the VT lock because the pane's read callback only copies into the pane's mailbox ([`crate::mailbox`]) and
/// never takes the VT lock.
#[derive(Debug, Default)]
pub(crate) struct ReplyOutlet {
    state: Mutex<Outlet>,
}

#[derive(Debug)]
enum Outlet {
    /// No actor yet: the replies so far, and their bytes.
    Buffered(Vec<Vec<u8>>, usize),
    Actor(ActorHandle),
    /// A live pane's: its Kitty graphics answers since the last barrier, and the attach the pane's own input goes to.
    Live(Vec<u8>, Arc<Live>),
    Closed,
}

impl Default for Outlet {
    fn default() -> Self {
        Self::Buffered(Vec::new(), 0)
    }
}

impl ReplyOutlet {
    /// Queue a reply for the program.
    pub(crate) fn push_reply(&self, data: &[u8]) {
        let mut state = lock(&self.state);
        match &mut *state {
            Outlet::Buffered(replies, bytes) => {
                replies.push(data.to_vec());
                *bytes += data.len();
            }
            Outlet::Actor(actor) => {
                let actor = actor.clone();
                drop(state);
                actor.write_reply(data);
            }
            Outlet::Live(answers, _) => session_io::keep_kitty_answer(answers, data),
            Outlet::Closed => {}
        }
    }

    /// Queue bytes the pane itself sends the program (a clear's form feed): a reply on a PTY, a live pane's input.
    pub(crate) fn push_input(&self, data: &[u8]) {
        let state = lock(&self.state);
        if let Outlet::Live(_, live) = &*state {
            let live = Arc::clone(live);
            drop(state);
            live.program_input(data);
        } else {
            drop(state);
            self.push_reply(data);
        }
    }

    /// Answer as a live pane's terminal from now on (before it parses anything).
    pub(crate) fn go_live(&self, live: Arc<Live>) {
        *lock(&self.state) = Outlet::Live(Vec::new(), live);
    }

    /// A live pane's Kitty graphics answers since the last call.
    pub(crate) fn take_kitty_answers(&self) -> Vec<u8> {
        match &mut *lock(&self.state) {
            Outlet::Live(answers, _) => std::mem::take(answers),
            _ => Vec::new(),
        }
    }

    /// Send replies to `actor` from now on, the ones made so far first.
    pub(crate) fn attach(&self, actor: &ActorHandle) {
        let mut state = lock(&self.state);
        if matches!(*state, Outlet::Closed) {
            return;
        }
        if let Outlet::Buffered(replies, _) = std::mem::replace(&mut *state, Outlet::Actor(actor.clone())) {
            for reply in &replies {
                actor.write_reply(reply);
            }
        }
    }

    /// Take no more replies, and let go of the actor.
    pub(crate) fn close(&self) {
        *lock(&self.state) = Outlet::Closed;
    }

    /// The reply bytes waiting for the actor, or queued in it with the program's input.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        let state = lock(&self.state);
        match &*state {
            Outlet::Buffered(_, bytes) => *bytes,
            Outlet::Actor(actor) => actor.queued(),
            Outlet::Live(answers, _) => answers.len(),
            Outlet::Closed => 0,
        }
    }
}

/// What the pane reads of its PTY on its own thread, without asking the actor (which would wait on its thread): a
/// close-on-exec duplicate of the master on Unix, for `tcgetpgrp` and `tcgetattr`. Windows' pseudoconsole has
/// neither: the parent holds only its pipes and its handle, not the console's input buffer whose mode says whether
/// the program echoes, and no process group is the console's foreground.
#[derive(Debug)]
pub(crate) struct Tty {
    #[cfg(unix)]
    master: std::os::fd::OwnedFd,
}

impl Tty {
    /// Keep what the pane reads of `master` before the actor takes it.
    #[cfg(unix)]
    pub(crate) fn of(master: &agterm_pty::Master) -> Result<Self, PaneError> {
        use std::os::fd::AsFd;

        let master = master
            .as_fd()
            .try_clone_to_owned()
            .map_err(|err| PaneError::Pty(err.to_string()))?;
        Ok(Self { master })
    }

    #[cfg(windows)]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the same signature as the Unix form, which duplicates a descriptor"
    )]
    pub(crate) fn of(_master: &agterm_pty::Master) -> Result<Self, PaneError> {
        Ok(Self {})
    }

    /// The PTY's foreground process group (`tcgetpgrp`), the pid of its leader; `None` when the PTY cannot say (the
    /// child side closed), and on Windows.
    #[cfg(unix)]
    pub(crate) fn foreground_pid(&self) -> Option<u32> {
        use std::os::fd::AsRawFd;

        // SAFETY: `tcgetpgrp` reads the process group of the master this struct owns; no memory is passed.
        let pgrp = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        u32::try_from(pgrp).ok().filter(|pgrp| *pgrp > 0)
    }

    #[cfg(windows)]
    #[expect(
        clippy::unused_self,
        reason = "the same signature as the Unix form, which reads the master"
    )]
    pub(crate) fn foreground_pid(&self) -> Option<u32> {
        None
    }

    /// Canonical input with echo off (`tcgetattr`): the program reads a password. `false` when the PTY cannot say,
    /// and always on Windows.
    #[cfg(unix)]
    pub(crate) fn password_input(&self) -> bool {
        use std::os::fd::AsRawFd;

        let mut termios = std::mem::MaybeUninit::<libc::termios>::zeroed();
        // SAFETY: the descriptor is the master this struct owns; `tcgetattr` only writes a `termios` into the buffer,
        // which is large enough and zeroed (a valid `termios`) whether or not it succeeds.
        let read = unsafe { libc::tcgetattr(self.master.as_raw_fd(), termios.as_mut_ptr()) };
        if read != 0 {
            return false;
        }
        // SAFETY: zero-initialized above and filled by the successful `tcgetattr`.
        let modes = unsafe { termios.assume_init() }.c_lflag;
        modes & libc::ICANON != 0 && modes & libc::ECHO == 0
    }

    #[cfg(windows)]
    #[expect(
        clippy::unused_self,
        reason = "the same signature as the Unix form, which reads the master"
    )]
    pub(crate) fn password_input(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_becomes_the_ptys_size() {
        let size = GridSize {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        assert_eq!(size.pty().pixels(), (800, 480));
        assert_eq!((size.pty().rows, size.pty().cols), (24, 80));
    }

    #[test]
    fn replies_wait_for_the_actor_and_none_are_taken_once_closed() {
        let outlet = ReplyOutlet::default();
        assert_eq!(outlet.queued(), 0);
        outlet.push_reply(b"\x1b[0n");
        outlet.push_reply(b"\x1b[1;1R");
        assert_eq!(outlet.queued(), 10);
        outlet.close();
        outlet.push_reply(b"late");
        assert_eq!(outlet.queued(), 0);
    }

    #[test]
    fn write_errors_map_to_the_panes() {
        assert_eq!(PaneError::from(WriteError::Backpressure), PaneError::Backpressure);
        assert_eq!(PaneError::from(WriteError::Closed), PaneError::Closed);
    }
}
