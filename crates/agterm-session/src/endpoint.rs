//! The session server's endpoint in an instance's namespace: a Unix domain socket in the namespace directory, a named
//! pipe on Windows.
//!
//! herdr: `src/ipc.rs` and `src/server/socket_paths.rs` at `HERDR_REV` (`build/pins.env`), licensed under the Apache
//! License 2.0, modified. Kept: `interprocess` local sockets; binding after `prepare_socket_path`'s stale-socket check
//! (a socket nobody listens on is removed, a live one refused as busy), owner-only socket permissions, the socket
//! file's identity so a server removes only the file it bound, the timed connect (on Windows through the named-pipe
//! API, since the local-socket wrapper ignores its wait mode), the peer-closed probe and the closed-connection error
//! kinds; on Windows the owner-only pipe DACL of `bind_private_local_listener` with the integrity label of
//! `platform/windows.rs` `local_server_security_descriptor`. Changed: the paths come from agterm's namespace, not
//! herdr's session directories and overrides; an over-long Unix socket path is refused before anything is touched;
//! Windows derives the pipe's name from the namespace directory instead of writing a marker file, creates it with
//! `FILE_FLAG_FIRST_PIPE_INSTANCE` (agterm's addition, which `interprocess` sets on a listener's first instance); a
//! Unix server holds the namespace's `flock` from before the stale-socket check for as long as it listens (agterm's
//! addition, so two racing servers cannot both bind); and a client checks that the server runs as its own user.
//!
//! # Namespace
//!
//! A namespace is a directory: on Unix `/tmp/ngterm-live-<hash>`, on Windows
//! `%LOCALAPPDATA%\ngterm\live\<hash>` (Task 21 derives both from the state dir). The endpoint is
//!
//! - Unix: the socket `<directory>/`[`SOCKET_FILE_NAME`], in a directory created owner-only, beside the lock file
//!   [`LOCK_FILE_NAME`] its server holds;
//! - Windows: the pipe `\\.\pipe\ngterm-live-<FNV-1a 64 of the case-folded directory path>` ([`pipe_name`]), so a
//!   client that holds only the directory (`AGTERM_LIVE_DIR`) finds it. A private directory gives a private pipe.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A connection to or from the server.
pub type Stream = interprocess::local_socket::Stream;

/// The server's socket in a Unix namespace directory.
pub const SOCKET_FILE_NAME: &str = "server.sock";

/// The lock file in a Unix namespace directory: its server holds an exclusive `flock` on it while it listens.
pub const LOCK_FILE_NAME: &str = "server.lock";

/// The prefix of a Windows namespace's pipe name.
pub const PIPE_PREFIX: &str = "ngterm-live-";

/// The endpoint of the server of one namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    directory: PathBuf,
    #[cfg(unix)]
    socket: PathBuf,
    #[cfg(windows)]
    pipe: String,
}

impl Endpoint {
    /// The endpoint of the namespace `directory`, made absolute against the current directory first
    /// ([`std::path::absolute`], as the server does with the directory its argv names): a relative directory, or one
    /// with `.` or `..` in it, then names the same pipe on Windows in the client and in the server it starts, whose
    /// working directory differs (`server::working_directory`). A directory that cannot be made absolute (empty, or
    /// relative with no readable current directory) is kept as given.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        let directory = directory.into();
        let directory = std::path::absolute(&directory).unwrap_or(directory);
        Self {
            #[cfg(unix)]
            socket: directory.join(SOCKET_FILE_NAME),
            #[cfg(windows)]
            pipe: pipe_name(&directory),
            directory,
        }
    }

    /// The namespace directory, absolute (see [`Endpoint::new`]).
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// The server's socket file.
    #[cfg(unix)]
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// The server's pipe name, without the `\\.\pipe\` prefix.
    #[cfg(windows)]
    #[must_use]
    pub fn pipe_name(&self) -> &str {
        &self.pipe
    }

    /// Takes the endpoint for a server: creates the namespace directory, then listens. Of any number of servers that
    /// bind one namespace at once, one gets the listener and the rest [`io::ErrorKind::AddrInUse`]: on Unix the
    /// namespace's lock serializes them, on Windows the pipe's first instance.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::AddrInUse`] when a server already listens there or is binding it (on Windows also when another
    /// process holds the pipe's name); [`io::ErrorKind::PermissionDenied`] for a Unix namespace directory that is not
    /// this user's; [`io::ErrorKind::InvalidInput`] for a Unix socket path longer than a socket address holds;
    /// any error creating the directory or the listener.
    pub fn bind(&self) -> io::Result<Listener> {
        sys::bind(self)
    }

    /// Connects to the server, waiting while every pipe instance is busy.
    ///
    /// # Errors
    ///
    /// No server listens ([`io::ErrorKind::NotFound`], or [`io::ErrorKind::ConnectionRefused`] for a stale socket);
    /// the server runs as another user ([`io::ErrorKind::PermissionDenied`]), or its user cannot be read.
    pub fn connect(&self) -> io::Result<Stream> {
        sys::connect(self)
    }

    /// As [`Endpoint::connect`], giving up with [`io::ErrorKind::TimedOut`] (or [`io::ErrorKind::WouldBlock`]) after
    /// `timeout` of a busy server.
    ///
    /// # Errors
    ///
    /// As [`Endpoint::connect`], or the timeout passed.
    pub fn connect_timeout(&self, timeout: Duration) -> io::Result<Stream> {
        sys::connect_timeout(self, timeout)
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(unix)]
        {
            write!(f, "{}", self.socket.display())
        }
        #[cfg(windows)]
        {
            write!(f, r"\\.\pipe\{}", self.pipe)
        }
    }
}

/// A bound endpoint. Dropping it stops listening; on Unix it also removes the socket file, if that is still the one
/// this listener bound (a replacement server may have bound its own since), and then releases the namespace's lock.
#[derive(Debug)]
pub struct Listener {
    inner: interprocess::local_socket::Listener,
    #[cfg(unix)]
    socket: PathBuf,
    #[cfg(unix)]
    identity: sys::SocketFileIdentity,
    /// The namespace's lock, released once the socket file is removed (fields drop after [`Drop::drop`]).
    #[cfg(unix)]
    _lock: std::fs::File,
}

impl Listener {
    /// Waits for the next client.
    ///
    /// # Errors
    ///
    /// Accepting failed.
    pub fn accept(&self) -> io::Result<Stream> {
        use interprocess::local_socket::traits::Listener as _;
        self.inner.accept()
    }
}

#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        // nothing to report to: a socket file left behind is stale, and the next bind removes it
        let _ = sys::remove_socket_file_if_owned(&self.socket, &self.identity);
    }
}

/// Whether the peer of a connection that sends nothing more has gone. Data from it counts as gone too (herdr's
/// probe): the probe is for a connection whose protocol has the peer silent, and it consumes what it reads.
///
/// # Errors
///
/// The probe itself failed.
pub fn peer_closed(stream: &mut Stream) -> io::Result<bool> {
    sys::peer_closed(stream)
}

/// Whether `error` means the peer closed the connection.
#[must_use]
pub fn is_connection_closed_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::WriteZero
    )
}

/// The Windows pipe name of the namespace `directory`, without the `\\.\pipe\` prefix: [`PIPE_PREFIX`] and the FNV-1a
/// 64 hash of the directory's path, case-folded, with `/` as `\` and no trailing separator, so every spelling Windows
/// takes for one directory names one pipe. Defined on every OS, so a namespace's pipe can be named anywhere.
#[must_use]
pub fn pipe_name(directory: &Path) -> String {
    let path = directory.to_string_lossy().replace('/', "\\").to_lowercase();
    let path = path.trim_end_matches('\\');
    format!("{PIPE_PREFIX}{}", fnv1a64(path.as_bytes()))
}

/// The FNV-1a 64 hash of `bytes` as 16 lower-case hex digits (`agterm-core::live::fnv1a64`).
fn fnv1a64(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(14_695_981_039_346_656_037_u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
    });
    format!("{hash:016x}")
}

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_matches_the_reference_vectors() {
        assert_eq!(fnv1a64(b""), "cbf29ce484222325");
        assert_eq!(fnv1a64(b"a"), "af63dc4c8601ec8c");
        assert_eq!(fnv1a64(b"foobar"), "85944171f73967e8");
    }

    #[test]
    fn a_pipe_name_ignores_case_separators_and_a_trailing_separator() {
        let name = pipe_name(Path::new(r"C:\Users\Nick\AppData\Local\ngterm\live\0123456789abcdef"));
        assert_eq!(
            name,
            pipe_name(Path::new("c:/users/nick/appdata/local/ngterm/live/0123456789ABCDEF/"))
        );
        assert!(name.starts_with(PIPE_PREFIX));
        assert_eq!(name.len(), PIPE_PREFIX.len() + 16);
        assert_eq!(
            name,
            format!(
                "{PIPE_PREFIX}{}",
                fnv1a64(br"c:\users\nick\appdata\local\ngterm\live\0123456789abcdef")
            )
        );
    }

    #[test]
    fn a_relative_directory_is_taken_from_the_current_one() {
        let current = std::env::current_dir().unwrap();
        let endpoint = Endpoint::new(Path::new("live").join("0123"));
        assert_eq!(endpoint.directory(), current.join("live").join("0123"));
        assert!(endpoint.directory().is_absolute());
        assert_eq!(endpoint, Endpoint::new(current.join("live").join("0123")));
        // nothing to make absolute: kept as given
        assert_eq!(Endpoint::new("").directory(), Path::new(""));
    }

    /// Every spelling of one directory names the server's pipe, which the server derives from its argv's directory
    /// made absolute: `.` and `..` resolved, the current directory's drive and folder filled in.
    #[cfg(windows)]
    #[test]
    fn every_spelling_of_a_directory_names_one_pipe() {
        let plain = Endpoint::new(r"C:\a\live\0123");
        for spelling in [r"C:\a\b\..\live\0123", r"C:\a\.\live\0123", r"c:/a/live/0123/"] {
            assert_eq!(Endpoint::new(spelling).pipe_name(), plain.pipe_name(), "{spelling}");
        }
        let current = std::env::current_dir().unwrap();
        assert_eq!(
            Endpoint::new(r"live\..\live\0123").pipe_name(),
            pipe_name(&current.join(r"live\0123"))
        );
    }

    #[test]
    fn different_directories_name_different_pipes() {
        assert_ne!(
            pipe_name(Path::new(r"C:\a\live\1")),
            pipe_name(Path::new(r"C:\a\live\2"))
        );
    }
}
