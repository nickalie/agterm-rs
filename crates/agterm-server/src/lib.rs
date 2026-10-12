//! agterm's control server: an `AF_UNIX` stream socket answering one JSON request line per connection.
//!
//! Ported from upstream's `agterm/Control/ControlServer.swift` (contract in `.claude/rules/control-api.md` >
//! Transport and addressing):
//! - ownership: an exclusive `flock` on `<socket>.lock`, taken in [`ControlServer::new`] (before any shell is
//!   spawned, so `AGTERM_SOCKET` is right from the start) and kept until [`ControlServer::stop`]; a refused instance
//!   never unlinks or binds the path and advertises `<socket>.unavailable` instead;
//! - each connection reads one line and writes one response within [`ConnectionDeadlines`]; a line that cannot be
//!   read is answered `request too large or read failed`, one that does not decode `invalid request: <context>`;
//! - the request hops to the main loop through [`agterm_host::MainLoop`];
//! - a reply may hand the connection to a [`StreamOwner`] after its `ok` (see [`stream`]), as upstream hands
//!   `zmx.present` and `session.overlay.job.run` connections to their stream owners.
//!
//! Rust-only: each connection gets its own thread, where upstream serves connections one at a time on its accept
//! queue and gives a thread only to the commands that wait on the network; a handler's panic becomes `ok:false`.

mod connection;
pub mod stream;

use std::fs::{self, File, OpenOptions, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use agterm_core::paths::{ownership_lock_path, unavailable_socket_path};
use agterm_host::MainLoop;
use agterm_proto::Request;

pub use crate::connection::ConnectionDeadlines;
use crate::connection::Hop;
pub use crate::stream::{Reply, StreamOwner};

/// A socket path must be shorter than this many bytes: upstream's `start` guard (`socketPath.utf8.count < 104`, the
/// macOS `sun_path` size), kept on Linux although its `sun_path` holds 108.
pub const MAX_SOCKET_PATH_BYTES: usize = 104;

/// Why the server is not serving.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("control socket path {} is {len} bytes; it must be under {MAX_SOCKET_PATH_BYTES}", path.display())]
    PathTooLong { path: PathBuf, len: usize },
    #[error("control socket {} is already served by another instance — not binding", .0.display())]
    Refused(PathBuf),
    #[error("cannot lock {}: {source}", path.display())]
    Lock { path: PathBuf, source: io::Error },
    #[error("cannot bind {}: {source}", path.display())]
    Bind { path: PathBuf, source: io::Error },
    #[error("cannot start the control accept thread: {0}")]
    Thread(#[source] io::Error),
}

enum Ownership {
    /// This process holds the lock; the open file keeps it until dropped.
    Held { _lock: File },
    /// Another live instance holds the lock.
    Refused,
    /// No lock: never taken, released by `stop`, or the lock file could not be opened.
    Unowned,
}

struct Running {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

/// The control socket of one app instance.
pub struct ControlServer {
    socket: PathBuf,
    ownership: Ownership,
    deadlines: ConnectionDeadlines,
    running: Option<Running>,
}

/// Takes the exclusive, non-blocking `flock` on `<socket>.lock`, creating the socket's directory and the lock file.
fn acquire_lock(socket: &Path) -> Result<File, ServerError> {
    let path = ownership_lock_path(socket);
    let lock_error = |source| ServerError::Lock {
        path: path.clone(),
        source,
    };
    if let Some(parent) = socket.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(lock_error)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(lock_error)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(ServerError::Refused(socket.to_path_buf())),
        Err(fs::TryLockError::Error(source)) => Err(lock_error(source)),
    }
}

/// Replaces whatever is at `socket` (a stale socket of a dead instance; only the lock holder gets here) with a fresh
/// listener only the owner can connect to.
fn bind(socket: &Path) -> Result<UnixListener, ServerError> {
    let bind_error = |source| ServerError::Bind {
        path: socket.to_path_buf(),
        source,
    };
    if let Err(error) = fs::remove_file(socket)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::debug!(%error, socket = %socket.display(), "stale control socket not removed");
    }
    let listener = UnixListener::bind(socket).map_err(bind_error)?;
    // another local user must not drive this terminal over the control channel
    fs::set_permissions(socket, Permissions::from_mode(0o600)).map_err(bind_error)?;
    Ok(listener)
}

fn accept_loop<S: 'static>(listener: &UnixListener, stop: &AtomicBool, hop: &Arc<Hop<S>>) {
    loop {
        let accepted = listener.accept();
        if stop.load(Ordering::Acquire) {
            return;
        }
        match accepted {
            Ok((stream, _)) => spawn_connection(stream, hop),
            Err(error) => {
                // transient (EINTR, ECONNABORTED, EMFILE): keep serving, without spinning
                tracing::warn!(%error, "control accept failed");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn spawn_connection<S: 'static>(stream: UnixStream, hop: &Arc<Hop<S>>) {
    let hop = Arc::clone(hop);
    let spawned = thread::Builder::new()
        .name("agterm-control-conn".to_owned())
        .spawn(move || connection::serve(stream, &hop));
    if let Err(error) = spawned {
        tracing::warn!(%error, "control connection dropped: no thread for it");
    }
}

impl ControlServer {
    /// Settles ownership of `socket` without binding it. A refusal is not an error here: it shows in
    /// [`ControlServer::is_refused`] and [`ControlServer::advertised_path`], and [`ControlServer::start`] reports it.
    #[must_use]
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        let socket = socket.into();
        let ownership = match acquire_lock(&socket) {
            Ok(file) => Ownership::Held { _lock: file },
            Err(ServerError::Refused(path)) => {
                tracing::warn!(
                    "control socket {} is already served by another instance — not binding",
                    path.display()
                );
                Ownership::Refused
            }
            Err(error) => {
                tracing::warn!(%error, "control socket ownership not taken");
                Ownership::Unowned
            }
        };
        Self {
            socket,
            ownership,
            deadlines: ConnectionDeadlines::default(),
            running: None,
        }
    }

    /// Serves every connection within `deadlines` instead of upstream's defaults; a running server takes them at its
    /// next [`ControlServer::start`].
    #[must_use]
    pub fn with_deadlines(mut self, deadlines: ConnectionDeadlines) -> Self {
        self.deadlines = deadlines;
        self
    }

    /// The socket path this server owns or tried to.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Whether another live instance owns the socket.
    #[must_use]
    pub fn is_refused(&self) -> bool {
        matches!(self.ownership, Ownership::Refused)
    }

    /// Whether the socket is bound and accepting.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// What spawned shells get as `AGTERM_SOCKET`: the socket path, or `<socket>.unavailable` when refused.
    #[must_use]
    pub fn advertised_path(&self) -> PathBuf {
        if self.is_refused() {
            unavailable_socket_path(&self.socket)
        } else {
            self.socket.clone()
        }
    }

    /// Binds the socket and starts accepting. Each request runs `handler` on the main loop with the state it owns.
    /// Starting a running server does nothing.
    ///
    /// # Errors
    ///
    /// The path is [`MAX_SOCKET_PATH_BYTES`] or longer, another instance owns it, or the lock, bind or accept thread
    /// failed. Every failure after the lock keeps it, so no other instance can bind a path this one may advertise.
    pub fn start<S, H>(&mut self, main_loop: Arc<dyn MainLoop<S>>, handler: H) -> Result<(), ServerError>
    where
        S: 'static,
        H: Fn(&mut S, Request) -> Reply + Send + Sync + 'static,
    {
        if self.running.is_some() {
            return Ok(());
        }
        let len = self.socket.as_os_str().as_bytes().len();
        if len >= MAX_SOCKET_PATH_BYTES {
            return Err(ServerError::PathTooLong {
                path: self.socket.clone(),
                len,
            });
        }
        self.ensure_ownership()?;
        let listener = bind(&self.socket)?;
        let hop = Arc::new(Hop {
            main_loop,
            handler: Arc::new(handler),
            deadlines: self.deadlines,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("agterm-control-accept".to_owned())
            .spawn({
                let stop = Arc::clone(&stop);
                move || accept_loop(&listener, &stop, &hop)
            })
            .map_err(ServerError::Thread)?;
        tracing::info!("control socket at {}", self.socket.display());
        self.running = Some(Running { stop, thread });
        Ok(())
    }

    fn ensure_ownership(&mut self) -> Result<(), ServerError> {
        if matches!(self.ownership, Ownership::Held { .. }) {
            return Ok(());
        }
        match acquire_lock(&self.socket) {
            Ok(file) => {
                self.ownership = Ownership::Held { _lock: file };
                Ok(())
            }
            Err(error) => {
                if matches!(error, ServerError::Refused(_)) {
                    self.ownership = Ownership::Refused;
                }
                Err(error)
            }
        }
    }

    /// Stops accepting, removes the socket and releases the lock. The lock file stays: unlinking it would let the
    /// next instance lock a fresh inode and exclude nobody. Connections already accepted finish on their own threads.
    pub fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop.store(true, Ordering::Release);
            // the accept thread is blocked in accept(); a connection wakes it to see the flag
            match UnixStream::connect(&self.socket) {
                Ok(_) => {
                    if running.thread.join().is_err() {
                        tracing::error!("control accept thread panicked");
                    }
                }
                Err(error) => tracing::warn!(%error, "control accept thread not woken; leaving it detached"),
            }
            if let Err(error) = fs::remove_file(&self.socket)
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(%error, "control socket not removed");
            }
        }
        if matches!(self.ownership, Ownership::Held { .. }) {
            self.ownership = Ownership::Unowned;
        }
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop();
    }
}
