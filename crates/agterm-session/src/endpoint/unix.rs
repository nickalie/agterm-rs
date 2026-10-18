//! The endpoint on Unix: a domain socket in the namespace directory.
//!
//! herdr: `src/ipc.rs` and `src/server/socket_paths.rs` at `HERDR_REV` (`build/pins.env`), licensed under the Apache
//! License 2.0, modified (see the parent module).

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{ConnectOptions, GenericFilePath, ListenerOptions, ToFsName};

use super::{Endpoint, Listener, Stream, is_connection_closed_error};

/// `sockaddr_un`'s `sun_path`, which holds the path and its terminating NUL.
#[cfg(any(target_os = "linux", target_os = "android"))]
const SUN_PATH_LEN: usize = 108;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const SUN_PATH_LEN: usize = 104;

/// The namespace directory's mode: only its owner reaches the socket.
const DIRECTORY_MODE: u32 = 0o700;
/// The socket file's mode (herdr's `SOCKET_PERMISSION_MODE`).
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
    })
}

pub(super) fn connect(endpoint: &Endpoint) -> io::Result<Stream> {
    Stream::connect(endpoint.socket_path().to_fs_name::<GenericFilePath>()?)
}

pub(super) fn connect_timeout(endpoint: &Endpoint, timeout: Duration) -> io::Result<Stream> {
    ConnectOptions::new()
        .name(endpoint.socket_path().to_fs_name::<GenericFilePath>()?)
        .wait_mode(interprocess::ConnectWaitMode::Timeout(timeout))
        .connect_sync()
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

/// herdr's `prepare_socket_path`: a socket file nobody listens on is removed, a live one refused as busy.
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
    fn the_longest_path_a_socket_address_holds_passes() {
        let longest = format!("/{}", "a".repeat(SUN_PATH_LEN - 2));
        assert!(check_socket_path_length(Path::new(&longest)).is_ok());
        let one_more = format!("{longest}a");
        let error = check_socket_path_length(Path::new(&one_more)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
