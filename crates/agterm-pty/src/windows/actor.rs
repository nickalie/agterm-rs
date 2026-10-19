//! The PTY actor on Windows: a reader thread blocks on the pseudoconsole's output pipe and hands each read to the
//! read callback; a writer thread writes the outbox into its input pipe; resizes go straight to the console.
//!
//! herdr: `src/pty/actor.rs` (the `windows` module: `PtyIoActor`'s reader, writer and control threads, the response
//! ordering lock) at `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified: std threads in
//! place of tokio's input task; the outbox of `crate::actor` in place of herdr's write channel; resizes on the
//! caller's thread under the console's lock in place of a control thread; and the handoff herdr's Windows actor does
//! not have: a quiesce parks the reader (its blocked read cancelled with `CancelSynchronousIo`) and waits for the
//! writer to drain, a read that completed meanwhile is held back and delivered on rollback or handed on by the
//! release, and a release leaves the pseudoconsole open for its new owner; a shutdown drains the output pipe only of
//! a console the actor owns, so an actor over a taken-over console that never rolled back reads nothing of it.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, BorrowedHandle, OwnedHandle, RawHandle};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use portable_pty::MasterPty;
use portable_pty::win::conpty::{ConPtyHandoff, ConPtyMasterPty};
use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_OPERATION_ABORTED};
use windows_sys::Win32::System::IO::CancelSynchronousIo;

use super::Master;
use crate::Size;
use crate::actor::{ActorConfig, ActorHandle, ExitCallback, ExitReason, Gate, Released, Shared, lock};

/// The most output one read hands to the callback.
const READ_BUFFER: usize = 256 * 1024;
/// How often a waiting thread looks again, besides being notified.
const WAIT_SLICE: Duration = Duration::from_millis(10);
/// How long the writer sleeps on an empty outbox before it looks again (a fallback: every queued chunk notifies it).
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// What the reader does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Read and deliver.
    Running,
    /// Read nothing: park until rolled back or released.
    Quiesced,
    /// Stop: another process owns the pseudoconsole now.
    Released,
    /// Shut down as the console's owner: read and drop until the console's pipe ends, so its close never waits on a
    /// full pipe.
    Discarding,
    /// Shut down before owning the console (an actor started quiesced that was never rolled back): read nothing, as
    /// the console's pipe still carries the output of the process that owns it.
    Stopped,
}

/// Whether the actor owns its pseudoconsole. A shutdown closes only an owner's console, so only an owner drains its
/// pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Console {
    /// Started running, or started quiesced and rolled back.
    Owned,
    /// Started quiesced over a console another process still owns, and not rolled back yet.
    Borrowed,
}

#[derive(Debug)]
struct ReaderState {
    mode: Mode,
    console: Console,
    /// The reader is inside its `ReadFile`: only then may it be cancelled, so no other I/O of the thread is.
    reading: bool,
    /// The reader waits in its park.
    parked: bool,
    /// The reader thread finished.
    exited: bool,
    /// A read that completed while quiescing, not delivered.
    held: Vec<u8>,
}

/// What the handles and the two threads share beside the outbox.
struct Inner {
    reader: Mutex<ReaderState>,
    reader_changed: Condvar,
    /// The reader thread, whose handle a quiesce cancels its blocked read through.
    reader_thread: Mutex<Option<JoinHandle<()>>>,
    /// The pseudoconsole: resized here, closed on shutdown, left open (leaked) on release.
    conpty: Mutex<Option<Box<ConPtyMasterPty>>>,
    input: File,
    output: File,
}

impl Inner {
    fn reader(&self) -> MutexGuard<'_, ReaderState> {
        lock(&self.reader)
    }

    /// Cancel the reader's blocked `ReadFile`, if it is in one; called with the reader's state locked, so the thread
    /// cannot be anywhere else in between.
    fn cancel_read(&self, state: &ReaderState) {
        if !state.reading {
            return;
        }
        if let Some(thread) = lock(&self.reader_thread).as_ref() {
            // SAFETY: cancels the pending synchronous I/O of a thread whose handle the stored JoinHandle keeps open;
            // "nothing to cancel" is a harmless failure.
            unsafe { CancelSynchronousIo(thread.as_raw_handle()) };
        }
    }

    /// Close the pseudoconsole on a thread of its own (`ClosePseudoConsole` may wait for the console host), so the
    /// reader then reads its last output and the end of its pipe.
    fn close_console(&self) {
        if let Some(conpty) = lock(&self.conpty).take() {
            let closing = std::thread::Builder::new()
                .name("pty-close".to_owned())
                .spawn(move || drop(conpty));
            if let Err(err) = closing {
                tracing::debug!(%err, "no thread to close the pseudoconsole on");
            }
        }
    }
}

/// The handles' side of the actor.
pub(crate) struct Control {
    inner: Arc<Inner>,
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the PTY actor stopped")
}

fn timed_out(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("timed out waiting for the PTY actor to {what}"),
    )
}

impl Control {
    pub(crate) fn shutdown(&self, shared: &Shared) {
        shared.outbox().close();
        shared.outbox_changed.notify_all();
        let mut state = self.inner.reader();
        let next = match state.mode {
            Mode::Running => Some(Mode::Discarding),
            Mode::Quiesced if state.console == Console::Owned => Some(Mode::Discarding),
            // the console host and its pipe stay with the process that owns them: a read from here would take its
            // output
            Mode::Quiesced => Some(Mode::Stopped),
            Mode::Released | Mode::Discarding | Mode::Stopped => None,
        };
        if let Some(mode) = next {
            state.mode = mode;
            self.inner.reader_changed.notify_all();
        }
        drop(state);
        self.inner.close_console();
    }
}

impl ActorHandle {
    fn inner(&self) -> &Inner {
        &self.control().inner
    }

    /// Wake the writer to look at what was just queued.
    pub(crate) fn wake(&self) {
        self.shared().outbox_changed.notify_all();
    }

    /// Resize the pseudoconsole (pixel sizes are not passed on: `ConPTY` has none).
    pub fn resize(&self, size: Size) {
        if let Some(conpty) = lock(&self.inner().conpty).as_ref()
            && let Err(err) = conpty.resize(size.pty())
        {
            tracing::debug!(%err, "resizing the pseudoconsole failed");
        }
    }

    /// The child exited: close the pseudoconsole, which otherwise keeps its output pipe open for good, so the reader
    /// delivers the last output and the actor stops with [`ExitReason::Eof`].
    pub fn child_exited(&self) {
        self.inner().close_console();
    }

    /// Quiesce for a handoff: input is refused from now on, the reader parks (a read it completes meanwhile is held
    /// back), what is queued is written, then nothing is read or written until [`ActorHandle::rollback_handoff`] or
    /// [`ActorHandle::release_after_commit`]. A failure rolls back.
    ///
    /// # Errors
    ///
    /// A handoff is already in progress (`WouldBlock`), the actor stopped, or the reader did not park or the outbox
    /// did not drain within `timeout`.
    pub fn begin_handoff(&self, timeout: Duration) -> io::Result<()> {
        {
            let mut outbox = self.shared().outbox();
            match outbox.gate {
                Gate::Open => outbox.gate = Gate::Draining,
                Gate::Closed => return Err(closed()),
                Gate::Draining | Gate::Paused => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "a PTY handoff is already in progress",
                    ));
                }
            }
        }
        let deadline = Instant::now() + timeout;
        let result = self.park_reader(deadline).and_then(|()| self.drain(deadline));
        if result.is_err() {
            let _ = self.rollback_handoff();
        }
        result
    }

    fn park_reader(&self, deadline: Instant) -> io::Result<()> {
        let inner = self.inner();
        let mut state = inner.reader();
        if state.mode != Mode::Running {
            return Err(closed());
        }
        state.mode = Mode::Quiesced;
        loop {
            if state.exited {
                return Err(closed());
            }
            if state.parked {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(timed_out("park its reader"));
            }
            inner.cancel_read(&state);
            state = inner
                .reader_changed
                .wait_timeout(state, WAIT_SLICE)
                .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard);
        }
    }

    fn drain(&self, deadline: Instant) -> io::Result<()> {
        let shared = self.shared();
        let mut outbox = shared.outbox();
        loop {
            if outbox.gate == Gate::Closed {
                return Err(closed());
            }
            if outbox.drained() {
                outbox.gate = Gate::Paused;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(timed_out("write its queue"));
            }
            outbox = shared.wait_outbox(outbox, WAIT_SLICE);
        }
    }

    /// Duplicates of the handles the pseudoconsole is made of (its pipes' ends and its three console handles), for
    /// the process taking it over; only while quiesced.
    ///
    /// # Safety
    ///
    /// The console handles are read from behind the HPCON, whose layout is undocumented: the caller has checked that
    /// the `ConPTY` implementation this process runs on keeps them there (the session server's support gate).
    ///
    /// # Errors
    ///
    /// The actor is not quiesced or stopped, or a handle could not be duplicated.
    pub unsafe fn duplicate_for_handoff(&self) -> io::Result<ConPtyHandoff> {
        let inner = self.inner();
        if inner.reader().mode != Mode::Quiesced {
            return Err(io::Error::other(
                "the PTY actor must be quiesced before its handoff duplicate",
            ));
        }
        let input = OwnedHandle::from(inner.input.try_clone()?);
        let output = OwnedHandle::from(inner.output.try_clone()?);
        let conpty = lock(&inner.conpty);
        let conpty = conpty.as_ref().ok_or_else(closed)?;
        // SAFETY: the caller's contract: the console handles are where this implementation keeps them.
        let [signal, reference, process] = unsafe { conpty.console_handles() };
        Ok(ConPtyHandoff {
            input,
            output,
            console: [duplicate(signal)?, duplicate(reference)?, duplicate(process)?],
        })
    }

    /// End a quiesce: what the reader held back is delivered first, then it reads again, the outbox is written and
    /// input is taken (also how an actor started quiesced becomes the console's owner).
    ///
    /// # Errors
    ///
    /// The actor stopped or was released.
    pub fn rollback_handoff(&self) -> io::Result<()> {
        let inner = self.inner();
        loop {
            let mut state = inner.reader();
            if state.exited || matches!(state.mode, Mode::Released | Mode::Discarding | Mode::Stopped) {
                return Err(closed());
            }
            if state.held.is_empty() {
                // running again in the step that found nothing held, so no read still in progress (the reader may not
                // have parked) can complete into `held` after it was taken and stay there
                if state.mode == Mode::Quiesced {
                    state.mode = Mode::Running;
                    state.console = Console::Owned;
                }
                inner.reader_changed.notify_all();
                break;
            }
            let held = std::mem::take(&mut state.held);
            drop(state);
            // still quiesced while the held read goes first: a read completing meanwhile is held, taken next round
            self.shared().deliver(&held);
        }
        let mut outbox = self.shared().outbox();
        if outbox.gate != Gate::Closed {
            outbox.gate = Gate::Open;
        }
        drop(outbox);
        self.shared().outbox_changed.notify_all();
        Ok(())
    }

    /// Let go of the pseudoconsole once another process owns it: the actor stops ([`ExitReason::Released`]) without
    /// reading or writing again, what it queued is dropped, and the console is left open (never closed from here).
    /// Only after [`ActorHandle::begin_handoff`]. The output read meanwhile is in [`Released::pending_output`].
    ///
    /// # Errors
    ///
    /// The actor is not quiesced, or stopped.
    pub fn release_after_commit(&self) -> io::Result<Released> {
        let inner = self.inner();
        let mut state = inner.reader();
        if state.mode != Mode::Quiesced {
            return Err(io::Error::other("the PTY actor must be quiesced before its release"));
        }
        state.mode = Mode::Released;
        let pending_output = std::mem::take(&mut state.held);
        inner.reader_changed.notify_all();
        drop(state);
        self.shared().outbox().close();
        self.shared().outbox_changed.notify_all();
        if let Some(conpty) = lock(&inner.conpty).take() {
            // ClosePseudoConsole would end the console host the new owner now holds
            std::mem::forget(conpty);
        }
        Ok(Released { pending_output })
    }
}

fn duplicate(raw: RawHandle) -> io::Result<OwnedHandle> {
    // SAFETY: `raw` is one of the console handles, open as long as the pseudoconsole the caller holds locked.
    unsafe { BorrowedHandle::borrow_raw(raw) }.try_clone_to_owned()
}

pub(crate) fn spawn_actor(config: ActorConfig) -> io::Result<ActorHandle> {
    let ActorConfig {
        name,
        master,
        initially_quiesced,
        input_limit,
        on_read,
        on_exit,
    } = config;
    let Master { conpty, input, output } = master;
    let shared = Arc::new(Shared::new(on_read, input_limit, initially_quiesced));
    let inner = Arc::new(Inner {
        reader: Mutex::new(ReaderState {
            mode: if initially_quiesced {
                Mode::Quiesced
            } else {
                Mode::Running
            },
            console: if initially_quiesced {
                Console::Borrowed
            } else {
                Console::Owned
            },
            reading: false,
            parked: false,
            exited: false,
            held: Vec::new(),
        }),
        reader_changed: Condvar::new(),
        reader_thread: Mutex::new(None),
        conpty: Mutex::new(Some(conpty)),
        input,
        output,
    });
    let control = Control {
        inner: Arc::clone(&inner),
    };
    let writer = {
        let (writer_shared, writer_inner) = (Arc::clone(&shared), Arc::clone(&inner));
        std::thread::Builder::new()
            .name(format!("pty-write-{name}"))
            .spawn(move || write_loop(&writer_shared, &writer_inner.input))
    };
    if let Err(err) = writer {
        control.shutdown(&shared);
        return Err(err);
    }
    let reader = {
        let (reader_shared, reader_inner) = (Arc::clone(&shared), Arc::clone(&inner));
        std::thread::Builder::new()
            .name(format!("pty-read-{name}"))
            .spawn(move || run_reader(&reader_shared, &reader_inner, on_exit))
    };
    match reader {
        Ok(thread) => *lock(&inner.reader_thread) = Some(thread),
        Err(err) => {
            control.shutdown(&shared);
            return Err(err);
        }
    }
    Ok(ActorHandle::new(shared, control))
}

fn run_reader(shared: &Shared, inner: &Inner, on_exit: Option<ExitCallback>) {
    let reason = read_loop(shared, inner);
    let mut state = inner.reader();
    state.exited = true;
    state.reading = false;
    inner.reader_changed.notify_all();
    drop(state);
    shared.outbox().close();
    shared.outbox_changed.notify_all();
    tracing::debug!(?reason, "the PTY actor stops");
    if let Some(on_exit) = on_exit {
        on_exit(reason);
    }
}

/// Read until the pipe ends or the actor is released; returns why it stopped.
fn read_loop(shared: &Shared, inner: &Inner) -> ExitReason {
    let mut buffer = vec![0_u8; READ_BUFFER];
    loop {
        if let Some(reason) = wait_to_read(inner) {
            return reason;
        }
        let result = (&inner.output).read(&mut buffer);
        let mut state = inner.reader();
        state.reading = false;
        match result {
            Ok(0) => return ended(state.mode),
            Ok(read) => match state.mode {
                Mode::Running => {
                    drop(state);
                    shared.deliver(&buffer[..read]);
                }
                Mode::Quiesced | Mode::Released | Mode::Stopped => state.held.extend_from_slice(&buffer[..read]),
                Mode::Discarding => {}
            },
            Err(err) if os_error(&err) == Some(ERROR_OPERATION_ABORTED) => {}
            Err(err) if matches!(os_error(&err), Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA)) => {
                return ended(state.mode);
            }
            Err(err) => {
                tracing::debug!(%err, "reading the pseudoconsole failed; the PTY actor stops");
                return if matches!(state.mode, Mode::Discarding | Mode::Stopped) {
                    ExitReason::Shutdown
                } else {
                    ExitReason::Failed
                };
            }
        }
    }
}

/// Why the reader stopped when the pipe ended in `mode`.
fn ended(mode: Mode) -> ExitReason {
    match mode {
        Mode::Discarding | Mode::Stopped => ExitReason::Shutdown,
        Mode::Released => ExitReason::Released,
        Mode::Running | Mode::Quiesced => ExitReason::Eof,
    }
}

/// Park while quiesced; `Some` once released or stopped, else mark the reader as reading and return.
fn wait_to_read(inner: &Inner) -> Option<ExitReason> {
    let mut state = inner.reader();
    loop {
        match state.mode {
            Mode::Running | Mode::Discarding => {
                state.reading = true;
                return None;
            }
            Mode::Released => return Some(ExitReason::Released),
            Mode::Stopped => return Some(ExitReason::Shutdown),
            Mode::Quiesced => {
                state.parked = true;
                inner.reader_changed.notify_all();
                state = inner
                    .reader_changed
                    .wait_timeout(state, IDLE_WAIT)
                    .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard);
                state.parked = false;
            }
        }
    }
}

fn os_error(err: &io::Error) -> Option<u32> {
    err.raw_os_error().and_then(|code| u32::try_from(code).ok())
}

/// The next chunk for the writer, which holds it (`busy`) until written; `None` once the outbox closed.
fn next_chunk(shared: &Shared) -> Option<Vec<u8>> {
    let mut outbox = shared.outbox();
    loop {
        if outbox.gate == Gate::Closed {
            return None;
        }
        if outbox.writable()
            && let Some(chunk) = outbox.take_front()
        {
            outbox.busy = true;
            return Some(chunk);
        }
        outbox = shared.wait_outbox(outbox, IDLE_WAIT);
    }
}

/// Write the outbox into the console's input pipe, a chunk at a time, until it closes or a write fails.
fn write_loop(shared: &Shared, input: &File) {
    while let Some(chunk) = next_chunk(shared) {
        let mut writer = input;
        let result = writer.write_all(&chunk).and_then(|()| writer.flush());
        shared.outbox().busy = false;
        shared.outbox_changed.notify_all();
        if let Err(err) = result {
            tracing::debug!(%err, "writing the pseudoconsole failed; the PTY takes no more input");
            shared.outbox().close();
            shared.outbox_changed.notify_all();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipe_ending_names_the_reason_of_the_mode() {
        assert_eq!(ended(Mode::Running), ExitReason::Eof);
        assert_eq!(ended(Mode::Quiesced), ExitReason::Eof);
        assert_eq!(ended(Mode::Released), ExitReason::Released);
        assert_eq!(ended(Mode::Discarding), ExitReason::Shutdown);
        assert_eq!(ended(Mode::Stopped), ExitReason::Shutdown);
    }
}
