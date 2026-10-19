//! The PTY actor's platform-neutral half: what it is started with, the handle its users hold, and the outbox of
//! bytes waiting for the child. The IO itself is per OS (`unix.rs`: one thread polling the master and a wake pipe;
//! `windows.rs`: a reader and a writer thread on the pseudoconsole's pipes).
//!
//! herdr: `src/pty/actor.rs` and `src/pty/actor/unix.rs` (`PtyIoActorConfig`, `PtyReadResult`, `PtyIoActorHandle`,
//! its user-write gate and its terminal-response ordering) at `HERDR_REV` (`build/pins.env`), licensed under the
//! Apache License 2.0, modified:
//! - std threads and channels in place of tokio; herdr's input submissions (a prompt, a delay, then Enter) are not
//!   ported;
//! - input and replies share one outbox, bounded in bytes as `agterm-term`'s pane queue is (a chunk that does not fit
//!   beside what is queued is refused, so the frontend sees backpressure; the terminal's replies are always taken, up
//!   to a hard cap), in place of herdr's bounded command channel;
//! - quiesced means the actor neither reads nor writes, on Windows too, and the exit callback says why the actor
//!   stopped.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::{Master, Size, sys};

/// The input bytes an actor queues for the child before it refuses more ([`WriteError::Backpressure`]).
pub const DEFAULT_INPUT_LIMIT: usize = 1024 * 1024;

/// The most bytes queued before the terminal's replies are dropped (when the input limit is lower): a program that
/// floods queries without reading its input loses replies, and the actor never stops reading its output for them (a
/// child blocked writing output while the actor waits for it to read input would deadlock).
pub const REPLY_CAP: usize = 16 * 1024 * 1024;

/// What the actor does with each read of the child's output: its return is the terminal's replies, written back to
/// the child in order. Runs on the actor's thread.
pub type ReadCallback = Box<dyn FnMut(&[u8]) -> Vec<Vec<u8>> + Send>;

/// Called once when the actor stops, with the reason.
pub type ExitCallback = Box<dyn FnOnce(ExitReason) + Send>;

/// Why an actor stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The PTY ended: every descriptor of the child side is closed.
    Eof,
    /// Reading or writing the PTY failed.
    Failed,
    /// [`ActorHandle::shutdown`], or the last handle was dropped.
    Shutdown,
    /// [`ActorHandle::release_after_commit`]: another process took the PTY over.
    Released,
}

/// Why input was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    /// More is queued than the input limit takes: write again once the child has read some.
    #[error("the PTY's input queue is full")]
    Backpressure,
    /// The actor stopped, or is quiesced for a handoff.
    #[error("the PTY takes no input")]
    Closed,
}

/// What a released actor leaves behind for the process that took the PTY over.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Released {
    /// Output the actor read but never delivered (a read that completed while it quiesced; always empty on Unix,
    /// where a quiesced actor reads nothing): it comes before anything the new owner reads.
    pub pending_output: Vec<u8>,
}

/// What an actor is started with.
pub struct ActorConfig {
    /// The thread names' suffix, for logs and debuggers.
    pub name: String,
    pub master: Master,
    /// Start quiesced, as an actor over a PTY another process still owns does until
    /// [`ActorHandle::rollback_handoff`] makes it the owner.
    pub initially_quiesced: bool,
    /// [`DEFAULT_INPUT_LIMIT`] unless set.
    pub input_limit: usize,
    pub on_read: ReadCallback,
    pub on_exit: Option<ExitCallback>,
}

impl ActorConfig {
    #[must_use]
    pub fn new(name: impl Into<String>, master: Master, on_read: ReadCallback) -> Self {
        Self {
            name: name.into(),
            master,
            initially_quiesced: false,
            input_limit: DEFAULT_INPUT_LIMIT,
            on_read,
            on_exit: None,
        }
    }
}

/// The PTY actor: see [`Actor::spawn`].
#[derive(Debug)]
pub struct Actor;

impl Actor {
    /// Start the actor's threads over `config.master`.
    ///
    /// # Errors
    ///
    /// The master's descriptor flags could not be set (Unix), or a thread or the wake pipe could not be created.
    pub fn spawn(config: ActorConfig) -> io::Result<ActorHandle> {
        sys::spawn_actor(config)
    }
}

/// A running actor. Clones share it; when the last one is dropped the actor shuts down, as
/// [`ActorHandle::shutdown`] does.
#[derive(Clone)]
pub struct ActorHandle {
    pub(crate) owner: Arc<Owner>,
}

impl std::fmt::Debug for ActorHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActorHandle")
            .field("queued", &self.queued())
            .finish_non_exhaustive()
    }
}

/// What every handle shares; dropping the last one shuts the actor down.
pub(crate) struct Owner {
    pub(crate) shared: Arc<Shared>,
    pub(crate) control: sys::Control,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.control.shutdown(&self.shared);
    }
}

impl ActorHandle {
    pub(crate) fn new(shared: Arc<Shared>, control: sys::Control) -> Self {
        Self {
            owner: Arc::new(Owner { shared, control }),
        }
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.owner.shared
    }

    pub(crate) fn control(&self) -> &sys::Control {
        &self.owner.control
    }

    /// Queue `data` for the child. A chunk that does not fit beside what is queued is refused whole; an empty queue
    /// takes any size.
    ///
    /// # Errors
    ///
    /// [`WriteError::Backpressure`] while the queue is full; [`WriteError::Closed`] once the actor stopped, or while
    /// it is quiesced.
    pub fn write_input(&self, data: &[u8]) -> Result<(), WriteError> {
        let shared = self.shared();
        shared.outbox().push_input(data, shared.input_limit)?;
        self.wake();
        Ok(())
    }

    /// Queue one of the terminal's replies for the child, after those of the read in progress. Never refused while
    /// the actor runs; dropped past [`REPLY_CAP`] queued bytes, and once it stopped. Not from inside the read
    /// callback, which returns its replies instead (this waits for the callback to return).
    pub fn write_reply(&self, data: &[u8]) {
        let shared = self.shared();
        let order = lock(&shared.on_read);
        shared.outbox().push_reply(data, shared.input_limit);
        drop(order);
        self.wake();
    }

    /// The bytes queued for the child and not yet written.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.shared().outbox().bytes
    }

    /// Make the child redraw after a handoff, as herdr does: one row smaller (one column, for two rows or fewer)
    /// for 30 ms, then `size` again, so a full-screen program gets the resize it repaints on. Blocks for those 30 ms.
    pub fn nudge_redraw(&self, size: Size) {
        let smaller = if size.rows > 2 {
            Size {
                rows: size.rows - 1,
                ..size
            }
        } else {
            Size {
                cols: size.cols.saturating_sub(1).max(4),
                ..size
            }
        };
        if smaller != size {
            self.resize(smaller);
            std::thread::sleep(std::time::Duration::from_millis(30));
            self.resize(size);
        }
    }

    /// Stop the actor: no more reads or writes, queued bytes dropped, and the PTY closed once its threads let go of it
    /// (on Unix the child's session is hung up; on Windows the pseudoconsole is closed). The exit callback runs with
    /// [`ExitReason::Shutdown`] unless the actor had already stopped.
    pub fn shutdown(&self) {
        self.control().shutdown(self.shared());
    }
}

/// What the handles and the IO threads share.
pub(crate) struct Shared {
    outbox: Mutex<Outbox>,
    /// The outbox changed: Windows' writer waits here for bytes, a handoff for the drain.
    pub(crate) outbox_changed: Condvar,
    /// The read callback. Its lock also orders replies: one queued from another thread lands after those of the
    /// read in progress. Lock order: this one, then the outbox.
    pub(crate) on_read: Mutex<ReadCallback>,
    pub(crate) input_limit: usize,
}

impl Shared {
    pub(crate) fn new(on_read: ReadCallback, input_limit: usize, quiesced: bool) -> Self {
        Self {
            outbox: Mutex::new(Outbox::new(quiesced)),
            outbox_changed: Condvar::new(),
            on_read: Mutex::new(on_read),
            input_limit,
        }
    }

    pub(crate) fn outbox(&self) -> MutexGuard<'_, Outbox> {
        lock(&self.outbox)
    }

    /// Wait on [`Shared::outbox_changed`] with the outbox's guard, at most `timeout`.
    #[cfg(windows)]
    pub(crate) fn wait_outbox<'a>(
        &self,
        outbox: MutexGuard<'a, Outbox>,
        timeout: std::time::Duration,
    ) -> MutexGuard<'a, Outbox> {
        self.outbox_changed
            .wait_timeout(outbox, timeout)
            .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard)
    }

    /// Hand `bytes` to the read callback and queue its replies.
    pub(crate) fn deliver(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut on_read = lock(&self.on_read);
        let replies = on_read(bytes);
        if !replies.is_empty() {
            let mut outbox = self.outbox();
            for reply in &replies {
                outbox.push_reply(reply, self.input_limit);
            }
            drop(outbox);
            self.outbox_changed.notify_all();
        }
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the outbox lets through.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    /// Input is taken, and everything queued is written.
    #[default]
    Open,
    /// Input is refused, what is queued is still written: a handoff's drain, or a stop on its way.
    Draining,
    /// Quiesced: input is refused and nothing is written, though replies still queue.
    Paused,
    /// The actor stopped: nothing is queued or written any more.
    Closed,
}

/// The bytes waiting for the child, input and replies in the order they were queued.
#[derive(Debug, Default)]
pub(crate) struct Outbox {
    chunks: VecDeque<Vec<u8>>,
    /// Bytes of the front chunk a short write already sent.
    offset: usize,
    /// Bytes queued and not yet written (or handed to Windows' writer).
    bytes: usize,
    pub(crate) gate: Gate,
    /// Windows' writer holds a chunk it is writing.
    #[cfg(any(windows, test))]
    pub(crate) busy: bool,
}

impl Outbox {
    fn new(quiesced: bool) -> Self {
        Self {
            gate: if quiesced { Gate::Paused } else { Gate::Open },
            ..Self::default()
        }
    }

    /// Stop taking input, as a handoff or a stop does; a quiesced or closed outbox stays as it is.
    #[cfg(any(unix, test))]
    pub(crate) fn refuse_input(&mut self) {
        if self.gate == Gate::Open {
            self.gate = Gate::Draining;
        }
    }

    fn push_input(&mut self, data: &[u8], limit: usize) -> Result<(), WriteError> {
        if self.gate != Gate::Open {
            return Err(WriteError::Closed);
        }
        if self.bytes > 0 && self.bytes + data.len() > limit {
            return Err(WriteError::Backpressure);
        }
        self.push(data);
        Ok(())
    }

    fn push_reply(&mut self, data: &[u8], limit: usize) {
        if self.gate == Gate::Closed {
            return;
        }
        if self.bytes + data.len() > REPLY_CAP.max(limit) {
            tracing::warn!(
                queued = self.bytes,
                "the child reads no input; a terminal reply is dropped"
            );
            return;
        }
        self.push(data);
    }

    fn push(&mut self, data: &[u8]) {
        if !data.is_empty() {
            self.bytes += data.len();
            self.chunks.push_back(data.to_vec());
        }
    }

    /// Something is queued that may be written now.
    pub(crate) fn writable(&self) -> bool {
        matches!(self.gate, Gate::Open | Gate::Draining) && !self.chunks.is_empty()
    }

    /// Nothing is queued or being written.
    #[cfg(any(windows, test))]
    pub(crate) fn drained(&self) -> bool {
        self.chunks.is_empty() && !self.busy
    }

    /// What is left of the front chunk.
    #[cfg(any(unix, test))]
    pub(crate) fn front(&self) -> Option<&[u8]> {
        self.chunks.front().map(|chunk| &chunk[self.offset..])
    }

    /// `written` bytes of the front chunk went out.
    #[cfg(any(unix, test))]
    pub(crate) fn advance(&mut self, written: usize) {
        let Some(front) = self.chunks.front() else {
            return;
        };
        let written = written.min(front.len() - self.offset);
        self.offset += written;
        self.bytes -= written;
        if self.offset == front.len() {
            self.chunks.pop_front();
            self.offset = 0;
        }
    }

    /// The rest of the front chunk, taken out for a blocking write.
    #[cfg(any(windows, test))]
    pub(crate) fn take_front(&mut self) -> Option<Vec<u8>> {
        let mut chunk = self.chunks.pop_front()?;
        let offset = std::mem::take(&mut self.offset);
        self.bytes -= chunk.len() - offset;
        chunk.drain(..offset);
        Some(chunk)
    }

    /// Drop what is queued.
    pub(crate) fn clear(&mut self) {
        self.chunks.clear();
        self.offset = 0;
        self.bytes = 0;
    }

    /// Stop for good: nothing more is queued or written.
    pub(crate) fn close(&mut self) {
        self.gate = Gate::Closed;
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_over_the_limit_is_refused_but_replies_are_not() {
        let mut outbox = Outbox::new(false);
        outbox.push_input(b"12345", 8).unwrap();
        assert_eq!(outbox.push_input(b"6789", 8), Err(WriteError::Backpressure));
        outbox.push_reply(b"reply-bytes", 8);
        assert_eq!(outbox.bytes, 16);
        assert_eq!(outbox.take_front().as_deref(), Some(&b"12345"[..]));
        assert_eq!(outbox.take_front().as_deref(), Some(&b"reply-bytes"[..]));
        assert_eq!(outbox.bytes, 0);
        // an empty queue takes a chunk of any size
        outbox.push_input(&[0; 64], 8).unwrap();
    }

    #[test]
    fn short_writes_advance_through_the_front_chunk() {
        let mut outbox = Outbox::new(false);
        outbox.push_input(b"hello", 64).unwrap();
        outbox.push_reply(b"!", 64);
        outbox.advance(2);
        assert_eq!(outbox.front(), Some(&b"llo"[..]));
        assert_eq!(outbox.bytes, 4);
        assert_eq!(outbox.take_front().as_deref(), Some(&b"llo"[..]));
        outbox.advance(1);
        assert!(outbox.drained());
        assert_eq!(outbox.bytes, 0);
        // advancing an empty outbox does nothing
        outbox.advance(5);
        assert_eq!(outbox.front(), None);
    }

    #[test]
    fn a_quiesced_outbox_refuses_input_and_writes_nothing_but_queues_replies() {
        let mut outbox = Outbox::new(true);
        assert_eq!(outbox.push_input(b"x", 8), Err(WriteError::Closed));
        outbox.push_reply(b"\x1b[0n", 8);
        assert!(!outbox.writable());
        outbox.gate = Gate::Open;
        assert!(outbox.writable());
        outbox.refuse_input();
        assert_eq!(outbox.push_input(b"x", 8), Err(WriteError::Closed));
        assert!(outbox.writable());
    }

    #[test]
    fn a_closed_outbox_drops_everything() {
        let mut outbox = Outbox::new(false);
        outbox.push_input(b"queued", 64).unwrap();
        outbox.close();
        assert_eq!(outbox.bytes, 0);
        assert_eq!(outbox.push_input(b"x", 64), Err(WriteError::Closed));
        outbox.push_reply(b"ignored", 64);
        assert!(outbox.drained());
    }

    #[test]
    fn replies_past_the_cap_are_dropped() {
        let mut outbox = Outbox::new(false);
        outbox.push_reply(&vec![0; REPLY_CAP], 4);
        outbox.push_reply(b"one too many", 4);
        assert_eq!(outbox.bytes, REPLY_CAP);
    }

    #[test]
    fn empty_writes_queue_nothing() {
        let mut outbox = Outbox::new(false);
        outbox.push_input(b"", 4).unwrap();
        outbox.push_reply(b"", 4);
        assert!(outbox.drained());
        assert!(!outbox.writable());
    }
}
