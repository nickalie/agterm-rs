//! The session server of one namespace: `agterm-session server <namespace directory>`, started on demand by the first
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
//! handshake is agterm's stable control layer ([`crate::protocol`]); the server leaves its launcher's working
//! directory ([`working_directory`]); and it reports whether it runs detached from its launcher
//! ([`CAPABILITY_DETACHED`], herdr's `detached_server_daemon` status) so the client that started it can check that
//! the server outlives it.
//!
//! # Panes
//!
//! [`pane::Pane`] is one live session: its program on a PTY, the terminal its output builds and the clients that
//! output fans out to over byte-bounded, lossless queues ([`clients`]), with the client that owns its size chosen by
//! [`lead`]; [`pane::Panes`] holds them by name. The connections do not serve them yet.
//!
//! # Connections
//!
//! A connection counts as a client from its accept until it closes. Every connection starts with a hello
//! ([`crate::protocol::Control::Hello`]) within [`HANDSHAKE_TIMEOUT`] (on Unix; Windows pipes take no timeout). So far
//! the server serves the control role only: an api or attach hello is refused with
//! [`crate::protocol::ErrorCode::UnsupportedRole`], and a handoff request with a failed result, since the server does
//! not advertise [`crate::protocol::CAPABILITY_HANDOFF_REQUEST`].

pub mod clients;
pub mod lead;
pub mod lifecycle;
pub mod pane;
pub mod spawn;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::endpoint::{Endpoint, Listener, Stream};
use crate::protocol::{
    CAPABILITY_HANDOFF_REQUEST, Control, ControlError, ErrorCode, FramingError, HandoffResult, Hello, Role, Welcome,
    read_frame, welcome, write_control,
};
use lifecycle::{Graces, Lifecycle, StopReason};

/// The build id this server reports in its welcome.
pub const BUILD_ID: &str = concat!("agterm-session ", env!("CARGO_PKG_VERSION"));

/// The capability of a server that runs detached from whoever started it ([`spawn::current_process_is_detached`]),
/// so it outlives its launcher.
pub const CAPABILITY_DETACHED: &str = "server.detached.v1";

/// How long a connection has to send its hello, and a client to read the welcome.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// `agterm-session server`'s exit status once it stopped by itself.
pub const EXIT_STOPPED: u8 = 0;
/// `agterm-session server`'s exit status when it failed.
pub const EXIT_FAILED: u8 = 1;
/// `agterm-session server`'s exit status for a command line it does not take.
pub const EXIT_USAGE: u8 = 2;
/// `agterm-session server`'s exit status when another server holds the namespace: the one a racing start lost to.
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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            build: BUILD_ID.to_owned(),
            graces: Graces::default(),
        }
    }
}

/// Serves the namespace `directory` until the server stops by itself, and returns why.
///
/// # Errors
///
/// The endpoint could not be bound ([`io::ErrorKind::AddrInUse`] when another server holds it; see
/// [`Endpoint::bind`]), or a thread could not be started.
pub fn run(directory: &Path, config: &ServerConfig) -> io::Result<StopReason> {
    let endpoint = Endpoint::new(directory);
    let listener = endpoint.bind()?;
    let shared = Arc::new(Shared {
        lifecycle: Mutex::new(Lifecycle::new(Instant::now(), config.graces)),
        changed: Condvar::new(),
        identity: Identity {
            build: config.build.clone(),
            capabilities: capabilities(),
        },
    });
    let (done, accept_done) = mpsc::channel();
    let accepting = Arc::clone(&shared);
    let accept = thread::Builder::new()
        .name("agterm-session-accept".to_owned())
        .spawn(move || {
            accept_loop(&listener, &accepting);
            drop(listener);
            let _ = done.send(());
        })?;
    let reason = shared.wait_for_stop();
    // the accept loop sees the stop with its next connection: this one, held until the loop has let go
    let _wake = endpoint.connect_timeout(ACCEPT_STOP_TIMEOUT).ok();
    if accept_done.recv_timeout(ACCEPT_STOP_TIMEOUT).is_ok() {
        let _ = accept.join();
    }
    Ok(reason)
}

/// What this server process reports about itself.
fn capabilities() -> Vec<String> {
    let mut capabilities = Vec::new();
    if spawn::current_process_is_detached() {
        capabilities.push(CAPABILITY_DETACHED.to_owned());
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
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Lifecycle> {
        // a panicking connection thread leaves the counts as they were, which stay usable
        self.lifecycle.lock().unwrap_or_else(PoisonError::into_inner)
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
        if !shared.lock().client_connected() {
            return;
        }
        shared.changed.notify_all();
        let connection = Arc::clone(shared);
        let started = thread::Builder::new()
            .name("agterm-session-connection".to_owned())
            .spawn(move || {
                let _left = ClientLeaves(&connection);
                // a broken or vanished client ends only its own connection
                let _ = serve_connection(stream, &connection.identity);
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

/// One connection: the handshake, then its role until the client closes.
fn serve_connection(mut stream: Stream, identity: &Identity) -> Result<(), FramingError> {
    set_recv_timeout(&stream, Some(HANDSHAKE_TIMEOUT));
    let frame = read_frame(&mut stream)?;
    let welcome = match frame.control() {
        Ok(Control::Hello(hello)) => welcome_for(&hello, frame.protocol, identity),
        Ok(_) => refusal(
            identity,
            ControlError::new(ErrorCode::UnknownOp, "a connection starts with hello.v1"),
        ),
        Err(error) => return Err(error),
    };
    let refused = welcome.error.is_some();
    write_control(&mut stream, &Control::Welcome(welcome))?;
    if refused {
        return Ok(());
    }
    set_recv_timeout(&stream, None);
    serve_control(&mut stream)
}

/// The welcome for `hello` from a client of `protocol`: [`welcome`]'s, refusing the roles this server does not serve
/// yet.
fn welcome_for(hello: &Hello, protocol: u32, identity: &Identity) -> Welcome {
    let mut answer = welcome(hello, protocol, &identity.build, &identity.capabilities);
    if answer.error.is_none() && hello.role != Role::Control {
        answer.error = Some(ControlError::new(
            ErrorCode::UnsupportedRole,
            "this agterm-session server serves control connections only",
        ));
    }
    answer
}

fn refusal(identity: &Identity, error: ControlError) -> Welcome {
    Welcome {
        build: identity.build.clone(),
        capabilities: identity.capabilities.clone(),
        pid: std::process::id(),
        error: Some(error),
    }
}

/// A control connection: it holds the server until the client closes it. A handoff request is answered as failed
/// (the server does not advertise [`CAPABILITY_HANDOFF_REQUEST`]); anything else closes the connection.
fn serve_control(stream: &mut Stream) -> Result<(), FramingError> {
    let frame = match read_frame(stream) {
        Ok(frame) => frame,
        Err(FramingError::Closed) => return Ok(()),
        Err(error) => return Err(error),
    };
    if let Control::HandoffRequest(_) = frame.control()? {
        let error = ControlError::new(
            ErrorCode::HandoffFailed,
            format!("this agterm-session server takes no handoff requests (no {CAPABILITY_HANDOFF_REQUEST})"),
        );
        write_control(stream, &Control::HandoffResult(HandoffResult(Err(error))))?;
    }
    Ok(())
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
    use crate::protocol::PROTOCOL_VERSION;

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

    #[test]
    fn a_control_hello_is_welcomed_with_the_servers_identity() {
        let welcome = welcome_for(&hello(Role::Control), PROTOCOL_VERSION, &identity());
        assert_eq!(welcome.error, None);
        assert_eq!(welcome.build, "test-build");
        assert_eq!(welcome.capabilities, [CAPABILITY_DETACHED]);
        assert_eq!(welcome.pid, std::process::id());
    }

    #[test]
    fn the_roles_not_served_yet_are_refused() {
        for role in [Role::Api, Role::Attach] {
            let welcome = welcome_for(&hello(role), PROTOCOL_VERSION, &identity());
            let error = welcome.error.unwrap();
            assert_eq!(error.code, ErrorCode::UnsupportedRole);
            assert_eq!(
                error.message,
                "this agterm-session server serves control connections only"
            );
        }
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

    #[test]
    fn the_default_config_reports_the_crate_build() {
        let config = ServerConfig::default();
        assert_eq!(config.build, BUILD_ID);
        assert!(BUILD_ID.starts_with("agterm-session "));
        assert_eq!(config.graces, Graces::default());
    }
}
