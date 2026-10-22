//! The endpoint on a real socket or pipe, in a private temporary namespace: a server and a client exchange frames,
//! one endpoint has one server, and what is left behind is cleaned up or refused.
//!
//! herdr: the tests of `src/ipc.rs` and `src/server/socket_paths.rs` at `HERDR_REV` (`build/pins.env`), licensed
//! under the Apache License 2.0, modified: ngterm's namespace and its checks.

use std::io;
use std::thread;
use std::time::Duration;

use ngterm_session::endpoint::{Endpoint, Stream, is_connection_closed_error, peer_closed};
use ngterm_session::protocol::{
    AttachClientMessage, Control, FramingError, Hello, PROTOCOL_VERSION, Role, read_frame, write_control, write_message,
};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A namespace directory, not created yet, inside a private temporary directory.
fn namespace() -> TestResult<(TempDir, Endpoint)> {
    let root = tempfile::tempdir()?;
    let endpoint = Endpoint::new(root.path().join("live"));
    Ok((root, endpoint))
}

/// A connected pair: the client's stream and the server's accepted one.
fn connected(endpoint: &Endpoint) -> TestResult<(Stream, Stream)> {
    let listener = endpoint.bind()?;
    let accept = thread::spawn(move || listener.accept());
    let client = endpoint.connect()?;
    let server = accept.join().map_err(|_| "the accepting thread panicked")??;
    Ok((client, server))
}

#[test]
fn a_client_and_the_server_exchange_frames() {
    let (_root, endpoint) = namespace().unwrap();
    let listener = endpoint.bind().unwrap();
    let server = thread::spawn(move || {
        let mut stream = listener.accept().unwrap();
        let hello = read_frame(&mut stream).unwrap().control().unwrap();
        let input = read_frame(&mut stream)
            .unwrap()
            .message::<AttachClientMessage>(PROTOCOL_VERSION)
            .unwrap();
        write_message(&mut stream, &AttachClientMessage::Detach).unwrap();
        (hello, input)
    });
    let mut client = endpoint.connect().unwrap();
    let hello = Control::Hello(Hello {
        role: Role::Attach,
        build: "test".into(),
        capabilities: Vec::new(),
    });
    write_control(&mut client, &hello).unwrap();
    let input = AttachClientMessage::Input { data: b"ls\r".to_vec() };
    write_message(&mut client, &input).unwrap();
    let reply = read_frame(&mut client).unwrap();
    assert_eq!(
        reply.message::<AttachClientMessage>(PROTOCOL_VERSION).unwrap(),
        AttachClientMessage::Detach
    );
    assert_eq!(server.join().unwrap(), (hello, input));
    assert!(matches!(read_frame(&mut client), Err(FramingError::Closed)));
}

#[test]
fn a_timed_connect_reaches_the_server() {
    let (_root, endpoint) = namespace().unwrap();
    let listener = endpoint.bind().unwrap();
    let accept = thread::spawn(move || listener.accept().map(drop));
    // held until the accept returns: a Windows listener discards a connection that ended before it was accepted and
    // waits for the next one
    let _client = endpoint.connect_timeout(Duration::from_secs(5)).unwrap();
    accept.join().unwrap().unwrap();
}

#[test]
fn nothing_listening_is_not_found() {
    let (_root, endpoint) = namespace().unwrap();
    let error = endpoint.connect().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
    let error = endpoint.connect_timeout(Duration::from_millis(200)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
}

#[test]
fn a_second_server_under_a_taken_name_is_refused() {
    let (_root, endpoint) = namespace().unwrap();
    let first = endpoint.bind().unwrap();
    let error = Endpoint::new(endpoint.directory()).bind().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AddrInUse, "{error}");
    assert!(error.to_string().contains("already running"), "{error}");
    drop(first);
    // the name is free again once its server is gone
    let again = endpoint.bind().unwrap();
    let accept = thread::spawn(move || again.accept().map(drop));
    let _client = endpoint.connect().unwrap();
    accept.join().unwrap().unwrap();
}

#[test]
fn an_idle_peer_is_open_and_a_dropped_one_closed() {
    let (_root, endpoint) = namespace().unwrap();
    let (client, mut server) = connected(&endpoint).unwrap();
    assert!(!peer_closed(&mut server).unwrap());
    drop(client);
    assert!(peer_closed(&mut server).unwrap());
}

#[test]
fn closed_connection_errors_are_recognized() {
    for kind in [
        io::ErrorKind::BrokenPipe,
        io::ErrorKind::ConnectionAborted,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::NotConnected,
        io::ErrorKind::UnexpectedEof,
        io::ErrorKind::WriteZero,
    ] {
        assert!(is_connection_closed_error(&io::Error::from(kind)), "{kind:?}");
    }
    assert!(!is_connection_closed_error(&io::Error::from(io::ErrorKind::TimedOut)));
}

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Barrier};

    use ngterm_session::endpoint::LOCK_FILE_NAME;

    use super::*;

    #[test]
    fn the_socket_lives_in_an_owner_only_directory() {
        let (_root, endpoint) = namespace().unwrap();
        let _listener = endpoint.bind().unwrap();
        assert_eq!(endpoint.socket_path(), endpoint.directory().join("server.sock"));
        assert_eq!(endpoint.to_string(), endpoint.socket_path().display().to_string());
        let mode = |path: &std::path::Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(endpoint.directory()), 0o700);
        assert_eq!(mode(endpoint.socket_path()), 0o600);
        assert_eq!(mode(&endpoint.directory().join(LOCK_FILE_NAME)), 0o600);
    }

    #[test]
    fn a_directory_others_reach_is_closed_to_them() {
        let (_root, endpoint) = namespace().unwrap();
        fs::create_dir(endpoint.directory()).unwrap();
        fs::set_permissions(endpoint.directory(), fs::Permissions::from_mode(0o755)).unwrap();
        let _listener = endpoint.bind().unwrap();
        let mode = fs::metadata(endpoint.directory()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn a_symlinked_namespace_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("elsewhere");
        fs::create_dir(&target).unwrap();
        let link = root.path().join("live");
        symlink(&target, &link).unwrap();
        let error = Endpoint::new(&link).bind().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        assert!(!target.join("server.sock").exists());
    }

    #[test]
    fn an_over_long_socket_path_is_refused_before_anything_is_created() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("d".repeat(120));
        let error = Endpoint::new(&directory).bind().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
        assert!(error.to_string().contains("longer than the"), "{error}");
        assert!(!directory.exists());
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let (_root, endpoint) = namespace().unwrap();
        fs::create_dir(endpoint.directory()).unwrap();
        drop(UnixListener::bind(endpoint.socket_path()).unwrap());
        assert!(endpoint.socket_path().exists());
        let listener = endpoint.bind().unwrap();
        let accept = thread::spawn(move || listener.accept().map(drop));
        let _client = endpoint.connect().unwrap();
        accept.join().unwrap().unwrap();
    }

    #[test]
    fn a_server_that_holds_the_lock_keeps_the_namespace() {
        // a racing server between its lock and its listen: its socket is not there or not listening yet, so the
        // probe alone would find a stale socket and remove it
        let (_root, endpoint) = namespace().unwrap();
        fs::create_dir(endpoint.directory()).unwrap();
        drop(UnixListener::bind(endpoint.socket_path()).unwrap());
        let lock = fs::File::create(endpoint.directory().join(LOCK_FILE_NAME)).unwrap();
        lock.try_lock().unwrap();
        let error = endpoint.bind().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse, "{error}");
        assert!(error.to_string().contains("already running"), "{error}");
        assert!(endpoint.socket_path().exists(), "the lock holder's socket was removed");
        drop(lock);
        let _listener = endpoint.bind().unwrap();
    }

    #[test]
    fn racing_servers_over_a_stale_socket_leave_one_listening() {
        const SERVERS: usize = 8;
        let (_root, endpoint) = namespace().unwrap();
        fs::create_dir(endpoint.directory()).unwrap();
        drop(UnixListener::bind(endpoint.socket_path()).unwrap());
        let start = Arc::new(Barrier::new(SERVERS));
        let racers: Vec<_> = (0..SERVERS)
            .map(|_| {
                let endpoint = endpoint.clone();
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    endpoint.bind()
                })
            })
            .collect();
        let mut listeners = Vec::new();
        for racer in racers {
            match racer.join().unwrap() {
                Ok(listener) => listeners.push(listener),
                Err(error) => assert_eq!(error.kind(), io::ErrorKind::AddrInUse, "{error}"),
            }
        }
        assert_eq!(listeners.len(), 1, "every racer but one is refused");
        let listener = listeners.pop().unwrap();
        let accept = thread::spawn(move || listener.accept().map(drop));
        let _client = endpoint.connect().unwrap();
        accept.join().unwrap().unwrap();
    }

    #[test]
    fn the_server_removes_only_the_socket_it_bound() {
        let (_root, endpoint) = namespace().unwrap();
        drop(endpoint.bind().unwrap());
        assert!(!endpoint.socket_path().exists());

        let listener = endpoint.bind().unwrap();
        fs::remove_file(endpoint.socket_path()).unwrap();
        fs::write(endpoint.socket_path(), b"a replacement's").unwrap();
        drop(listener);
        assert_eq!(fs::read(endpoint.socket_path()).unwrap(), b"a replacement's");
    }
}

#[cfg(windows)]
mod windows {
    use interprocess::os::windows::named_pipe::{PipeListenerOptions, pipe_mode};
    use ngterm_session::endpoint::{PIPE_PREFIX, pipe_name};

    use super::*;

    #[test]
    fn the_pipe_is_named_after_the_namespace_directory() {
        let (_root, endpoint) = namespace().unwrap();
        assert_eq!(endpoint.pipe_name(), pipe_name(endpoint.directory()));
        assert!(endpoint.pipe_name().starts_with(PIPE_PREFIX));
        assert_eq!(endpoint.to_string(), format!(r"\\.\pipe\{}", endpoint.pipe_name()));
        let _listener = endpoint.bind().unwrap();
        assert!(endpoint.directory().is_dir());
    }

    #[test]
    fn a_pipe_another_listener_holds_is_refused() {
        let (_root, endpoint) = namespace().unwrap();
        let path = format!(r"\\.\pipe\{}", endpoint.pipe_name());
        let _squatter = PipeListenerOptions::new()
            .path(path.as_str())
            .create_duplex::<pipe_mode::Bytes>()
            .unwrap();
        let error = endpoint.bind().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse, "{error}");
    }
}
