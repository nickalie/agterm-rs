//! The PTY actor on Unix: one thread polls the non-blocking master and a wake pipe, hands each batch of output to
//! the read callback, writes the outbox as the master takes it, and serves the handoff's requests between rounds.
//!
//! herdr: `src/pty/actor/unix.rs` (`PtyIoActor`, `PtyIoActorHandle`, `PtyIoActorRunner`: the poll loop, the wake
//! after each queued command, the handoff's begin, duplicate, rollback and release, the foreground process group,
//! and the post-handoff redraw nudge) at `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0,
//! modified: std channels in place of tokio's; the outbox of `crate::actor` in place of herdr's pending writes;
//! reads batched up to `READ_BATCH` per callback; a quiesced actor writes nothing either; resizes on the caller's
//! thread in place of herdr's latest-wins request, so a resize reaches the child before later input; input
//! submissions are not ported.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::fd;
use crate::Size;
use crate::actor::{ActorConfig, ActorHandle, ExitCallback, ExitReason, Gate, Released, Shared, lock};

/// The poll timeout with nothing to do: only a fallback for a missed wake, as every queued request wakes the actor.
const IDLE_POLL_MS: i32 = 1000;
/// How long a handoff waits for the outbox to drain into the PTY.
const HANDOFF_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a request waits for the actor's answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
/// The most output gathered for one call of the read callback.
const READ_BATCH: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Running,
    /// Neither reading nor writing: the PTY may be passing to another process.
    Quiesced,
}

/// What the actor serves between poll rounds.
enum Request {
    BeginHandoff(Sender<io::Result<()>>),
    Duplicate(Sender<io::Result<OwnedFd>>),
    ForegroundProcessGroup(Sender<Option<u32>>),
    Rollback(Sender<io::Result<()>>),
    Release(Sender<io::Result<Released>>),
    Shutdown,
}

/// The handles' side of the actor.
pub(crate) struct Control {
    requests: Sender<Request>,
    wake: OwnedFd,
    /// The master, shared with the actor's thread: resizes are made on the caller's thread, so they reach the child
    /// before any input queued after them. Let go of by a shutdown or a release, so the descriptor closes with the
    /// actor's thread.
    file: Mutex<Option<Arc<File>>>,
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the PTY actor stopped")
}

impl Control {
    pub(crate) fn wake(&self) {
        if let Err(err) = fd::wake(self.wake.as_fd()) {
            tracing::debug!(%err, "waking the PTY actor failed");
        }
    }

    pub(crate) fn shutdown(&self, shared: &Shared) {
        shared.outbox().refuse_input();
        lock(&self.file).take();
        if self.requests.send(Request::Shutdown).is_ok() {
            self.wake();
        }
    }

    /// Send the request `make` builds around a reply channel and wait for the answer.
    fn request<T>(&self, make: impl FnOnce(Sender<T>) -> Request, what: &str) -> io::Result<T> {
        let (reply, answer) = mpsc::channel();
        self.requests.send(make(reply)).map_err(|_| closed())?;
        self.wake();
        answer.recv_timeout(REQUEST_TIMEOUT).map_err(|err| match err {
            RecvTimeoutError::Timeout => io::Error::new(
                io::ErrorKind::TimedOut,
                format!("timed out waiting for the PTY actor's {what}"),
            ),
            RecvTimeoutError::Disconnected => closed(),
        })
    }
}

impl ActorHandle {
    /// Wake the actor to look at what was just queued.
    pub(crate) fn wake(&self) {
        self.control().wake();
    }

    /// Resize the PTY (`TIOCSWINSZ`, which signals the child's foreground process group), on this thread: input
    /// queued afterwards reaches the child after the new size. Nothing once the actor stopped.
    pub fn resize(&self, size: Size) {
        if self.shared().outbox().gate == Gate::Closed {
            return;
        }
        if let Some(file) = lock(&self.control().file).as_ref()
            && let Err(err) = fd::resize(file.as_fd(), size)
        {
            tracing::debug!(%err, "resizing the PTY failed");
        }
    }

    /// Make the actor look at its [`crate::ReadPause`] again, after it was turned off.
    pub fn wake_reader(&self) {
        self.wake();
    }

    /// The child exited. Nothing to do on Unix, where the master reports the PTY's end by itself once every process
    /// holding its child side has closed it; on Windows this closes the pseudoconsole, which never ends by itself.
    pub fn child_exited(&self) {}

    /// Quiesce for a handoff: input is refused from now on, what is queued is written (within two seconds), then
    /// the actor neither reads nor writes until [`ActorHandle::rollback_handoff`] or
    /// [`ActorHandle::release_after_commit`]. A failure rolls back.
    ///
    /// # Errors
    ///
    /// A handoff is already in progress (`WouldBlock`), the actor stopped, the outbox did not drain, or the actor
    /// did not answer within `timeout`.
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
        let (reply, answer) = mpsc::channel();
        if self.control().requests.send(Request::BeginHandoff(reply)).is_err() {
            return Err(closed());
        }
        self.wake();
        let result = match answer.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for the PTY actor to quiesce",
            )),
            Err(RecvTimeoutError::Disconnected) => Err(closed()),
        };
        if result.is_err() {
            let _ = self.rollback_handoff();
        }
        result
    }

    /// A close-on-exec duplicate of the master for the process taking the PTY over; only while quiesced.
    ///
    /// # Errors
    ///
    /// The actor is not quiesced or stopped, or the descriptor could not be duplicated.
    pub fn duplicate_for_handoff(&self) -> io::Result<OwnedFd> {
        self.control().request(Request::Duplicate, "handoff duplicate")?
    }

    /// The PTY's foreground process group (`tcgetpgrp`), if it has one.
    #[must_use]
    pub fn foreground_process_group(&self) -> Option<u32> {
        self.control()
            .request(Request::ForegroundProcessGroup, "foreground process group")
            .ok()
            .flatten()
    }

    /// End a quiesce: the actor reads and writes again and takes input (also how an actor started quiesced becomes
    /// the PTY's owner).
    ///
    /// # Errors
    ///
    /// The actor stopped (or was released) or did not answer.
    pub fn rollback_handoff(&self) -> io::Result<()> {
        self.control().request(Request::Rollback, "handoff rollback")?
    }

    /// Let go of the PTY once another process owns it: the actor stops ([`ExitReason::Released`]) without reading or
    /// writing again, and what it queued is dropped. Only after [`ActorHandle::begin_handoff`].
    ///
    /// # Errors
    ///
    /// The actor is not quiesced, had already stopped, or did not answer.
    pub fn release_after_commit(&self) -> io::Result<Released> {
        let released = self.control().request(Request::Release, "release")??;
        lock(&self.control().file).take();
        Ok(released)
    }
}

pub(crate) fn spawn_actor(config: ActorConfig) -> io::Result<ActorHandle> {
    let ActorConfig {
        name,
        master,
        initially_quiesced,
        input_limit,
        read_pause,
        on_read,
        on_exit,
    } = config;
    let fd = OwnedFd::from(master);
    fd::set_cloexec(fd.as_fd())?;
    fd::set_nonblocking(fd.as_fd())?;
    let (wake_read, wake_write) = fd::wake_pipe()?;
    let (requests, received) = mpsc::channel();
    let shared = Arc::new(Shared::new(on_read, input_limit, initially_quiesced, read_pause));
    let file = Arc::new(File::from(fd));
    let runner = Runner {
        file: Arc::clone(&file),
        wake: wake_read,
        requests: received,
        shared: Arc::clone(&shared),
        state: if initially_quiesced {
            State::Quiesced
        } else {
            State::Running
        },
        on_exit,
        buffer: vec![0; READ_BATCH],
    };
    std::thread::Builder::new()
        .name(format!("pty-{name}"))
        .spawn(move || runner.run())?;
    Ok(ActorHandle::new(
        shared,
        Control {
            requests,
            wake: wake_write,
            file: Mutex::new(Some(file)),
        },
    ))
}

/// The actor's thread.
struct Runner {
    file: Arc<File>,
    wake: OwnedFd,
    requests: Receiver<Request>,
    shared: Arc<Shared>,
    state: State,
    on_exit: Option<ExitCallback>,
    buffer: Vec<u8>,
}

impl Runner {
    fn run(mut self) {
        let reason = self.serve();
        self.shared.outbox().close();
        self.shared.outbox_changed.notify_all();
        tracing::debug!(?reason, "the PTY actor stops");
        if let Some(on_exit) = self.on_exit.take() {
            on_exit(reason);
        }
    }

    fn serve(&mut self) -> ExitReason {
        loop {
            if let Some(reason) = self.serve_requests() {
                return reason;
            }
            if let Err(reason) = self.round() {
                return reason;
            }
        }
    }

    /// One poll round: write what the PTY takes, wait, then read and write what became ready. While reads are paused
    /// the master is polled for writing only (and not at all with nothing to write, so its hangup cannot spin the
    /// loop), and a hangup is left for the reads after the pause.
    fn round(&mut self) -> Result<(), ExitReason> {
        let running = self.state == State::Running;
        if running {
            self.flush()?;
        }
        let want_read = running && !self.shared.read_pause.is_paused();
        let want_write = running && self.shared.outbox().writable();
        let ready = fd::poll(
            self.file.as_fd(),
            self.wake.as_fd(),
            want_read,
            want_write,
            IDLE_POLL_MS,
        )
        .map_err(|err| failed("poll", &err))?;
        if ready.wake {
            fd::drain(self.wake.as_fd()).map_err(|err| failed("draining the wake pipe", &err))?;
        }
        if want_read && ready.read {
            self.read()?;
        }
        if want_write && ready.write {
            self.flush()?;
        }
        Ok(())
    }

    /// Serve every queued request; `Some` when one stops the actor.
    fn serve_requests(&mut self) -> Option<ExitReason> {
        loop {
            match self.requests.try_recv() {
                Ok(request) => {
                    if let Some(reason) = self.serve_request(request) {
                        return Some(reason);
                    }
                }
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => return Some(ExitReason::Shutdown),
            }
        }
    }

    fn serve_request(&mut self, request: Request) -> Option<ExitReason> {
        match request {
            Request::BeginHandoff(reply) => {
                let _ = reply.send(self.quiesce());
            }
            Request::Duplicate(reply) => {
                let duplicate = if self.state == State::Quiesced {
                    self.file.as_fd().try_clone_to_owned()
                } else {
                    Err(io::Error::other(
                        "the PTY actor must be quiesced before its handoff duplicate",
                    ))
                };
                let _ = reply.send(duplicate);
            }
            Request::ForegroundProcessGroup(reply) => {
                let _ = reply.send(fd::foreground_process_group(self.file.as_fd()));
            }
            Request::Rollback(reply) => {
                self.state = State::Running;
                self.shared.outbox().gate = Gate::Open;
                let _ = reply.send(Ok(()));
            }
            Request::Release(reply) => {
                if self.state != State::Quiesced {
                    let _ = reply.send(Err(io::Error::other(
                        "the PTY actor must be quiesced before its release",
                    )));
                    return None;
                }
                let _ = reply.send(Ok(Released::default()));
                return Some(ExitReason::Released);
            }
            Request::Shutdown => return Some(ExitReason::Shutdown),
        }
        None
    }

    /// Write what is queued, reading meanwhile so a child blocked on its output can take its input, then stop
    /// reading and writing (herdr's `begin_handoff`).
    fn quiesce(&mut self) -> io::Result<()> {
        if self.state != State::Running {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the PTY actor is already quiesced",
            ));
        }
        let gone = || {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the PTY closed while its writes drained for a handoff",
            )
        };
        let deadline = Instant::now() + HANDOFF_DRAIN_TIMEOUT;
        loop {
            self.flush().map_err(|_| gone())?;
            if !self.shared.outbox().writable() {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out draining PTY writes for a handoff",
                ));
            }
            let timeout = i32::try_from(left.as_millis()).unwrap_or(i32::MAX).max(1);
            let ready = fd::poll(self.file.as_fd(), self.wake.as_fd(), true, true, timeout)?;
            if ready.wake {
                fd::drain(self.wake.as_fd())?;
            }
            if ready.read {
                self.read().map_err(|_| gone())?;
            }
        }
        self.state = State::Quiesced;
        self.shared.outbox().gate = Gate::Paused;
        Ok(())
    }

    /// Read what the PTY has, up to the batch, and hand it to the callback. `Err` when the PTY ended.
    fn read(&mut self) -> Result<(), ExitReason> {
        let mut filled = 0;
        let mut end = None;
        while filled < self.buffer.len() {
            match (&*self.file).read(&mut self.buffer[filled..]) {
                Ok(0) => {
                    end = Some(ExitReason::Eof);
                    break;
                }
                Ok(read) => filled += read,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    end = Some(end_of(&err, "reading the PTY"));
                    break;
                }
            }
        }
        self.shared.deliver(&self.buffer[..filled]);
        end.map_or(Ok(()), Err)
    }

    /// Write the outbox while the PTY takes it. `EIO` (the child side closed) drops what is queued but does not end
    /// the actor: the child's output still in the PTY is read first, and the read reports the end (after a pause of
    /// the reads, too).
    fn flush(&mut self) -> Result<(), ExitReason> {
        let mut outbox = self.shared.outbox();
        while outbox.writable() {
            let Some(front) = outbox.front() else {
                break;
            };
            match (&*self.file).write(front) {
                Ok(0) => return Err(ExitReason::Failed),
                Ok(written) => outbox.advance(written),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) if err.raw_os_error() == Some(libc::EIO) => {
                    tracing::debug!("the PTY's child side closed; its queued input is dropped");
                    outbox.clear();
                    break;
                }
                Err(err) => {
                    outbox.clear();
                    return Err(failed("writing the PTY", &err));
                }
            }
        }
        Ok(())
    }
}

/// `EIO` on a master means the child side closed: the PTY's end, not a failure.
fn end_of(err: &io::Error, what: &str) -> ExitReason {
    if err.raw_os_error() == Some(libc::EIO) {
        ExitReason::Eof
    } else {
        failed(what, err)
    }
}

fn failed(what: &str, err: &io::Error) -> ExitReason {
    tracing::debug!(%err, "{what} failed; the PTY actor stops");
    ExitReason::Failed
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::*;
    use crate::{Actor, ActorConfig, Master, WriteError};

    /// An actor over one end of a socket pair (a stand-in for a master), its reads sent to the returned receiver.
    fn actor(quiesced: bool) -> (ActorHandle, UnixStream, Receiver<Vec<u8>>, Receiver<ExitReason>) {
        actor_with(quiesced, crate::ReadPause::new())
    }

    fn actor_with(
        quiesced: bool,
        read_pause: crate::ReadPause,
    ) -> (ActorHandle, UnixStream, Receiver<Vec<u8>>, Receiver<ExitReason>) {
        let (ours, peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (reads, read) = mpsc::channel();
        let (exits, exit) = mpsc::channel();
        let mut config = ActorConfig::new(
            "test",
            Master::from_fd(OwnedFd::from(ours)),
            Box::new(move |bytes| {
                let _ = reads.send(bytes.to_vec());
                if bytes == b"query" {
                    vec![b"reply".to_vec()]
                } else {
                    Vec::new()
                }
            }),
        );
        config.initially_quiesced = quiesced;
        config.read_pause = read_pause;
        config.on_exit = Some(Box::new(move |reason| {
            let _ = exits.send(reason);
        }));
        (Actor::spawn(config).unwrap(), peer, read, exit)
    }

    fn read_exactly(peer: &mut UnixStream, len: usize) -> Vec<u8> {
        let mut buffer = vec![0; len];
        peer.read_exact(&mut buffer).unwrap();
        buffer
    }

    #[test]
    fn input_reaches_the_peer_and_output_the_callback_with_its_reply_written_back() {
        let (handle, mut peer, read, _exit) = actor(false);
        handle.write_input(b"hello").unwrap();
        assert_eq!(read_exactly(&mut peer, 5), b"hello");
        peer.write_all(b"query").unwrap();
        assert_eq!(read.recv_timeout(Duration::from_secs(5)).unwrap(), b"query");
        assert_eq!(read_exactly(&mut peer, 5), b"reply");
        handle.write_reply(b"late");
        assert_eq!(read_exactly(&mut peer, 4), b"late");
    }

    #[test]
    fn paused_reads_leave_the_output_waiting_while_input_still_goes_out() {
        let pause = crate::ReadPause::new();
        pause.pause();
        let (handle, mut peer, read, exit) = actor_with(false, pause.clone());
        peer.write_all(b"output").unwrap();
        handle.write_input(b"input").unwrap();
        assert_eq!(read_exactly(&mut peer, 5), b"input");
        assert!(
            read.recv_timeout(Duration::from_millis(200)).is_err(),
            "read while paused"
        );
        // the peer hanging up while paused is left for the reads after the pause: no spin, no early end
        drop(peer);
        assert!(
            exit.recv_timeout(Duration::from_millis(200)).is_err(),
            "ended while paused"
        );
        pause.resume();
        handle.wake_reader();
        assert_eq!(read.recv_timeout(Duration::from_secs(5)).unwrap(), b"output");
        assert_eq!(exit.recv_timeout(Duration::from_secs(5)).unwrap(), ExitReason::Eof);
    }

    #[test]
    fn the_peer_closing_ends_the_actor_with_eof() {
        let (handle, peer, _read, exit) = actor(false);
        drop(peer);
        assert_eq!(exit.recv_timeout(Duration::from_secs(5)).unwrap(), ExitReason::Eof);
        assert_eq!(handle.write_input(b"x"), Err(WriteError::Closed));
    }

    #[test]
    fn dropping_the_last_handle_shuts_the_actor_down() {
        let (handle, _peer, _read, exit) = actor(false);
        let clone = handle.clone();
        drop(handle);
        assert!(exit.recv_timeout(Duration::from_millis(200)).is_err());
        drop(clone);
        assert_eq!(exit.recv_timeout(Duration::from_secs(5)).unwrap(), ExitReason::Shutdown);
    }

    #[test]
    fn a_quiesced_actor_neither_reads_nor_writes_until_rolled_back() {
        let (handle, mut peer, read, _exit) = actor(false);
        handle.begin_handoff(Duration::from_secs(5)).unwrap();
        assert_eq!(handle.write_input(b"x"), Err(WriteError::Closed));
        assert_eq!(
            handle.begin_handoff(Duration::from_secs(5)).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        handle.write_reply(b"held");
        peer.write_all(b"while-quiesced").unwrap();
        assert!(read.recv_timeout(Duration::from_millis(300)).is_err());
        handle.rollback_handoff().unwrap();
        assert_eq!(read.recv_timeout(Duration::from_secs(5)).unwrap(), b"while-quiesced");
        assert_eq!(read_exactly(&mut peer, 4), b"held");
        handle.write_input(b"again").unwrap();
        assert_eq!(read_exactly(&mut peer, 5), b"again");
    }

    #[test]
    fn begin_handoff_writes_what_was_queued_first() {
        let (handle, mut peer, _read, _exit) = actor(false);
        handle.write_input(b"before").unwrap();
        handle.begin_handoff(Duration::from_secs(5)).unwrap();
        assert_eq!(read_exactly(&mut peer, 6), b"before");
    }

    #[test]
    fn the_duplicate_needs_a_quiesced_actor_and_reaches_the_same_peer() {
        let (handle, mut peer, _read, _exit) = actor(false);
        assert!(handle.duplicate_for_handoff().is_err());
        handle.begin_handoff(Duration::from_secs(5)).unwrap();
        let duplicate = File::from(handle.duplicate_for_handoff().unwrap());
        (&duplicate).write_all(b"dup").unwrap();
        assert_eq!(read_exactly(&mut peer, 3), b"dup");
    }

    #[test]
    fn a_released_actor_stops_and_does_no_more_io() {
        let (handle, mut peer, read, exit) = actor(false);
        handle.begin_handoff(Duration::from_secs(5)).unwrap();
        handle.write_reply(b"dropped");
        assert_eq!(handle.release_after_commit().unwrap(), Released::default());
        assert_eq!(exit.recv_timeout(Duration::from_secs(5)).unwrap(), ExitReason::Released);
        assert!(read.recv_timeout(Duration::from_millis(200)).is_err());
        let mut buffer = [0; 8];
        // the actor's end closed with the actor: EOF, never the dropped reply
        assert_eq!(peer.read(&mut buffer).unwrap(), 0);
        assert!(handle.rollback_handoff().is_err());
    }

    #[test]
    fn an_actor_started_quiesced_owns_the_pty_once_rolled_back() {
        let (handle, mut peer, read, _exit) = actor(true);
        assert_eq!(handle.write_input(b"x"), Err(WriteError::Closed));
        peer.write_all(b"queued").unwrap();
        assert!(read.recv_timeout(Duration::from_millis(300)).is_err());
        handle.rollback_handoff().unwrap();
        assert_eq!(read.recv_timeout(Duration::from_secs(5)).unwrap(), b"queued");
    }

    #[test]
    fn input_waits_while_the_peer_does_not_read_and_output_still_arrives() {
        let (handle, mut peer, read, _exit) = actor(false);
        // fill the socket's buffer and the outbox
        let chunk = vec![b'x'; 64 * 1024];
        let mut sent = 0;
        while sent < 64 * 1024 * 1024 {
            match handle.write_input(&chunk) {
                Ok(()) => sent += chunk.len(),
                Err(WriteError::Backpressure) => break,
                Err(err) => panic!("{err}"),
            }
        }
        assert!(handle.queued() > 0);
        peer.write_all(b"out").unwrap();
        assert_eq!(read.recv_timeout(Duration::from_secs(5)).unwrap(), b"out");
    }
}
