//! The session server's client library: the api a `SessionHost` and `ngtermctl` call, and a session's attach stream,
//! synchronous, for a caller's own threads (`agterm-term`'s IO thread, the `agterm-session` CLI).
//!
//! herdr: `src/client/handshake.rs` (connect, hello, the welcome checked before anything else) and `src/client/mod.rs`
//! (a connection's reader and writer halves) at `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0,
//! modified: agterm's api calls by id and its raw attach stream ([`crate::protocol`]) in place of herdr's rendered
//! frames, threads in place of tokio, and the stream's contract checked on the client's side (epochs, contiguous
//! offsets, the tokens a delegated request and a Kitty barrier are answered with).
//!
//! # Connecting
//!
//! A [`Client`] names a namespace (its directory, [`Endpoint`]). The api never starts a server: one that does not run
//! holds no session, so [`Client::api`] and [`Client::call`] answer `None` for it. An attach starts one when the client
//! was given [`StartOptions`] ([`Client::starting`]), as the attach that creates a pane's session must
//! ([`spawn::connect_or_start`]); without them it reports [`ClientError::NoServer`].
//!
//! Every api call runs within the client's timeout ([`DEFAULT_TIMEOUT`]), connecting included, the whole call
//! together however its reply arrives; an attach's first answer within [`ATTACH_TIMEOUT`]. A call that outran its
//! timeout drops its connection.
//!
//! # The attach stream
//!
//! [`Client::attach`] sends the attach request and reads the server's first answer: a refusal is
//! [`ClientError::Refused`], anything else the stream's start. The stream then splits in two halves for two threads:
//!
//! - [`AttachReader::recv`] reads the server's messages as [`AttachEvent`]s in order and checks the stream's contract:
//!   each snapshot opens a later epoch (a resync's leads with its own reset, so a client feeds every snapshot and output
//!   byte to its terminal as it comes); output and size changes come only between snapshots, contiguous from the last
//!   snapshot's offset; a delegated request or a Kitty barrier of a superseded epoch is dropped; nothing follows the
//!   exit. A broken contract is [`ClientError::Protocol`]. The stream ends (`Ok(None)`) when the connection closes
//!   in order after the exit or a shutdown (a frame cut or malformed after them is still an error), or once this
//!   client detached or closed it; a connection that ends otherwise is [`ClientError::Disconnected`].
//! - [`AttachWriter`] (cloneable, its writes serialized) sends input, the client's grid, its reply metadata, and the
//!   answers: [`AttachWriter::answer`] takes a delegated request's [`Delegated`] token, [`AttachWriter::complete`] a
//!   [`Barrier`], each once. A Kitty barrier is completed once the client's terminal has *processed* every output byte
//!   [`AttachReader::recv`] returned before the barrier (a caller that feeds each event before it reads the next one
//!   has), with what its terminal answered to the Kitty graphics commands since the previous barrier — and completed
//!   even when that is nothing, since the server holds its own later replies behind it until then. An answer whose
//!   epoch is no longer the stream's current one is not sent: the server would drop it. Its writes take no timeout:
//!   against a server that stopped reading (stopped or deadlocked; a live one queues input and never blocks on it) a
//!   write waits, and holds the writes after it back. [`AttachWriter::detach`] waits for a write in progress for at
//!   most [`DETACH_WAIT`] and closes the connection then; [`AttachWriter::close`] is the call that always returns.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread;
use std::time::{Duration, Instant};
use std::{fmt, io};

use crate::endpoint::{self, Endpoint, Stream, is_connection_closed_error};
use crate::protocol::{
    ApiCall, ApiError, ApiReply, ApiRequest, ApiResponse, AttachClientMessage, AttachRefusal, AttachRequest,
    AttachServerMessage, DelegatedReply, DelegatedRequest, ErrorCode, ExitStatus, FramingError, GridSize, Hello,
    JobProcess, KillResult, LeadRole, PROTOCOL_VERSION, ReplyMetadata, Role, Screen, SessionRecord, Welcome,
    read_frame, write_message,
};
use crate::server::BUILD_ID;
use crate::server::spawn::{self, Connected, HandoffAttempt, StartError, StartOptions};

/// How long an api call may take, connecting included (zmx's `DEFAULT_TIMEOUT`, which bounded one invocation).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// How long an attach waits for the server's first answer once connected: a session is created and its first
/// snapshot cut by then (the server waits for a cut for at most 2 s, `server::pane::SNAPSHOT_WAIT`).
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(15);

/// The least an api call waits, however close its deadline (a zero wait would mean no timeout to a Unix socket).
const MIN_WAIT: Duration = Duration::from_millis(10);

/// How long [`AttachWriter::detach`] waits for a write in progress before it closes the connection instead: a live
/// server reads what a client writes at once, so a write held longer is one a stuck server no longer reads.
pub const DETACH_WAIT: Duration = Duration::from_secs(1);

/// How often [`AttachWriter::detach`] looks whether the write in progress is done.
const DETACH_POLL: Duration = Duration::from_millis(5);

/// Why a client call failed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No server runs in the namespace, and the call does not start one.
    #[error("no live session server runs at {0}")]
    NoServer(String),
    /// Connecting failed, or a server could not be started or reached.
    #[error(transparent)]
    Start(#[from] StartError),
    /// The server refused the request, with its reason.
    #[error("{}", .0.message)]
    Api(ApiError),
    /// The server answered a request with a reply of another kind or id.
    #[error("the live session server answered {request} with {got}")]
    Unexpected { request: &'static str, got: String },
    /// The server refused the attach.
    #[error("the live session server refused the attach ({reason:?}): {message}")]
    Refused { reason: AttachRefusal, message: String },
    /// The server broke the attach stream's contract (module docs).
    #[error("the live session server broke the attach stream: {0}")]
    Protocol(String),
    /// The connection ended before the stream did.
    #[error("the connection to the live session server ended")]
    Disconnected,
    /// Reading or writing the connection failed, or it outran its timeout.
    #[error("the connection to the live session server failed: {0}")]
    Framing(#[from] FramingError),
}

/// A client of one namespace's server.
#[derive(Debug, Clone)]
pub struct Client {
    endpoint: Endpoint,
    build: String,
    timeout: Duration,
    start: Option<StartOptions>,
}

impl Client {
    /// A client of the server of the namespace `directory`, with [`DEFAULT_TIMEOUT`], that starts none.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            endpoint: Endpoint::new(directory),
            build: BUILD_ID.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            start: None,
        }
    }

    /// This client with every api call bounded by `timeout`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// This client with its hellos naming the build `build`.
    #[must_use]
    pub fn with_build(mut self, build: impl Into<String>) -> Self {
        self.build = build.into();
        self
    }

    /// This client starting a server with `options` when an attach finds none.
    #[must_use]
    pub fn starting(mut self, options: StartOptions) -> Self {
        self.start = Some(options);
        self
    }

    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    fn hello(&self, role: Role) -> Hello {
        Hello {
            role,
            build: self.build.clone(),
            capabilities: Vec::new(),
        }
    }

    fn connect_existing(&self, role: Role, timeout: Duration) -> Result<Option<Connected>, ClientError> {
        Ok(spawn::connect_existing(&self.endpoint, &self.hello(role), timeout)?)
    }

    /// An api connection to the server for any number of calls, each within the client's timeout; `None` when no
    /// server runs.
    ///
    /// # Errors
    ///
    /// Connecting failed ([`ClientError::Start`]).
    pub fn api(&self) -> Result<Option<Api>, ClientError> {
        Ok(self.connect_existing(Role::Api, self.timeout)?.map(|connected| Api {
            welcome: connected.welcome,
            stream: Some(connected.stream),
            next: 1,
            timeout: self.timeout,
        }))
    }

    /// `request` on a connection of its own, the connect and the call within the client's timeout together; `None`
    /// when no server runs.
    ///
    /// # Errors
    ///
    /// Connecting or the call failed; [`ClientError::Api`] for the server's refusal of the request.
    pub fn call(&self, request: ApiRequest) -> Result<Option<ApiResponse>, ClientError> {
        let deadline = Instant::now() + self.timeout;
        let Some(mut api) = self.api()? else {
            return Ok(None);
        };
        let left = deadline.saturating_duration_since(Instant::now()).max(MIN_WAIT);
        api.call_within(request, left).map(Some)
    }

    /// An update: asks the namespace's running server, when it is of another build than `options` expects, to hand
    /// its sessions over to `options.executable` ([`spawn::connect_or_start`]'s handoff), as the app launching a new
    /// build does before its first pane. It never starts a server where none runs. `None` when no server runs, it is
    /// of the expected build, or `options` expects none (an unstaged executable); else how the handoff went
    /// ([`HandoffAttempt::Failed`] leaves the sessions with the old server, predating the build).
    ///
    /// # Errors
    ///
    /// Connecting failed, or the server refused the protocol and could not hand over ([`StartError::HandoffFailed`]).
    pub fn update(&self, options: &StartOptions) -> Result<Option<HandoffAttempt>, ClientError> {
        let Some(build) = options.build.as_deref() else {
            return Ok(None);
        };
        match self.connect_existing(Role::Api, self.timeout) {
            Ok(None) => return Ok(None),
            Ok(Some(connected)) if connected.welcome.build == build => return Ok(None),
            // the old server disconnects its clients for the handoff anyway
            Ok(Some(connected)) => drop(connected),
            Err(ClientError::Start(StartError::Refused { error, welcome, .. }))
                if error.code == ErrorCode::ProtocolMismatch && welcome.build != build => {}
            Err(error) => return Err(error),
        }
        let connected = spawn::connect_or_start(&self.endpoint, &self.hello(Role::Api), options)?;
        Ok(connected.handoff)
    }

    /// Attaches to a session as `request` says, creating it when the request carries a spec, starting the server
    /// when the client was given [`StartOptions`].
    ///
    /// # Errors
    ///
    /// [`ClientError::NoServer`] without a server to attach to; [`ClientError::Refused`] for the server's refusal
    /// (gone, the create failed, no snapshot in time); a failed connect or exchange.
    pub fn attach(&self, request: AttachRequest) -> Result<Attachment, ClientError> {
        let hello = self.hello(Role::Attach);
        let connected = match &self.start {
            Some(options) => spawn::connect_or_start(&self.endpoint, &hello, options)?,
            None => self
                .connect_existing(Role::Attach, ATTACH_TIMEOUT)?
                .ok_or_else(|| ClientError::NoServer(self.endpoint.to_string()))?,
        };
        let (stream, first) = spawn::exchange_within(connected.stream, ATTACH_TIMEOUT, move |stream| {
            write_message(stream, &AttachClientMessage::Attach(request))?;
            read_frame(stream)?.message::<AttachServerMessage>(PROTOCOL_VERSION)
        })?;
        if let AttachServerMessage::Refused { reason, message } = first {
            return Err(ClientError::Refused { reason, message });
        }
        Ok(Attachment::new(connected.welcome, stream, first))
    }
}

/// An api connection: calls answered in order, each by id.
#[derive(Debug)]
pub struct Api {
    /// The server's welcome.
    pub welcome: Welcome,
    /// `None` once a call broke the connection.
    stream: Option<Stream>,
    next: u64,
    timeout: Duration,
}

impl Api {
    /// The server's answer to `request` within the client's timeout.
    ///
    /// # Errors
    ///
    /// As [`Api::call_within`].
    pub fn call(&mut self, request: ApiRequest) -> Result<ApiResponse, ClientError> {
        self.call_within(request, self.timeout)
    }

    /// The server's answer to `request` within `timeout`.
    ///
    /// # Errors
    ///
    /// [`ClientError::Api`] for the server's refusal; [`ClientError::Unexpected`] for a reply to another call; a
    /// failed or timed-out exchange, after which the connection is gone ([`ClientError::Disconnected`] for every later
    /// call).
    pub fn call_within(&mut self, request: ApiRequest, timeout: Duration) -> Result<ApiResponse, ClientError> {
        let stream = self.stream.take().ok_or(ClientError::Disconnected)?;
        let id = self.next;
        self.next += 1;
        let (stream, reply) = spawn::exchange_within(stream, timeout.max(MIN_WAIT), move |stream| {
            write_message(stream, &ApiCall { id, request })?;
            read_frame(stream)?.message::<ApiReply>(PROTOCOL_VERSION)
        })?;
        if reply.id != id {
            return Err(ClientError::Unexpected {
                request: "a call",
                got: format!("the reply to call {} in place of call {id}", reply.id),
            });
        }
        self.stream = Some(stream);
        reply.result.map_err(ClientError::Api)
    }

    /// Every session of the namespace.
    ///
    /// # Errors
    ///
    /// As [`Api::call`].
    pub fn list(&mut self) -> Result<Vec<SessionRecord>, ClientError> {
        match self.call(ApiRequest::List)? {
            ApiResponse::Sessions(sessions) => Ok(sessions),
            other => Err(unexpected("a list", &other)),
        }
    }

    /// Kills each of `names`, one result per name in order.
    ///
    /// # Errors
    ///
    /// As [`Api::call`].
    pub fn kill(&mut self, names: &[String], force: bool) -> Result<Vec<KillResult>, ClientError> {
        let names = names.to_vec();
        match self.call(ApiRequest::Kill { names, force })? {
            ApiResponse::Killed(results) => Ok(results),
            other => Err(unexpected("a kill", &other)),
        }
    }

    /// The session's screen, with scrollback when `all`.
    ///
    /// # Errors
    ///
    /// As [`Api::call`].
    pub fn screen(&mut self, name: &str, all: bool) -> Result<Screen, ClientError> {
        let name = name.to_owned();
        match self.call(ApiRequest::Screen { name, all })? {
            ApiResponse::Screen(screen) => Ok(screen),
            other => Err(unexpected("a screen read", &other)),
        }
    }

    /// Queues `data` as typed input to the session, without taking the lead.
    ///
    /// # Errors
    ///
    /// As [`Api::call`].
    pub fn type_input(&mut self, name: &str, data: &[u8]) -> Result<(), ClientError> {
        let (name, data) = (name.to_owned(), data.to_vec());
        done(self.call(ApiRequest::Type { name, data })?, "typing")
    }

    /// Writes `data` into the session's output as if its program printed it.
    ///
    /// # Errors
    ///
    /// As [`Api::call`].
    pub fn print(&mut self, name: &str, data: &[u8]) -> Result<(), ClientError> {
        let (name, data) = (name.to_owned(), data.to_vec());
        done(self.call(ApiRequest::Print { name, data })?, "a print")
    }

    /// The foreground job of the terminal the shell `shell` leads; `None` when the server cannot read its process
    /// table.
    ///
    /// # Errors
    ///
    /// As [`Api::call`].
    pub fn foreground_job(&mut self, shell: i32) -> Result<Option<Vec<JobProcess>>, ClientError> {
        match self.call(ApiRequest::ForegroundJob { shell })? {
            ApiResponse::ForegroundJob(job) => Ok(job),
            other => Err(unexpected("a foreground job read", &other)),
        }
    }
}

fn done(response: ApiResponse, request: &'static str) -> Result<(), ClientError> {
    match response {
        ApiResponse::Done => Ok(()),
        other => Err(unexpected(request, &other)),
    }
}

/// The error for a reply of another kind than `request`'s.
pub(crate) fn unexpected(request: &'static str, got: &ApiResponse) -> ClientError {
    ClientError::Unexpected {
        request,
        got: format!("{got:?}"),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The attach stream
// ---------------------------------------------------------------------------------------------------------------------

/// A delegated request to answer, once, with [`AttachWriter::answer`].
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a delegated request waits for its answer until the server's deadline"]
pub struct Delegated {
    pub id: u64,
    /// The epoch it was sent in.
    pub epoch: u64,
    pub request: DelegatedRequest,
}

/// A Kitty graphics barrier to complete, once, with [`AttachWriter::complete`] (module docs).
#[derive(Debug, PartialEq, Eq)]
#[must_use = "the server holds its later replies behind an uncompleted barrier until its deadline"]
pub struct Barrier {
    pub id: u64,
    /// The epoch it was sent in.
    pub epoch: u64,
}

/// What the server sent, in order, its contract checked (module docs).
#[derive(Debug, PartialEq, Eq)]
pub enum AttachEvent {
    /// A snapshot opens the epoch `epoch`: its chunks follow, then [`AttachEvent::SnapshotEnd`], then the output from
    /// `offset` on. The session's grid is `size`. A resync's snapshot starts with its own reset of the client's
    /// terminal.
    SnapshotBegin { epoch: u64, offset: u64, size: GridSize },
    /// Bytes of the open snapshot, for the client's terminal as output.
    SnapshotChunk(Vec<u8>),
    /// The snapshot is complete.
    SnapshotEnd,
    /// The session's output from `offset` on.
    Output { offset: u64, data: Vec<u8> },
    /// The session's grid became `size`, in the output at `offset`.
    Resized { offset: u64, size: GridSize },
    /// The client's role changed (a managed client's).
    Role { role: LeadRole, generation: u32 },
    /// Whether the client owns the session's replies from here on.
    ReplyOwner { owner: bool, generation: u32 },
    /// The output before ended with a Kitty graphics command the client's terminal answers.
    KittyBarrier(Barrier),
    /// A request only the reply owner answers.
    Delegated(Delegated),
    /// The session's program ended; the stream ends.
    Exit(ExitStatus),
    /// The server is shutting down; the stream ends.
    Shutdown(Option<String>),
}

/// What the halves of one attach stream share.
#[derive(Debug)]
struct Shared {
    stream: Stream,
    /// Serializes the writers, so a message's frames are never interleaved with another's.
    write: Mutex<()>,
    /// The stream's current epoch, as the reader last read it.
    epoch: AtomicU64,
    /// The client detached or closed the connection: its end is the stream's.
    leaving: AtomicBool,
    /// The client closed the connection ([`AttachWriter::close`]): nothing is read or written on it any more.
    closed: AtomicBool,
}

/// An attached session's stream: the server's welcome and the stream's two halves.
#[derive(Debug)]
pub struct Attachment {
    pub welcome: Welcome,
    pub reader: AttachReader,
    pub writer: AttachWriter,
}

impl Attachment {
    fn new(welcome: Welcome, stream: Stream, first: AttachServerMessage) -> Self {
        let shared = Arc::new(Shared {
            stream,
            write: Mutex::new(()),
            epoch: AtomicU64::new(0),
            leaving: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        });
        Self {
            welcome,
            reader: AttachReader {
                shared: Arc::clone(&shared),
                state: StreamState::default(),
                pending: Some(first),
                finished: false,
            },
            writer: AttachWriter { shared },
        }
    }
}

/// The reading half of an attach stream.
#[derive(Debug)]
pub struct AttachReader {
    shared: Arc<Shared>,
    state: StreamState,
    /// The first message, read by the attach.
    pending: Option<AttachServerMessage>,
    /// The stream ended.
    finished: bool,
}

impl AttachReader {
    /// The next event, waiting for it; `None` once the stream ended (module docs).
    ///
    /// # Errors
    ///
    /// [`ClientError::Protocol`] for a broken contract, [`ClientError::Disconnected`] for a connection that ended
    /// before the stream did, [`ClientError::Framing`] for a failed read; the stream is over after any of them.
    pub fn recv(&mut self) -> Result<Option<AttachEvent>, ClientError> {
        while !self.finished {
            if self.shared.closed.load(Ordering::SeqCst) {
                // the client closed the connection: a read would not end where the close cannot wake it (Windows)
                self.finished = true;
                break;
            }
            let message = match self.pending.take() {
                Some(message) => message,
                None => match self.read() {
                    Ok(message) => message,
                    Err(err) => return self.ended(err),
                },
            };
            match self.state.accept(message) {
                Ok(Some(event)) => {
                    self.shared.epoch.store(self.state.epoch, Ordering::SeqCst);
                    return Ok(Some(event));
                }
                Ok(None) => {}
                Err(err) => {
                    self.finished = true;
                    return Err(err);
                }
            }
        }
        Ok(None)
    }

    /// The epoch of the last snapshot read; zero before the first.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.state.epoch
    }

    fn read(&self) -> Result<AttachServerMessage, FramingError> {
        read_frame(&mut &self.shared.stream)?.message(PROTOCOL_VERSION)
    }

    /// The stream's end after the read failed with `err`: in order after the client's leaving, however the read
    /// failed, and after the exit or a shutdown when the connection closed between frames; else an error.
    fn ended(&mut self, err: FramingError) -> Result<Option<AttachEvent>, ClientError> {
        self.finished = true;
        if self.shared.leaving.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let closed = match &err {
            FramingError::Closed => true,
            FramingError::Io(err) => is_connection_closed_error(err),
            _ => false,
        };
        let cut = matches!(err, FramingError::UnexpectedEof);
        if self.state.ended && closed {
            Ok(None)
        } else if !self.state.ended && (closed || cut) {
            Err(ClientError::Disconnected)
        } else {
            // a frame cut or malformed, after the end too
            Err(ClientError::Framing(err))
        }
    }
}

/// The writing half of an attach stream; its clones share the connection. Its writes take no timeout (module docs):
/// against a server that stopped reading they wait, [`AttachWriter::detach`] for at most [`DETACH_WAIT`], and
/// [`AttachWriter::close`] always returns.
#[derive(Debug, Clone)]
pub struct AttachWriter {
    shared: Arc<Shared>,
}

impl AttachWriter {
    fn send(&self, message: &AttachClientMessage) -> Result<(), ClientError> {
        let serialized = self.shared.write.lock().unwrap_or_else(PoisonError::into_inner);
        self.write(&serialized, message)
    }

    /// Writes `message`, the writers' lock held (`_serialized`); refused once the client closed the connection.
    fn write(&self, _serialized: &MutexGuard<'_, ()>, message: &AttachClientMessage) -> Result<(), ClientError> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(ClientError::Disconnected);
        }
        write_message(&mut &self.shared.stream, message)?;
        Ok(())
    }

    /// The writers' lock, once the write holding it is done; `None` when it is still held after `wait`.
    fn lock_within(&self, wait: Duration) -> Option<MutexGuard<'_, ()>> {
        let deadline = Instant::now() + wait;
        loop {
            match self.shared.write.try_lock() {
                Ok(serialized) => return Some(serialized),
                Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) if Instant::now() >= deadline => return None,
                Err(TryLockError::WouldBlock) => thread::sleep(DETACH_POLL),
            }
        }
    }

    /// Input for the session's program.
    ///
    /// # Errors
    ///
    /// The write failed.
    pub fn input(&self, data: &[u8]) -> Result<(), ClientError> {
        self.send(&AttachClientMessage::Input { data: data.to_vec() })
    }

    /// The client's grid; the session takes it while the client leads.
    ///
    /// # Errors
    ///
    /// The write failed.
    pub fn resize(&self, size: GridSize) -> Result<(), ClientError> {
        self.send(&AttachClientMessage::Resize { size })
    }

    /// The client's facts for the replies the server gives on its behalf, replacing the earlier ones whole.
    ///
    /// # Errors
    ///
    /// The write failed.
    pub fn metadata(&self, metadata: ReplyMetadata) -> Result<(), ClientError> {
        self.send(&AttachClientMessage::Metadata(metadata))
    }

    /// Answers `request`; false, sending nothing, when its epoch is no longer the stream's current one.
    ///
    /// # Errors
    ///
    /// The write failed.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the request is taken so it is answered once"
    )]
    pub fn answer(&self, request: Delegated, reply: DelegatedReply) -> Result<bool, ClientError> {
        let Delegated { id, epoch, request: _ } = request;
        if epoch != self.epoch() {
            return Ok(false);
        }
        self.send(&AttachClientMessage::DelegatedReply { id, epoch, reply })?;
        Ok(true)
    }

    /// Completes `barrier` with `data`, what the client's terminal answered to the Kitty graphics commands since the
    /// previous barrier (module docs), even empty; false, sending nothing, when its epoch is no longer the stream's
    /// current one.
    ///
    /// # Errors
    ///
    /// The write failed.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the barrier is taken so it is completed once"
    )]
    pub fn complete(&self, barrier: Barrier, data: Vec<u8>) -> Result<bool, ClientError> {
        let Barrier { id, epoch } = barrier;
        if epoch != self.epoch() {
            return Ok(false);
        }
        self.send(&AttachClientMessage::KittyComplete { id, epoch, data })?;
        Ok(true)
    }

    /// Leaves the session running: the server closes the connection, and the reader's stream ends. A write still in
    /// progress after [`DETACH_WAIT`] (a server that stopped reading) is given up: the connection is closed instead
    /// ([`AttachWriter::close`]), which leaves the session running too.
    ///
    /// # Errors
    ///
    /// The write failed or was given up (the connection is closed then; the reader's stream ends too).
    pub fn detach(&self) -> Result<(), ClientError> {
        self.shared.leaving.store(true, Ordering::SeqCst);
        let sent = match self.lock_within(DETACH_WAIT) {
            Some(serialized) => self.write(&serialized, &AttachClientMessage::Detach),
            None => Err(ClientError::Framing(FramingError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "a write to the live session server was still in progress after {}s",
                    DETACH_WAIT.as_secs_f32()
                ),
            )))),
        };
        if sent.is_err() {
            self.close();
        }
        sent
    }

    /// Closes the connection at once, waking a reader blocked on it and a write in progress; every later write is
    /// refused ([`ClientError::Disconnected`]) and the reader's stream ends. On Unix the server sees the client leave
    /// at once and detaches it. On Windows a client cannot disconnect its end of a pipe: the server sees the client
    /// leave only once the attachment's halves are all dropped, which closes it (until then the client stays attached,
    /// as a leader or the reply owner too); and a read that starts only after the call is not woken
    /// (`endpoint::disconnect`), so a caller whose reader may be about to read calls it again until the reader is done.
    pub fn close(&self) {
        self.shared.leaving.store(true, Ordering::SeqCst);
        self.shared.closed.store(true, Ordering::SeqCst);
        endpoint::disconnect(&self.shared.stream);
    }

    /// The stream's current epoch, as the reader last read it.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.shared.epoch.load(Ordering::SeqCst)
    }
}

/// The attach stream's contract, as a reader checks it (module docs).
#[derive(Debug, Default)]
struct StreamState {
    /// The last snapshot's epoch; zero before the first.
    epoch: u64,
    /// A snapshot's chunks are being read.
    in_snapshot: bool,
    /// Where the next output starts.
    next: u64,
    /// The exit or a shutdown was read.
    ended: bool,
}

impl StreamState {
    /// The event `message` is; `None` for one dropped as stale.
    fn accept(&mut self, message: AttachServerMessage) -> Result<Option<AttachEvent>, ClientError> {
        if self.ended {
            return Err(broken(format_args!("{message:?} after the stream's end")));
        }
        Ok(Some(match message {
            AttachServerMessage::SnapshotBegin { epoch, offset, size } => {
                if epoch <= self.epoch {
                    return Err(broken(format_args!(
                        "a snapshot of epoch {epoch} after epoch {}",
                        self.epoch
                    )));
                }
                (self.epoch, self.in_snapshot, self.next) = (epoch, true, offset);
                AttachEvent::SnapshotBegin { epoch, offset, size }
            }
            AttachServerMessage::SnapshotChunk { epoch, data } => {
                self.check_snapshot(epoch, "a snapshot chunk")?;
                AttachEvent::SnapshotChunk(data)
            }
            AttachServerMessage::SnapshotEnd { epoch } => {
                self.check_snapshot(epoch, "a snapshot end")?;
                self.in_snapshot = false;
                AttachEvent::SnapshotEnd
            }
            AttachServerMessage::Output { offset, data } => {
                self.check_offset(offset, "output")?;
                self.next += data.len() as u64;
                AttachEvent::Output { offset, data }
            }
            AttachServerMessage::Resized { offset, size } => {
                self.check_offset(offset, "a size change")?;
                AttachEvent::Resized { offset, size }
            }
            AttachServerMessage::Role { role, generation } => AttachEvent::Role { role, generation },
            AttachServerMessage::ReplyOwner { owner, generation } => AttachEvent::ReplyOwner { owner, generation },
            AttachServerMessage::KittyBarrier { id, epoch } => {
                if !self.current(epoch, "a Kitty barrier")? {
                    return Ok(None);
                }
                AttachEvent::KittyBarrier(Barrier { id, epoch })
            }
            AttachServerMessage::DelegatedRequest { id, epoch, request } => {
                if !self.current(epoch, "a delegated request")? {
                    return Ok(None);
                }
                AttachEvent::Delegated(Delegated { id, epoch, request })
            }
            AttachServerMessage::Refused { reason, message } => {
                return Err(broken(format_args!(
                    "a refusal ({reason:?}: {message}) of an attached client"
                )));
            }
            AttachServerMessage::Exit { status } => {
                self.ended = true;
                AttachEvent::Exit(status)
            }
            AttachServerMessage::Shutdown { reason } => {
                self.ended = true;
                AttachEvent::Shutdown(reason)
            }
        }))
    }

    fn check_snapshot(&self, epoch: u64, what: &str) -> Result<(), ClientError> {
        if self.in_snapshot && epoch == self.epoch {
            Ok(())
        } else if self.in_snapshot {
            Err(broken(format_args!(
                "{what} of epoch {epoch} in the snapshot of epoch {}",
                self.epoch
            )))
        } else {
            Err(broken(format_args!("{what} of epoch {epoch} outside a snapshot")))
        }
    }

    fn check_offset(&self, offset: u64, what: &str) -> Result<(), ClientError> {
        if self.epoch == 0 || self.in_snapshot {
            return Err(broken(format_args!(
                "{what} at {offset} outside the output between snapshots"
            )));
        }
        if offset != self.next {
            return Err(broken(format_args!(
                "{what} at {offset} where the output is at {}",
                self.next
            )));
        }
        Ok(())
    }

    /// Whether an answerable message of `epoch` is the current epoch's: false for a superseded one (dropped), an error
    /// for one ahead of every snapshot read.
    fn current(&self, epoch: u64, what: &str) -> Result<bool, ClientError> {
        if epoch > self.epoch {
            return Err(broken(format_args!(
                "{what} of epoch {epoch} ahead of epoch {}",
                self.epoch
            )));
        }
        if epoch < self.epoch {
            tracing::debug!(epoch, current = self.epoch, "{what} of a superseded epoch was dropped");
        }
        Ok(epoch == self.epoch)
    }
}

fn broken(what: fmt::Arguments<'_>) -> ClientError {
    ClientError::Protocol(what.to_string())
}

#[cfg(test)]
mod tests;
