//! The stream handoff seam: a handler's reply may claim the connection once its response is written, for commands
//! that keep streaming after the first `ok` (`zmx.present`, `session.overlay.job.run`; `events.read` is a plain read
//! that `agtermctl events` polls, as upstream's).
//!
//! [`LineStream`] is what such an owner usually runs on the connection: newline-delimited lines both ways, ported
//! from upstream's `agterm/Control/ControlStreamOwner.swift`.

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, ErrorKind, Read};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use agterm_core::ControlError;
use agterm_proto::Response;

/// Takes over a connection after its `ok` response was written.
pub trait StreamOwner: Send {
    /// Owns `stream` from here on, on the connection's own thread; it may block for the stream's whole life and closes
    /// the stream by dropping it. The stream comes without read or write timeouts: the owner sets its own. `pending`
    /// holds bytes the client sent after the request line that the server already read.
    fn adopt(self: Box<Self>, stream: UnixStream, pending: Vec<u8>);

    /// The connection never reached the owner: the response was a failure or could not be written. The default does
    /// nothing; an owner holding a claim (an overlay job, a presentation) releases it here.
    fn abandon(self: Box<Self>) {}
}

/// A handler's answer: the response, plus an optional owner for the connection afterwards. A deferred reply
/// ([`Reply::deferred`]) carries no answer yet: the connection waits for the one its [`ReplySender`] sends later.
pub struct Reply {
    pub response: Response,
    pub stream: Option<Box<dyn StreamOwner>>,
    deferred: Option<mpsc::Receiver<Reply>>,
}

/// The far end of a [`Reply::deferred`]: sends the connection its answer once the main loop has one (a command that
/// waits for a terminal to come up or for libghostty to report back). Dropping it unsent answers "shutting down".
#[derive(Debug)]
pub struct ReplySender(mpsc::SyncSender<Reply>);

impl ReplySender {
    /// Answers the waiting connection; a connection gone since drops the reply.
    pub fn send(self, reply: impl Into<Reply>) {
        if self.0.send(reply.into()).is_err() {
            tracing::debug!("control connection gone before its deferred reply");
        }
    }
}

impl Reply {
    /// A reply the main loop finishes later through the returned [`ReplySender`]; the connection's thread waits for
    /// it, the main loop does not.
    #[must_use]
    pub fn deferred() -> (Self, ReplySender) {
        let (sender, receiver) = mpsc::sync_channel(1);
        let reply = Self {
            response: Response::from(ControlError::ShuttingDown),
            stream: None,
            deferred: Some(receiver),
        };
        (reply, ReplySender(sender))
    }

    /// Waits out a deferred reply, a deferred one answered with another included; a ready one returns at once.
    #[must_use]
    pub fn settled(mut self) -> Self {
        while let Some(receiver) = self.deferred.take() {
            self = receiver.recv().unwrap_or_else(|_| ControlError::ShuttingDown.into());
        }
        self
    }

    /// A response after which `owner` takes the connection, provided the response is `ok` and was written.
    #[must_use]
    pub fn streaming(response: Response, owner: Box<dyn StreamOwner>) -> Self {
        Self {
            response,
            stream: Some(owner),
            deferred: None,
        }
    }
}

impl From<Response> for Reply {
    fn from(response: Response) -> Self {
        Self {
            response,
            stream: None,
            deferred: None,
        }
    }
}

impl From<ControlError> for Reply {
    fn from(error: ControlError) -> Self {
        Response::from(error).into()
    }
}

impl fmt::Debug for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reply")
            .field("response", &self.response)
            .field("streams", &self.stream.is_some())
            .field("deferred", &self.deferred.is_some())
            .finish()
    }
}

/// A [`StreamOwner`] that runs a [`LineStream`] over the adopted connection and hands each received line and the close
/// to callbacks on the connection's thread: what an app-side owner such as an overlay job's helper connection needs,
/// its callbacks posting to the main loop. A connection that never reached it reports its close all the same, so a
/// claim waiting on the stream is released either way.
pub struct LineStreamOwner {
    stream: LineStream,
    on_line: Box<dyn FnMut(Vec<u8>) + Send>,
    on_close: Box<dyn FnOnce() + Send>,
}

impl LineStreamOwner {
    /// An owner running `stream`, calling `on_line` with each line (newline excluded) and `on_close` once.
    #[must_use]
    pub fn new(
        stream: LineStream,
        on_line: impl FnMut(Vec<u8>) + Send + 'static,
        on_close: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            stream,
            on_line: Box::new(on_line),
            on_close: Box::new(on_close),
        }
    }
}

impl StreamOwner for LineStreamOwner {
    fn adopt(self: Box<Self>, socket: UnixStream, pending: Vec<u8>) {
        let Self {
            stream,
            mut on_line,
            on_close,
        } = *self;
        stream.run(socket, &pending, |line| on_line(line.to_vec()), on_close);
    }

    fn abandon(self: Box<Self>) {
        self.stream.shutdown();
        (self.on_close)();
    }
}

impl fmt::Debug for LineStreamOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LineStreamOwner")
            .field("stream", &self.stream)
            .finish_non_exhaustive()
    }
}

/// The bounds of one [`LineStream`], upstream's `ControlStreamOwner.Limits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineStreamLimits {
    /// The longest line the peer may send, newline excluded; a longer one closes the stream.
    pub max_line_bytes: usize,
    /// How many lines [`LineStream::send`] holds for the writer before it refuses more.
    pub max_pending_lines: usize,
    /// Bounds one blocked write (`SO_SNDTIMEO`): a peer that stops reading is dropped after this, not waited on. A
    /// zero duration cannot be set, and the stream then closes as soon as it runs.
    pub write_timeout: Duration,
}

/// How much one `read` takes off the socket, as upstream's reader.
const READ_CHUNK_BYTES: usize = 16 * 1024;

/// One connection that stays open and carries newline-delimited lines both ways, upstream's `ControlStreamOwner`.
///
/// A cheap, cloneable handle: create it before the connection is adopted, register it wherever the app sends from,
/// and hand a clone to the [`StreamOwner`] that calls [`LineStream::run`] with the adopted socket. Lines sent before
/// that are written once it runs; a [`LineStream::shutdown`] before it closes the connection as soon as it arrives.
///
/// [`LineStream::run`] reads on the calling thread and spawns a writer thread that drains the queue, so the app never
/// waits on the peer. Reads never time out (an idle stream is healthy; liveness is the app's business), writes are
/// bounded by [`LineStreamLimits::write_timeout`], and a write to a peer that hung up fails with `EPIPE` rather than
/// raising `SIGPIPE` (`MSG_NOSIGNAL`, upstream's `SO_NOSIGPIPE`).
///
/// The reader is the only closer. [`LineStream::shutdown`] wakes it with `shutdown(2)` on a duplicate of the socket
/// that it keeps until the reader is done, so where upstream must guard its one descriptor number against reuse, each
/// handle here is its own descriptor and none is closed while another thread may still call into it.
#[derive(Debug, Clone)]
pub struct LineStream {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    limits: LineStreamLimits,
    state: Mutex<State>,
    /// Wakes the writer: a line was queued or the stream is closing.
    wake: Condvar,
}

#[derive(Debug, Default)]
struct State {
    queue: VecDeque<Vec<u8>>,
    closing: bool,
    started: bool,
    /// A duplicate of the socket for `shutdown(2)`, held from the start of [`LineStream::run`] until its reader is
    /// done.
    control: Option<UnixStream>,
}

/// What [`LineStream::run`] found when it attached its socket.
enum Attach {
    Started,
    /// Shut down before it ran, or the socket could not be set up.
    Closed,
    AlreadyRun,
}

impl LineStream {
    #[must_use]
    pub fn new(limits: LineStreamLimits) -> Self {
        Self {
            shared: Arc::new(Shared {
                limits,
                state: Mutex::new(State::default()),
                wake: Condvar::new(),
            }),
        }
    }

    /// Queues one line, without its newline: the stream adds it. False when the stream is closing or the queue is
    /// full, which the caller treats as a stalled peer. Never blocks.
    #[must_use = "a refused line means the peer is gone or stalled"]
    pub fn send(&self, mut line: Vec<u8>) -> bool {
        let mut state = self.shared.lock();
        if state.closing || state.queue.len() >= self.shared.limits.max_pending_lines {
            return false;
        }
        line.push(b'\n');
        state.queue.push_back(line);
        self.shared.wake.notify_one();
        true
    }

    /// Ends the stream from any thread; idempotent. Lines still queued are dropped.
    pub fn shutdown(&self) {
        self.shared.shutdown();
    }

    /// Carries the stream over `socket` until either side ends it, on the calling thread (a [`StreamOwner::adopt`]),
    /// with a writer thread beside it. `pending` (bytes read past the request line) is taken as the first input.
    ///
    /// `on_line` gets each received line, newline excluded, on this thread; `on_close` runs once the socket is closed,
    /// and then this returns. A line over [`LineStreamLimits::max_line_bytes`] closes the stream. A second `run` on the
    /// same stream drops its socket at once and calls neither.
    pub fn run<L, C>(&self, socket: UnixStream, pending: &[u8], mut on_line: L, on_close: C)
    where
        L: FnMut(&[u8]),
        C: FnOnce(),
    {
        match self.shared.attach(&socket) {
            Attach::AlreadyRun => {
                tracing::warn!("control stream already ran; dropping another connection");
                return;
            }
            Attach::Closed => {}
            Attach::Started => self.serve(&socket, pending, &mut on_line),
        }
        self.shutdown();
        drop(socket);
        on_close();
    }

    /// Reads until the stream ends, then stops the writer and lets go of the control duplicate.
    fn serve(&self, socket: &UnixStream, pending: &[u8], on_line: &mut impl FnMut(&[u8])) {
        let writer = self.shared.spawn_writer(socket);
        if writer.is_some() {
            self.shared.read_loop(socket, pending, on_line);
        }
        self.shutdown();
        if let Some(writer) = writer
            && writer.join().is_err()
        {
            tracing::error!("control stream writer panicked");
        }
        let control = self.shared.lock().control.take();
        drop(control);
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_closing(&self) -> bool {
        self.lock().closing
    }

    fn shutdown(&self) {
        let mut state = self.lock();
        if state.closing {
            return;
        }
        state.closing = true;
        state.queue.clear();
        self.wake.notify_all();
        // non-blocking; under the lock so the duplicate cannot be dropped in between
        if let Some(control) = &state.control
            && let Err(error) = control.shutdown(Shutdown::Both)
        {
            tracing::debug!(%error, "control stream: shutdown(2) failed");
        }
    }

    /// Sets stream timing on the socket (reads never time out, writes are bounded) and keeps a duplicate for
    /// [`Shared::shutdown`].
    fn attach(&self, socket: &UnixStream) -> Attach {
        let mut state = self.lock();
        if state.started {
            return Attach::AlreadyRun;
        }
        state.started = true;
        if state.closing {
            return Attach::Closed;
        }
        let prepared = socket
            .set_read_timeout(None)
            .and_then(|()| socket.set_write_timeout(Some(self.limits.write_timeout)))
            .and_then(|()| socket.try_clone());
        match prepared {
            Ok(control) => {
                state.control = Some(control);
                Attach::Started
            }
            Err(error) => {
                tracing::warn!(%error, "control stream not started");
                Attach::Closed
            }
        }
    }

    fn spawn_writer(self: &Arc<Self>, socket: &UnixStream) -> Option<JoinHandle<()>> {
        let spawned = socket.try_clone().and_then(|socket| {
            let shared = Arc::clone(self);
            thread::Builder::new()
                .name("agterm-control-stream-write".to_owned())
                .spawn(move || shared.write_loop(&socket))
        });
        spawned
            .inspect_err(|error| tracing::warn!(%error, "control stream closed: no writer for it"))
            .ok()
    }

    fn read_loop(&self, mut socket: &UnixStream, pending: &[u8], on_line: &mut impl FnMut(&[u8])) {
        let mut lines = LineBuffer::new(self.limits.max_line_bytes);
        let mut chunk = vec![0_u8; READ_CHUNK_BYTES];
        let mut input = pending;
        loop {
            if lines.feed(input, on_line).is_err() {
                tracing::error!(
                    "control stream line exceeds {} bytes; closing",
                    self.limits.max_line_bytes
                );
                return;
            }
            let count = match socket.read(&mut chunk) {
                Ok(0) => return,
                Ok(count) => count,
                Err(error) if error.kind() == ErrorKind::Interrupted => 0,
                Err(error) => {
                    tracing::debug!(%error, "control stream read failed; closing");
                    return;
                }
            };
            if self.is_closing() {
                return;
            }
            input = chunk.get(..count).unwrap_or_default();
        }
    }

    fn write_loop(&self, socket: &UnixStream) {
        while let Some(line) = self.next_line() {
            if let Err(error) = send_all(socket, &line) {
                tracing::info!(%error, "control stream write failed or timed out; closing");
                self.shutdown();
                return;
            }
        }
    }

    /// The next line to write, waiting for one; `None` once the stream is closing.
    fn next_line(&self) -> Option<Vec<u8>> {
        let mut state = self.lock();
        while state.queue.is_empty() && !state.closing {
            state = self.wake.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
        if state.closing { None } else { state.queue.pop_front() }
    }
}

/// Writes all of `data` with `MSG_NOSIGNAL`: std's `UnixStream` writes with `write(2)`, which raises `SIGPIPE` on a
/// peer that hung up unless the process ignores it, where this fails with `EPIPE` whatever the signal disposition.
fn send_all(socket: &UnixStream, mut data: &[u8]) -> io::Result<()> {
    let fd = socket.as_raw_fd();
    while !data.is_empty() {
        let (buf, len) = (data.as_ptr().cast::<libc::c_void>(), data.len());
        // SAFETY: `fd` is the socket's open descriptor, borrowed for the call; `buf` and `len` describe the live slice
        // `data`, which `send` only reads.
        let sent = unsafe { libc::send(fd, buf, len, libc::MSG_NOSIGNAL) };
        match usize::try_from(sent) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(sent) => data = data.get(sent..).unwrap_or_default(),
            Err(_) => {
                let error = io::Error::last_os_error();
                if error.kind() != ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

/// A received line ran past the limit.
struct Oversize;

/// Splits received bytes into lines, holding the unfinished tail.
struct LineBuffer {
    max_line_bytes: usize,
    buffer: Vec<u8>,
}

impl LineBuffer {
    fn new(max_line_bytes: usize) -> Self {
        Self {
            max_line_bytes,
            buffer: Vec::new(),
        }
    }

    /// Appends `bytes` and hands each completed line to `on_line`. Fails on a line, or an unfinished tail, longer
    /// than the limit.
    fn feed(&mut self, bytes: &[u8], on_line: &mut impl FnMut(&[u8])) -> Result<(), Oversize> {
        // the held tail has no newline, so only the new bytes need scanning
        let mut from = self.buffer.len();
        self.buffer.extend_from_slice(bytes);
        let mut start = 0;
        while let Some(at) = self
            .buffer
            .get(from..)
            .and_then(|tail| tail.iter().position(|&byte| byte == b'\n'))
        {
            let end = from + at;
            let line = self.buffer.get(start..end).unwrap_or_default();
            if line.len() > self.max_line_bytes {
                return Err(Oversize);
            }
            on_line(line);
            start = end + 1;
            from = start;
        }
        self.buffer.drain(..start);
        if self.buffer.len() > self.max_line_bytes {
            Err(Oversize)
        } else {
            Ok(())
        }
    }
}
