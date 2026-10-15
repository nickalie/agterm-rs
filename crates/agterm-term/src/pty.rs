//! The PTY side of a pane: spawning the child, the bounded queue of bytes for it and the thread that writes them, and
//! the wake pipe that interrupts the IO thread's `poll`.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use portable_pty::{CommandBuilder, MasterPty, PtySize};

use crate::PaneError;

/// What a pane runs, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    /// The working directory; `None` keeps the current one.
    pub cwd: Option<PathBuf>,
    /// Variables set on top of the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    /// Inherited variables removed (before `env` is applied).
    pub env_remove: Vec<OsString>,
}

impl PaneCommand {
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub fn env_remove(mut self, key: impl Into<OsString>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    pub(crate) fn builder(&self) -> CommandBuilder {
        let mut builder = CommandBuilder::new(&self.program);
        builder.args(&self.args);
        for key in &self.env_remove {
            builder.env_remove(key);
        }
        for (key, value) in &self.env {
            builder.env(key, value);
        }
        match &self.cwd {
            Some(cwd) => builder.cwd(cwd),
            None => {
                if let Ok(cwd) = std::env::current_dir() {
                    builder.cwd(cwd);
                }
            }
        }
        builder
    }
}

/// A pane's grid: cells and the pixel size of one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u32,
    pub cell_height: u32,
}

impl GridSize {
    pub(crate) fn pty(self) -> PtySize {
        let pixels = |cells: u16, cell: u32| u16::try_from(u32::from(cells).saturating_mul(cell)).unwrap_or(u16::MAX);
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: pixels(self.cols, self.cell_width),
            pixel_height: pixels(self.rows, self.cell_height),
        }
    }
}

/// Bytes waiting for the child: input is bounded by the queue's limit (backpressure to the frontend); the terminal's
/// own replies are always queued, up to a hard cap past which a program that floods queries without reading its
/// input loses replies (the IO thread never stops reading output for them: a child blocked writing output while we
/// wait for it to read input would deadlock).
#[derive(Debug)]
pub(crate) struct WriteQueue {
    state: Mutex<QueueState>,
    /// The writer waits here for bytes.
    ready: Condvar,
    limit: usize,
}

/// The most bytes queued before terminal replies are dropped (when the input limit is lower).
const REPLY_CAP: usize = 16 * 1024 * 1024;

#[derive(Debug, Default)]
struct QueueState {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    /// The PTY refused a write, or the pane is shutting down: nothing more is written.
    closed: bool,
}

impl WriteQueue {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
            limit,
        }
    }

    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue input for the child. A chunk that does not fit next to what is queued is refused whole
    /// ([`PaneError::Backpressure`]); an empty queue takes any size.
    pub(crate) fn push_input(&self, data: &[u8]) -> Result<(), PaneError> {
        let mut state = self.lock();
        if state.closed {
            return Err(PaneError::Closed);
        }
        if state.bytes > 0 && state.bytes + data.len() > self.limit {
            return Err(PaneError::Backpressure);
        }
        Self::push(&mut state, data);
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    /// Queue a reply of the terminal's (never blocks; dropped past [`REPLY_CAP`] queued bytes).
    pub(crate) fn push_reply(&self, data: &[u8]) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        if state.bytes + data.len() > REPLY_CAP.max(self.limit) {
            tracing::warn!(
                queued = state.bytes,
                "the child reads no input; a terminal reply is dropped"
            );
            return;
        }
        Self::push(&mut state, data);
        drop(state);
        self.ready.notify_one();
    }

    fn push(state: &mut QueueState, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        state.bytes += data.len();
        state.chunks.push_back(data.to_vec());
    }

    /// The next chunk for the writer, waiting for one; `None` once closed.
    fn pop(&self) -> Option<Vec<u8>> {
        let mut state = self.lock();
        loop {
            if state.closed {
                return None;
            }
            if let Some(chunk) = state.chunks.pop_front() {
                state.bytes -= chunk.len();
                return Some(chunk);
            }
            state = self.ready.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Stop writing: queued bytes are dropped and every waiter wakes.
    pub(crate) fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        state.chunks.clear();
        state.bytes = 0;
        drop(state);
        self.ready.notify_all();
    }

    pub(crate) fn queued(&self) -> usize {
        self.lock().bytes
    }
}

/// Write queued chunks to the PTY until the queue closes or the PTY refuses a write (the child side is gone).
pub(crate) fn write_loop(queue: &WriteQueue, mut writer: Box<dyn Write + Send>) {
    while let Some(chunk) = queue.pop() {
        if let Err(err) = writer.write_all(&chunk).and_then(|()| writer.flush()) {
            tracing::debug!(%err, "pty write failed; the pane stops writing");
            queue.close();
            return;
        }
    }
}

/// A duplicate of the master's descriptor (close-on-exec) for the IO thread to poll and read.
pub(crate) fn reader(master: &dyn MasterPty) -> Result<File, PaneError> {
    let fd = master
        .as_raw_fd()
        .ok_or_else(|| PaneError::Pty("the pty master has no descriptor".to_owned()))?;
    // SAFETY: `fd` is the master's open descriptor; F_DUPFD_CLOEXEC returns a new descriptor (or -1) and touches no
    // memory.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(PaneError::Pty(std::io::Error::last_os_error().to_string()));
    }
    // SAFETY: `duplicate` is a fresh descriptor nothing else owns.
    Ok(File::from(unsafe { OwnedFd::from_raw_fd(duplicate) }))
}

/// A self-pipe that wakes the IO thread out of `poll` (pane shutdown).
#[derive(Debug)]
pub(crate) struct WakePipe {
    read: OwnedFd,
    write: OwnedFd,
}

impl WakePipe {
    pub(crate) fn new() -> Result<Self, PaneError> {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `fds` has room for the two descriptors `pipe2` writes on success.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return Err(PaneError::Pty(std::io::Error::last_os_error().to_string()));
        }
        // SAFETY: both descriptors were just created and are owned by nothing else.
        let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        // SAFETY: as above.
        let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        Ok(Self { read, write })
    }

    pub(crate) fn wake(&self) {
        let byte = 1_u8;
        // SAFETY: writes one byte from a live local to an owned, non-blocking descriptor; a full pipe is already awake.
        unsafe { libc::write(self.write.as_raw_fd(), (&raw const byte).cast(), 1) };
    }
}

/// What `poll` reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ready {
    /// The PTY has bytes (or hung up: the next read says which).
    Pty,
    /// The wake pipe was written.
    Woken,
    /// Nothing within the timeout.
    Idle,
}

/// Wait up to `timeout_ms` (-1: forever) for the PTY or the wake pipe.
pub(crate) fn poll(pty: &File, wake: &WakePipe, timeout_ms: i32) -> Result<Ready, PaneError> {
    let mut fds = [
        libc::pollfd {
            fd: pty.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.read.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: `fds` is a live array of two `pollfd`s whose descriptors are owned by the callers' borrows.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if result >= 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(PaneError::Pty(err.to_string()));
        }
    }
    Ok(if fds[1].revents != 0 {
        Ready::Woken
    } else if fds[0].revents != 0 {
        Ready::Pty
    } else {
        Ready::Idle
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn input_over_the_limit_is_refused_but_replies_are_not() {
        let queue = WriteQueue::new(8);
        queue.push_input(b"12345").unwrap();
        assert_eq!(queue.push_input(b"6789"), Err(PaneError::Backpressure));
        queue.push_reply(b"reply-bytes");
        assert_eq!(queue.queued(), 16);
        assert_eq!(queue.pop().as_deref(), Some(&b"12345"[..]));
        assert_eq!(queue.pop().as_deref(), Some(&b"reply-bytes"[..]));
        // an empty queue takes a chunk of any size
        queue.push_input(&[0; 64]).unwrap();
    }

    #[test]
    fn a_closed_queue_refuses_input_and_releases_the_writer() {
        let queue = Arc::new(WriteQueue::new(4));
        let writer = {
            let queue = Arc::clone(&queue);
            std::thread::spawn(move || queue.pop())
        };
        queue.close();
        assert!(writer.join().unwrap().is_none());
        assert_eq!(queue.push_input(b"x"), Err(PaneError::Closed));
        queue.push_reply(b"ignored");
        assert_eq!(queue.queued(), 0);
    }

    #[test]
    fn replies_past_the_cap_are_dropped() {
        let queue = WriteQueue::new(4);
        queue.push_reply(&vec![0; REPLY_CAP]);
        queue.push_reply(b"one too many");
        assert_eq!(queue.queued(), REPLY_CAP);
    }

    #[test]
    fn the_pixel_size_saturates() {
        let size = GridSize {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        assert_eq!((size.pty().pixel_width, size.pty().pixel_height), (800, 480));
        let huge = GridSize {
            cell_width: 10_000,
            ..size
        };
        assert_eq!(huge.pty().pixel_width, u16::MAX);
    }
}
