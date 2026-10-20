//! The endpoint on Unix: a domain socket in the namespace directory.
//!
//! herdr: `src/ipc.rs` and `src/server/socket_paths.rs` at `HERDR_REV` (`build/pins.env`), licensed under the Apache
//! License 2.0, modified (see the parent module).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use interprocess::local_socket::traits::{Stream as _, StreamCommon as _};
use interprocess::local_socket::{ConnectOptions, GenericFilePath, ListenerOptions, ToFsName};

use super::{Endpoint, LOCK_FILE_NAME, Listener, Stream, is_connection_closed_error};

/// `sockaddr_un`'s `sun_path`, which holds the path and its terminating NUL.
#[cfg(any(target_os = "linux", target_os = "android"))]
const SUN_PATH_LEN: usize = 108;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const SUN_PATH_LEN: usize = 104;

/// The namespace directory's mode: only its owner reaches the socket.
const DIRECTORY_MODE: u32 = 0o700;
/// The socket file's mode (herdr's `SOCKET_PERMISSION_MODE`), and the lock file's.
const SOCKET_MODE: u32 = 0o600;

/// A socket file as bound: its device and inode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SocketFileIdentity {
    dev: u64,
    ino: u64,
}

pub(super) fn bind(endpoint: &Endpoint) -> io::Result<Listener> {
    let socket = endpoint.socket_path();
    check_socket_path_length(socket)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(DIRECTORY_MODE)
        .create(endpoint.directory())?;
    make_directory_private(endpoint.directory())?;
    let lock = lock_namespace(endpoint)?;
    prepare_socket_path(socket)?;
    let inner = ListenerOptions::new()
        .name(socket.to_fs_name::<GenericFilePath>()?)
        .reclaim_name(false)
        .create_sync()?;
    fs::set_permissions(socket, fs::Permissions::from_mode(SOCKET_MODE))?;
    Ok(Listener {
        inner,
        socket: socket.to_path_buf(),
        identity: socket_file_identity(socket)?,
        owns_socket: AtomicBool::new(true),
        lock,
    })
}

pub(super) fn adopt(endpoint: &Endpoint, listener: OwnedFd, lock: OwnedFd) -> io::Result<Listener> {
    let socket = endpoint.socket_path();
    let inner = interprocess::os::unix::uds_local_socket::Listener::from(listener);
    Ok(Listener {
        inner: inner.into(),
        socket: socket.to_path_buf(),
        identity: socket_file_identity(socket)?,
        owns_socket: AtomicBool::new(false),
        lock: File::from(lock),
    })
}

pub(super) fn accept(listener: &Listener) -> io::Result<Stream> {
    use interprocess::local_socket::traits::Listener as _;
    listener.inner.accept()
}

pub(super) fn connect(endpoint: &Endpoint) -> io::Result<Stream> {
    let stream = Stream::connect(endpoint.socket_path().to_fs_name::<GenericFilePath>()?)?;
    check_server_user(&stream, endpoint)?;
    Ok(stream)
}

pub(super) fn connect_timeout(endpoint: &Endpoint, timeout: Duration) -> io::Result<Stream> {
    let stream = ConnectOptions::new()
        .name(endpoint.socket_path().to_fs_name::<GenericFilePath>()?)
        .wait_mode(interprocess::ConnectWaitMode::Timeout(timeout))
        .connect_sync()?;
    check_server_user(&stream, endpoint)?;
    Ok(stream)
}

/// Takes the namespace's exclusive, non-blocking `flock` (agterm's addition to herdr's bind), which the listener holds
/// for its whole life: the stale-socket check, the removal and the bind that follow are one server's alone, so two
/// racing servers cannot both find the socket stale and the second unlink the first's live one. The lock file stays
/// behind: removing it would let a third server lock a fresh file while the second still holds the old one. Only this
/// user writes in the directory ([`make_directory_private`]), so nobody else can plant the file.
fn lock_namespace(endpoint: &Endpoint) -> io::Result<File> {
    let path = endpoint.directory().join(LOCK_FILE_NAME);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(SOCKET_MODE)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "agterm-session server is already running (namespace locked at {})",
                path.display()
            ),
        )),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Refuses a server of another user: the namespace's path is derived, so another user could have bound a socket there
/// before this user's server made the directory. The server's effective user at its `listen` decides, as the server's
/// account does for the Windows client.
fn check_server_user(stream: &Stream, endpoint: &Endpoint) -> io::Result<()> {
    let server = stream.peer_creds()?.euid();
    check_peer_user(server, rustix::process::geteuid().as_raw(), endpoint)
}

fn check_peer_user(server: Option<u32>, user: u32, endpoint: &Endpoint) -> io::Result<()> {
    match server {
        Some(server) if server == user => Ok(()),
        Some(server) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the socket at {endpoint} belongs to a server of another user (uid {server}, this user {user})"),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the user of the server at {endpoint} is unknown"),
        )),
    }
}

/// Refuses a path a socket address cannot hold with its NUL, before anything is created or removed.
fn check_socket_path_length(socket: &Path) -> io::Result<()> {
    let length = socket.as_os_str().len();
    if length < SUN_PATH_LEN {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "socket path is {length} bytes, longer than the {} a Unix socket address holds: {}",
            SUN_PATH_LEN - 1,
            socket.display()
        ),
    ))
}

/// Makes sure only this user reaches the namespace directory. Its name is derived, so another user could have made
/// it first: anything but a directory of this user's is refused, and one of this user's that others can reach is
/// closed to them.
fn make_directory_private(directory: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    let user = rustix::process::geteuid().as_raw();
    if !metadata.is_dir() || metadata.uid() != user {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "the namespace directory {} is not a directory of this user's (owner {}, this user {user})",
                directory.display(),
                metadata.uid()
            ),
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        fs::set_permissions(directory, fs::Permissions::from_mode(DIRECTORY_MODE))?;
    }
    Ok(())
}

/// herdr's `prepare_socket_path`: a socket file nobody listens on is removed, a live one refused as busy. It runs under
/// the namespace's lock ([`lock_namespace`]), so no other server is between its check and the bind.
fn prepare_socket_path(socket: &Path) -> io::Result<()> {
    if fs::symlink_metadata(socket).is_err() {
        return Ok(());
    }
    match Stream::connect(socket.to_fs_name::<GenericFilePath>()?) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "agterm-session server is already running (socket busy at {})",
                    socket.display()
                ),
            ));
        }
        Err(error) if stale_socket_connect_error(error.kind()) => {}
        Err(error) => return Err(error),
    }
    match fs::remove_file(socket) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn stale_socket_connect_error(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound | io::ErrorKind::TimedOut
    )
}

fn socket_file_identity(socket: &Path) -> io::Result<SocketFileIdentity> {
    let metadata = fs::symlink_metadata(socket)?;
    Ok(SocketFileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

pub(super) fn remove_socket_file_if_owned(socket: &Path, identity: &SocketFileIdentity) -> io::Result<()> {
    match socket_file_identity(socket) {
        Ok(current) if current == *identity => {}
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => return Ok(()),
    }
    match fs::remove_file(socket) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Shut the socket down both ways: a blocked read returns its end, a write fails, the peer reads its end.
pub(super) fn disconnect(stream: &Stream) {
    let Stream::UdSocket(socket) = stream;
    // a socket already shut down or reset reports it, and stays closed
    let _ = socket.inner().shutdown(std::net::Shutdown::Both);
}

/// What was written is in the peer's receive queue already, which the shutdown leaves for it to read before its end.
pub(super) fn close(stream: &Stream) {
    disconnect(stream);
}

pub(super) fn peer_closed(stream: &mut Stream) -> io::Result<bool> {
    stream.set_nonblocking(true)?;
    let mut probe = [0_u8; 1];
    let status = match stream.read(&mut probe) {
        Ok(_) => Ok(true),
        Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => Ok(false),
        Err(error) if is_connection_closed_error(&error) => Ok(true),
        Err(error) => Err(error),
    };
    stream.set_nonblocking(false)?;
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_socket_errors_are_refused_connections_missing_files_and_timeouts() {
        assert!(stale_socket_connect_error(io::ErrorKind::ConnectionRefused));
        assert!(stale_socket_connect_error(io::ErrorKind::NotFound));
        assert!(stale_socket_connect_error(io::ErrorKind::TimedOut));
        assert!(!stale_socket_connect_error(io::ErrorKind::WouldBlock));
        assert!(!stale_socket_connect_error(io::ErrorKind::PermissionDenied));
    }

    #[test]
    fn only_a_server_of_this_user_is_accepted() {
        let endpoint = Endpoint::new("/tmp/ngterm-live-test");
        assert!(check_peer_user(Some(1000), 1000, &endpoint).is_ok());
        let error = check_peer_user(Some(1001), 1000, &endpoint).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            error.to_string().contains("another user (uid 1001, this user 1000)"),
            "{error}"
        );
        let error = check_peer_user(None, 1000, &endpoint).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn the_longest_path_a_socket_address_holds_passes() {
        let longest = format!("/{}", "a".repeat(SUN_PATH_LEN - 2));
        assert!(check_socket_path_length(Path::new(&longest)).is_ok());
        let one_more = format!("{longest}a");
        let error = check_socket_path_length(Path::new(&one_more)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
