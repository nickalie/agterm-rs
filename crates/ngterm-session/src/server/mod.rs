//! The session server of one namespace: `ngterm-session server <namespace directory>`, started on demand by the first
//! client that finds none ([`spawn::connect_or_start`]), detached from it, and stopped by itself once it holds no
//! session and no client ([`lifecycle`]).
//!
//! herdr: `src/server/headless/bootstrap.rs` (`run_server`: bind, then serve until stopped; a second server under a
//! taken endpoint gives up), `src/server/client_accept.rs` and `src/api/server.rs` (the accept loop and a thread per
//! connection) and `src/server/client_transport.rs` (`handle_client_handshake`: the hello read under a timeout before
//! anything else) at `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified. Kept: the
//! server owns its endpoint for its whole life, accepts on one thread, gives every connection a thread of its own that
//! starts with the handshake, and drops what it cannot serve. Changed: threads only, with the stop decided by a
//! coordinator waiting on [`lifecycle::Lifecycle`]'s deadline instead of herdr's tokio loop and stop flag; the
//! handshake is ngterm's stable control layer ([`crate::protocol`]); the server leaves its launcher's working
//! directory ([`working_directory`]); and it reports whether it runs detached from its launcher
//! ([`CAPABILITY_DETACHED`], herdr's `detached_server_daemon` status) so the client that started it can check that
//! the server outlives it.
//!
//! # Panes
//!
//! [`pane::Pane`] is one live session: its program on a PTY, the terminal its output builds and the clients that
//! output fans out to over byte-bounded, lossless queues ([`clients`]), with the client that owns its size chosen by
//! [`lead`], each client starting from a snapshot of the session ([`snapshot`]), its program's queries answered once,
//! by the server's terminal with the facts of the client that owns the replies ([`replies`]), a client that fell
//! behind given back the clipboard writes it missed ([`recovery`]); [`pane::Panes`] holds the server's by name. A session counts in the server's lifecycle from its creation until its program ends.
//!
//! # Connections
//!
//! A connection counts as a client from its accept until it closes. Every connection starts with a hello
//! ([`crate::protocol::Control::Hello`]) within [`HANDSHAKE_TIMEOUT`] (on Unix; Windows pipes take no timeout). The
//! server serves every role: an attach connection attaches to a session, creating it when asked, and carries its
//! stream ([`attach`]); an api connection asks what `SessionHost` and `ngtermctl clipboard set` need, request by
//! request ([`api`]); a control connection holds the server, and asks it to hand its sessions over to a server of
//! another build ([`handoff`]), which it takes when it advertises [`crate::protocol::CAPABILITY_HANDOFF_REQUEST`]. A
//! role this server does not know is refused with [`crate::protocol::ErrorCode::UnsupportedRole`].
//!
//! Every api and attach operation runs under the server's operation lock, shared, and only for a connection of the
//! current generation, the one it was admitted in at its accept (not at its hello, which may come after a handoff
//! began): an update handoff takes the lock exclusively and moves the generation on, so no operation decoded before
//! it runs after it.
//!
//! # The command
//!
//! [`command`] is what `ngterm-session server …` runs: `server <namespace directory>` serves the namespace;
//! `server <namespace directory> --handoff-import <channel> <token>` is an update handoff's replacement, which takes the
//! namespace's sessions and endpoint over from the server that started it ([`run_import`]).

pub mod api;
mod attach;
pub mod clients;
pub mod handoff;
pub mod lead;
pub mod lifecycle;
pub mod pane;
pub mod recovery;
pub mod replies;
pub mod snapshot;
pub mod spawn;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, RwLock, RwLockWriteGuard, Weak};
use std::thread;
use std::time::{Duration, Instant};

use crate::endpoint::{self, Endpoint, Listener, Stream};
use crate::job::Sweeper;
use crate::protocol::{
    CAPABILITY_HANDOFF_REQUEST, Control, ControlError, ErrorCode, FramingError, HandoffResult, Role, Welcome,
    read_frame, welcome, write_control,
};
use clients::ResyncBudget;
use handoff::Support;
use lifecycle::{Graces, Lifecycle, StopReason};
use pane::{Limits, Panes};

/// The build id a server without a staged id reports in its welcome ([`crate::build_id::UNSTAGED`]).
pub const BUILD_ID: &str = crate::build_id::UNSTAGED;

/// The capability of a server that runs detached from whoever started it ([`spawn::current_process_is_detached`]),
/// so it outlives its launcher.
pub const CAPABILITY_DETACHED: &str = "server.detached.v1";

/// How long a connection has to send its hello, and a client to read the welcome.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// `ngterm-session server`'s exit status once it stopped by itself.
pub const EXIT_STOPPED: u8 = 0;
/// `ngterm-session server`'s exit status when it failed.
pub const EXIT_FAILED: u8 = 1;
/// `ngterm-session server`'s exit status for a command line it does not take.
pub const EXIT_USAGE: u8 = 2;
/// `ngterm-session server`'s exit status when another server holds the namespace: the one a racing start lost to.
pub const EXIT_TAKEN: u8 = 3;

/// How long the accept loop waits after an accept failed (descriptor exhaustion is transient, and exiting would end
/// every session the server holds).
const ACCEPT_RETRY: Duration = Duration::from_millis(100);
/// How long a stopping server waits for its accept loop to let go of the endpoint.
const ACCEPT_STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// What a server runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// The build id its welcome reports.
    pub build: String,
    pub graces: Graces,
    /// The output bytes each attached client's queue holds ([`clients::DEFAULT_QUEUE_LIMIT`]).
    pub client_queue_limit: usize,
    /// How often a client that falls behind is resynced before it is disconnected.
    pub resync_budget: ResyncBudget,
    /// Whether it hands its sessions over when asked ([`Support::of_this_host`]).
    pub handoff: Support,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            build: BUILD_ID.to_owned(),
            graces: Graces::default(),
            client_queue_limit: clients::DEFAULT_QUEUE_LIMIT,
            resync_budget: ResyncBudget::default(),
            handoff: Support::of_this_host(),
        }
    }
}

impl ServerConfig {
    /// The limits of the panes it takes over in a handoff.
    fn limits(&self) -> Limits {
        Limits {
            client_queue_limit: self.client_queue_limit,
            resync_budget: self.resync_budget,
            ..Limits::default()
        }
    }
}

/// Serves the namespace `directory` until the server stops by itself (or handed its sessions over), and returns why.
///
/// # Errors
///
/// The endpoint could not be bound ([`io::ErrorKind::AddrInUse`] when another server holds it; see
/// [`Endpoint::bind`]), or a thread could not be started.
pub fn run(directory: &Path, config: &ServerConfig) -> io::Result<StopReason> {
    let endpoint = Endpoint::new(directory);
    let listener = endpoint.bind()?;
    let shared = Shared::new(&endpoint, config);
    let accepting = Accepting::start(listener, &shared)?;
    Ok(serve(&endpoint, accepting, &shared))
}

/// An update handoff's replacement for the namespace `directory` ([`handoff`]): it takes the sessions and the endpoint
/// over from the server listening on `channel`, which started it with `token`, then serves as [`run`] does.
///
/// # Errors
///
/// The handoff failed before its commit (nothing was taken: the old server keeps serving), or a thread could not be
/// started.
pub fn run_import(directory: &Path, channel: &Path, token: &str, config: &ServerConfig) -> io::Result<StopReason> {
    let endpoint = Endpoint::new(directory);
    let shared = Shared::new(&endpoint, config);
    let imported = handoff::receive(&config.build, &endpoint, channel, token, config.limits())?;
    // the accept thread is the last thing that can fail: started before `ready`, it accepts once committed
    let accepting = Accepting::start(imported.listener, &shared)?;
    handoff::write_line(&imported.stream, "ready")?;
    if !handoff::committed(&imported.stream) {
        // the accept thread lets the listener go without accepting
        return Err(io::Error::other(
            "the handoff source did not commit; its sessions stay with it",
        ));
    }
    for pane in imported.panes {
        pane.activate();
        shared.lock().session_started();
        let pane = shared.panes.adopt(pane);
        let ending = Arc::clone(&shared);
        pane.on_end(Box::new(move || ending.session_ended()));
    }
    if let Some(listener) = shared.listener() {
        listener.claim();
    }
    if let Err(err) = handoff::write_line(&imported.stream, "owned") {
        tracing::debug!(%err, "the handoff source left before it was told the sessions are owned");
    }
    tracing::info!(sessions = shared.panes.len(), "update handoff imported");
    Ok(serve(&endpoint, accepting, &shared))
}

/// The accept thread of a server, started before it serves and accepting only once opened ([`serve`]): dropped
/// unopened, it lets the listener go without accepting.
struct Accepting {
    open: mpsc::Sender<()>,
    done: mpsc::Receiver<()>,
    thread: thread::JoinHandle<()>,
}

impl Accepting {
    /// The accept thread over `listener`, which `shared` names as the server's from now on.
    ///
    /// # Errors
    ///
    /// The thread could not be started.
    fn start(listener: Listener, shared: &Arc<Shared>) -> io::Result<Self> {
        let listener = Arc::new(listener);
        *lock(&shared.listener) = Arc::downgrade(&listener);
        let (open, opened) = mpsc::channel();
        let (done, accept_done) = mpsc::channel();
        let accepting = Arc::clone(shared);
        let thread = thread::Builder::new()
            .name("ngterm-session-accept".to_owned())
            .spawn(move || {
                if opened.recv().is_ok() {
                    accept_loop(&listener, &accepting);
                }
                drop(listener);
                let _ = done.send(());
            })?;
        Ok(Self {
            open,
            done: accept_done,
            thread,
        })
    }
}

/// Accept on the thread `accepting` until the server stops; why it stopped.
fn serve(endpoint: &Endpoint, accepting: Accepting, shared: &Arc<Shared>) -> StopReason {
    let Accepting {
        open,
        done: accept_done,
        thread: accept,
    } = accepting;
    // a send fails only when the thread is gone already
    let _ = open.send(());
    let reason = shared.wait_for_stop();
    if reason == StopReason::HandedOver {
        // the endpoint is the replacement's: no wake through it; the accept thread ends with the process
        return reason;
    }
    // the accept loop sees the stop with its next connection: this one, held until the loop has let go
    let _wake = endpoint.connect_timeout(ACCEPT_STOP_TIMEOUT).ok();
    if accept_done.recv_timeout(ACCEPT_STOP_TIMEOUT).is_ok() {
        let _ = accept.join();
    }
    reason
}

/// What `ngterm-session server <args…>` runs, `args` being what follows `server` (module docs), with its exit status:
/// [`EXIT_STOPPED`] once it stopped by itself or handed its sessions over, [`EXIT_TAKEN`] when another server holds
/// the namespace, [`EXIT_USAGE`] for arguments it does not take (after a usage line on standard error), else
/// [`EXIT_FAILED`].
#[must_use]
pub fn command(args: &[OsString]) -> ExitCode {
    let (directory, import) = match args {
        [directory] => (directory, None),
        [directory, flag, channel, token] if flag == handoff::IMPORT_FLAG => (directory, Some((channel, token))),
        _ => {
            eprintln!(
                "usage: ngterm-session server <namespace directory> [{} <channel> <token>]",
                handoff::IMPORT_FLAG
            );
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let config = ServerConfig {
        build: crate::build_id::of_this_process(),
        ..ServerConfig::default()
    };
    // the directory is made absolute before the server leaves the working directory it was started in
    let result = std::path::absolute(Path::new(directory)).and_then(|directory| {
        std::env::set_current_dir(working_directory(&directory)?)?;
        match import {
            None => run(&directory, &config),
            Some((channel, token)) => {
                let token = token.to_string_lossy();
                run_import(&directory, Path::new(channel), &token, &config)
            }
        }
    });
    match result {
        Ok(_) => ExitCode::from(EXIT_STOPPED),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            eprintln!("ngterm-session server: {error}");
            ExitCode::from(EXIT_TAKEN)
        }
        Err(error) => {
            eprintln!("ngterm-session server: {error}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}

/// What this server process reports about itself.
fn capabilities(handoff: &Support) -> Vec<String> {
    let mut capabilities = Vec::new();
    if spawn::current_process_is_detached() {
        capabilities.push(CAPABILITY_DETACHED.to_owned());
    }
    if *handoff == Support::Supported {
        capabilities.push(CAPABILITY_HANDOFF_REQUEST.to_owned());
    }
    capabilities
}

/// The build and capabilities every welcome carries.
struct Identity {
    build: String,
    capabilities: Vec<String>,
}

/// The state the coordinator, the accept loop and the connections share.
struct Shared {
    lifecycle: Mutex<Lifecycle>,
    /// Notified whenever a client or a session comes or goes.
    changed: Condvar,
    identity: Identity,
    /// The live sessions.
    panes: Panes,
    /// The process table the api's foreground jobs are read from and signalled through.
    jobs: Sweeper,
    /// The namespace directory served, absolute: the sessions' `NGTERM_LIVE_DIR`.
    namespace: PathBuf,
    /// The sessions' client queues and resync budget ([`ServerConfig`]).
    client_queue_limit: usize,
    resync_budget: ResyncBudget,
    /// Whether it hands its sessions over when asked.
    handoff: Support,
    /// The operation lock (module docs): api and attach operations take it shared, a handoff exclusively.
    ops: RwLock<()>,
    /// The connection generation an operation must belong to (module docs).
    generation: AtomicU64,
    /// The api and attach connections, so a handoff can disconnect them.
    connections: Mutex<Connections>,
    /// A handoff runs: connections accepted meanwhile are closed at once.
    handing_off: AtomicBool,
    /// The handoffs this server failed, which it does not run again.
    failed_handoffs: handoff::Failures,
    /// The listener the accept loop owns, for a handoff to pass on.
    listener: Mutex<Weak<Listener>>,
}

/// The registered connections, by id.
#[derive(Default)]
struct Connections {
    next: u64,
    streams: HashMap<u64, Arc<Stream>>,
}

/// One api or attach connection, registered for as long as it lives.
struct Connection<'a> {
    shared: &'a Shared,
    id: u64,
    /// The generation it was admitted in ([`Shared::admit`]).
    generation: u64,
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        lock(&self.shared.connections).streams.remove(&self.id);
    }
}

impl Connection<'_> {
    /// Run `operation` under the operation lock, shared, unless the connection's generation is over (a handoff began
    /// after it was accepted): `None` then, and nothing ran.
    fn operate<R>(&self, operation: impl FnOnce() -> R) -> Option<R> {
        let _shared = self.shared.ops.read().unwrap_or_else(PoisonError::into_inner);
        (self.shared.generation.load(Ordering::Acquire) == self.generation).then(operation)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn new(endpoint: &Endpoint, config: &ServerConfig) -> Arc<Self> {
        Arc::new(Self {
            lifecycle: Mutex::new(Lifecycle::new(Instant::now(), config.graces)),
            changed: Condvar::new(),
            identity: Identity {
                build: config.build.clone(),
                capabilities: capabilities(&config.handoff),
            },
            panes: Panes::new(),
            jobs: Sweeper::live(),
            namespace: endpoint.directory().to_path_buf(),
            client_queue_limit: config.client_queue_limit,
            resync_budget: config.resync_budget,
            handoff: config.handoff.clone(),
            ops: RwLock::new(()),
            generation: AtomicU64::new(0),
            connections: Mutex::new(Connections::default()),
            handing_off: AtomicBool::new(false),
            failed_handoffs: handoff::Failures::default(),
            listener: Mutex::new(Weak::new()),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Lifecycle> {
        // a panicking connection thread leaves the counts as they were, which stay usable
        self.lifecycle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The generation a connection accepted now belongs to, or `None` while a handoff runs (the connection is dropped,
    /// its client retrying). The generation is read before the handoff flag, so a connection admitted before a handoff
    /// fenced it is of the generation that fence ended, however late its hello registers it ([`Shared::register`]).
    fn admit(&self) -> Option<u64> {
        let generation = self.generation.load(Ordering::Acquire);
        (!self.handing_off.load(Ordering::Acquire)).then_some(generation)
    }

    /// A handoff's first step: the operation lock taken exclusively (held by the returned guard), the generation moved
    /// on, so no connection admitted before runs another operation, and every api and attach connection disconnected.
    fn fence(&self) -> RwLockWriteGuard<'_, ()> {
        let fence = self.ops.write().unwrap_or_else(PoisonError::into_inner);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.disconnect_all();
        fence
    }

    /// Register `stream` as an api or attach connection of `generation`, the one it was admitted in
    /// ([`Shared::admit`]).
    fn register(&self, stream: &Arc<Stream>, generation: u64) -> Connection<'_> {
        let mut connections = lock(&self.connections);
        let id = connections.next;
        connections.next += 1;
        connections.streams.insert(id, Arc::clone(stream));
        Connection {
            shared: self,
            id,
            generation,
        }
    }

    /// Disconnect every api and attach connection (a handoff's first step).
    fn disconnect_all(&self) {
        for stream in lock(&self.connections).streams.values() {
            endpoint::disconnect(stream);
        }
    }

    /// The listener the accept loop serves, while it does.
    fn listener(&self) -> Option<Arc<Listener>> {
        lock(&self.listener).upgrade()
    }

    /// Waits until the lifecycle decides to stop.
    fn wait_for_stop(&self) -> StopReason {
        let mut lifecycle = self.lock();
        loop {
            let now = Instant::now();
            if let Some(reason) = lifecycle.poll(now) {
                return reason;
            }
            lifecycle = match lifecycle.deadline() {
                Some(deadline) => {
                    self.changed
                        .wait_timeout(lifecycle, deadline.saturating_duration_since(now))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self.changed.wait(lifecycle).unwrap_or_else(PoisonError::into_inner),
            };
        }
    }

    fn client_left(&self) {
        self.lock().client_disconnected(Instant::now());
        self.changed.notify_all();
    }
}

/// Accepts connections until the server has decided to stop; a connection that arrives after the decision is dropped,
/// and its client reconnects to the next server.
fn accept_loop(listener: &Listener, shared: &Arc<Shared>) {
    loop {
        let stream = match listener.accept() {
            Ok(stream) => stream,
            Err(_) if shared.lock().stopped().is_some() => return,
            Err(_) => {
                thread::sleep(ACCEPT_RETRY);
                continue;
            }
        };
        let Some(generation) = shared.admit() else {
            // its client retries, and finds whichever server serves once the handoff is over
            drop(stream);
            continue;
        };
        if !shared.lock().client_connected() {
            return;
        }
        shared.changed.notify_all();
        let connection = Arc::clone(shared);
        let started = thread::Builder::new()
            .name("ngterm-session-connection".to_owned())
            .spawn(move || {
                let _left = ClientLeaves(&connection);
                // a broken or vanished client ends only its own connection
                let _ = serve_connection(stream, &connection, generation);
            });
        if started.is_err() {
            shared.client_left();
        }
    }
}

/// Counts the client out when its connection thread ends, however it ends.
struct ClientLeaves<'a>(&'a Shared);

impl Drop for ClientLeaves<'_> {
    fn drop(&mut self) {
        self.0.client_left();
    }
}

/// One connection, admitted in `generation`: the handshake, then its role until the client closes.
fn serve_connection(mut stream: Stream, shared: &Arc<Shared>, generation: u64) -> Result<(), FramingError> {
    let identity = &shared.identity;
    set_recv_timeout(&stream, Some(HANDSHAKE_TIMEOUT));
    let frame = read_frame(&mut stream)?;
    let (welcome, role) = match frame.control() {
        Ok(Control::Hello(hello)) => (
            welcome(&hello, frame.protocol, &identity.build, &identity.capabilities),
            hello.role,
        ),
        Ok(_) => (
            refusal(
                identity,
                ControlError::new(ErrorCode::UnknownOp, "a connection starts with hello.v1"),
            ),
            Role::Unknown,
        ),
        Err(error) => return Err(error),
    };
    let refused = welcome.error.is_some();
    write_control(&mut stream, &Control::Welcome(welcome))?;
    if refused {
        return Ok(());
    }
    set_recv_timeout(&stream, None);
    match role {
        Role::Attach | Role::Api => {
            let stream = Arc::new(stream);
            let connection = shared.register(&stream, generation);
            if role == Role::Attach {
                attach::serve(&stream, shared, &connection)
            } else {
                api::serve(&stream, shared, &connection)
            }
        }
        // a welcomed hello names a role this server knows
        Role::Control | Role::Unknown => serve_control(&mut stream, shared),
    }
}

fn refusal(identity: &Identity, error: ControlError) -> Welcome {
    Welcome {
        build: identity.build.clone(),
        capabilities: identity.capabilities.clone(),
        pid: std::process::id(),
        error: Some(error),
    }
}

/// A control connection: it holds the server until the client closes it. A handoff request runs the handoff
/// ([`handoff`]) and is answered with its outcome; once it committed, the server stops (its sessions are the
/// replacement's). A server that does not hand over ([`ServerConfig::handoff`]) answers it failed with the reason.
/// Anything else closes the connection.
fn serve_control(stream: &mut Stream, shared: &Arc<Shared>) -> Result<(), FramingError> {
    let frame = match read_frame(stream) {
        Ok(frame) => frame,
        Err(FramingError::Closed) => return Ok(()),
        Err(error) => return Err(error),
    };
    let Control::HandoffRequest(request) = frame.control()? else {
        return Ok(());
    };
    let result = handoff::export(shared, &request);
    let committed = result.is_ok();
    let answered = write_control(stream, &Control::HandoffResult(HandoffResult(result)));
    if committed {
        shared.lock().hand_over();
        shared.changed.notify_all();
    }
    answered
}

/// The refusal of a handoff by a server that does not hand over.
fn no_handoff(reason: &str) -> ControlError {
    ControlError::new(
        ErrorCode::HandoffFailed,
        format!("this ngterm-session server takes no handoff requests (no {CAPABILITY_HANDOFF_REQUEST}): {reason}"),
    )
}

/// Bounds a read on Unix; Windows pipes take no timeout, so there a silent client waits as a client.
fn set_recv_timeout(stream: &Stream, timeout: Option<Duration>) {
    #[cfg(unix)]
    {
        use interprocess::local_socket::traits::Stream as _;
        // without the timeout a silent client only holds its own connection
        let _ = stream.set_recv_timeout(timeout);
    }
    #[cfg(windows)]
    {
        let _ = (stream, timeout);
    }
}

/// The directory a server works in: the root of its namespace directory's file system, so it never holds the
/// directory its launcher worked in (on Windows that would keep it from being removed).
///
/// # Errors
///
/// `directory` is relative and the current directory cannot be read.
pub fn working_directory(directory: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(directory)?;
    Ok(absolute
        .ancestors()
        .last()
        .map_or_else(|| absolute.clone(), Path::to_path_buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Hello, PROTOCOL_VERSION};

    fn identity() -> Identity {
        Identity {
            build: "test-build".to_owned(),
            capabilities: vec![CAPABILITY_DETACHED.to_owned()],
        }
    }

    fn hello(role: Role) -> Hello {
        Hello {
            role,
            build: "client".to_owned(),
            capabilities: Vec::new(),
        }
    }

    fn welcome_for(hello: &Hello, protocol: u32, identity: &Identity) -> Welcome {
        welcome(hello, protocol, &identity.build, &identity.capabilities)
    }

    #[test]
    fn a_control_hello_is_welcomed_with_the_servers_identity() {
        let welcome = welcome_for(&hello(Role::Control), PROTOCOL_VERSION, &identity());
        assert_eq!(welcome.error, None);
        assert_eq!(welcome.build, "test-build");
        assert_eq!(welcome.capabilities, [CAPABILITY_DETACHED]);
        assert_eq!(welcome.pid, std::process::id());
    }

    #[test]
    fn every_known_role_is_served_and_an_unknown_one_refused() {
        for role in [Role::Control, Role::Api, Role::Attach] {
            let welcome = welcome_for(&hello(role), PROTOCOL_VERSION, &identity());
            assert_eq!(welcome.error, None, "{role:?} is served");
        }
        let welcome = welcome_for(&hello(Role::Unknown), PROTOCOL_VERSION, &identity());
        assert_eq!(welcome.error.unwrap().code, ErrorCode::UnsupportedRole);
    }

    #[test]
    fn a_protocol_mismatch_is_refused_as_such_before_the_role() {
        let welcome = welcome_for(&hello(Role::Api), PROTOCOL_VERSION + 1, &identity());
        assert_eq!(welcome.error.unwrap().code, ErrorCode::ProtocolMismatch);
        // the control role is welcomed across protocols
        let welcome = welcome_for(&hello(Role::Control), PROTOCOL_VERSION + 1, &identity());
        assert_eq!(welcome.error, None);
    }

    #[test]
    fn a_server_works_in_the_root_of_its_namespace() {
        #[cfg(unix)]
        assert_eq!(
            working_directory(Path::new("/tmp/ngterm-live-0123456789abcdef")).unwrap(),
            Path::new("/")
        );
        #[cfg(windows)]
        assert_eq!(
            working_directory(Path::new(r"C:\Users\a\AppData\Local\ngterm\live\0123")).unwrap(),
            Path::new(r"C:\")
        );
        // a relative directory is taken from the current one, whose root it shares
        let current = std::env::current_dir().unwrap();
        assert_eq!(
            working_directory(Path::new("live")).unwrap(),
            current.ancestors().last().unwrap()
        );
    }

    /// A connected stream over an endpoint in `directory`, its peer kept open beside it.
    fn connection(directory: &Path) -> (Arc<Stream>, Stream) {
        let endpoint = Endpoint::new(directory);
        let listener = endpoint.bind().unwrap();
        let accepting = thread::spawn(move || listener.accept().unwrap());
        let client = endpoint.connect().unwrap();
        (Arc::new(accepting.join().unwrap()), client)
    }

    /// A connection admitted before a handoff fenced the server and registered after (its hello was still on the way)
    /// runs nothing; connections are dropped while the handoff runs, and one admitted after it runs its operations.
    #[test]
    fn a_connection_admitted_before_a_handoff_runs_nothing_after_it() {
        let root = tempfile::tempdir().unwrap();
        let shared = Shared::new(&Endpoint::new(root.path().join("live")), &ServerConfig::default());
        let admitted = shared.admit().unwrap();
        // the handoff begins while that connection's hello is on the way
        shared.handing_off.store(true, Ordering::Release);
        drop(shared.fence());
        assert_eq!(shared.admit(), None, "dropped while the handoff runs");
        let (late, _peer) = connection(&root.path().join("late"));
        let registered = shared.register(&late, admitted);
        assert_eq!(
            registered.operate(|| "ran"),
            None,
            "the late registration belongs to the fenced generation"
        );
        // the handoff rolled back: a fresh connection is of the next generation, and runs
        shared.handing_off.store(false, Ordering::Release);
        let fresh = shared.admit().unwrap();
        assert_eq!(fresh, admitted + 1);
        let (stream, _other) = connection(&root.path().join("fresh"));
        assert_eq!(shared.register(&stream, fresh).operate(|| "ran"), Some("ran"));
    }

    #[test]
    fn the_default_config_reports_the_crate_build() {
        let config = ServerConfig::default();
        assert_eq!(config.build, BUILD_ID);
        assert!(BUILD_ID.starts_with("ngterm-session "));
        assert_eq!(config.graces, Graces::default());
        assert_eq!(config.client_queue_limit, clients::DEFAULT_QUEUE_LIMIT);
        assert_eq!(config.resync_budget, ResyncBudget::default());
    }
}
