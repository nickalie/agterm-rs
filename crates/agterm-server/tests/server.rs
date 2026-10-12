//! The control server over real sockets in a temp dir, with the fake main loop standing in for GTK.

use std::error::Error;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, ThreadId};
use std::time::Duration;

use agterm_core::paths::ownership_lock_path;
use agterm_core::{Core, CoreConfig};
use agterm_host::MainLoop;
use agterm_host::fakes::FakeMainLoop;
use agterm_proto::{AppIdentity, MAX_REQUEST_LINE_BYTES, Request, Response, VersionResult};
use agterm_server::{ControlServer, MAX_SOCKET_PATH_BYTES, Reply, ServerError, StreamOwner};
use serde_json::json;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const CLIENT_TIMEOUT: Duration = Duration::from_secs(20);
const VERSION: &[u8] = br#"{"cmd":"version"}"#;

/// A short temp dir: socket paths must stay under 104 bytes whatever `TMPDIR` is.
fn short_dir() -> io::Result<TempDir> {
    tempfile::Builder::new().prefix("agt").tempdir_in("/tmp")
}

fn core() -> Core {
    Core::new(CoreConfig {
        app: AppIdentity::from_recorded("0.1.0", Some("abc1234")),
        capabilities: vec!["posix-shell".to_owned()],
    })
}

/// The app's handler: the core answers on the main loop.
fn core_handler() -> impl Fn(&mut Core, Request) -> Reply + Send + Sync + 'static {
    |core: &mut Core, request: Request| core.dispatch(&request).into()
}

/// A running server over a fake main loop owning `state`.
/// Fields drop top to bottom: the server stops (waking and joining its accept thread through the socket) before
/// the temp dir holding that socket is deleted.
struct Fixture<S: Send + 'static> {
    server: ControlServer,
    main_loop: Arc<FakeMainLoop<S>>,
    socket: PathBuf,
    dir: TempDir,
}

fn start<S, H>(state: S, handler: H) -> TestResult<Fixture<S>>
where
    S: Send + 'static,
    H: Fn(&mut S, Request) -> Reply + Send + Sync + 'static,
{
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let main_loop = Arc::new(FakeMainLoop::spawn(state)?);
    let mut server = ControlServer::new(&socket);
    server.start(Arc::clone(&main_loop) as Arc<dyn MainLoop<S>>, handler)?;
    Ok(Fixture {
        server,
        main_loop,
        socket,
        dir,
    })
}

fn start_core() -> TestResult<Fixture<Core>> {
    start(core(), core_handler())
}

fn connect(socket: &Path) -> io::Result<UnixStream> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    Ok(stream)
}

/// Reads one response line; `None` when the server closed the connection without one.
fn read_response(stream: &UnixStream) -> TestResult<Option<Response>> {
    let mut line = String::new();
    match BufReader::new(stream).read_line(&mut line) {
        Ok(0) => Ok(None),
        // a server closing on unread input resets the connection
        Err(error) if error.kind() == io::ErrorKind::ConnectionReset => Ok(None),
        Ok(_) => Ok(Some(serde_json::from_str(&line)?)),
        Err(error) => Err(error.into()),
    }
}

/// Sends `line` plus a newline on a fresh connection and reads the reply.
fn send(socket: &Path, line: &[u8]) -> TestResult<Option<Response>> {
    let mut stream = connect(socket)?;
    stream.write_all(line)?;
    stream.write_all(b"\n")?;
    read_response(&stream)
}

fn answer(socket: &Path, line: &[u8]) -> TestResult<Response> {
    send(socket, line)?.ok_or_else(|| "no response".into())
}

fn assert_version_ok(socket: &Path) -> TestResult {
    let response = answer(socket, VERSION)?;
    assert!(response.ok, "{response:?}");
    let result: VersionResult = serde_json::from_value(response.result.ok_or("no result")?)?;
    assert_eq!(result.app.version, "0.1.0");
    Ok(())
}

#[test]
fn version_round_trips_through_the_main_loop() -> TestResult {
    let fixture = start_core()?;
    let response = answer(&fixture.socket, VERSION)?;
    assert_eq!(
        serde_json::to_value(&response)?,
        json!({"ok": true, "result": {
            "app": {"version": "0.1.0", "commit": "abc1234"},
            "conformance": {"parityLevel": "0.36.0", "capabilities": ["posix-shell"]}
        }})
    );
    let mode = fs::metadata(&fixture.socket)?.permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    Ok(())
}

#[test]
fn handler_runs_on_the_main_loop_thread() -> TestResult {
    let fixture = start(Vec::<ThreadId>::new(), |seen: &mut Vec<ThreadId>, _| {
        seen.push(thread::current().id());
        Response::success().into()
    })?;
    for _ in 0..3 {
        assert_eq!(answer(&fixture.socket, VERSION)?, Response::success());
    }
    let seen = fixture.main_loop.shutdown().ok_or("main loop lost")?;
    assert_eq!(seen, vec![fixture.main_loop.thread_id(); 3]);
    Ok(())
}

#[test]
fn concurrent_connections_are_served_independently() -> TestResult {
    let fixture = start_core()?;
    // an idle connection holds only its own thread
    let mut idle = connect(&fixture.socket)?;
    let socket = Arc::new(fixture.socket.clone());
    let clients: Vec<_> = (0..8)
        .map(|_| {
            let socket = Arc::clone(&socket);
            thread::spawn(move || answer(&socket, VERSION).map(|r| r.ok).map_err(|e| e.to_string()))
        })
        .collect();
    for client in clients {
        assert_eq!(client.join().map_err(|_| "client panicked")?, Ok(true));
    }
    idle.write_all(VERSION)?;
    idle.write_all(b"\n")?;
    assert!(read_response(&idle)?.ok_or("idle connection got no reply")?.ok);
    Ok(())
}

#[test]
fn malformed_requests_get_the_decode_error() -> TestResult {
    let fixture = start_core()?;
    for line in [
        &b"not json"[..],
        b"",
        b"[]",
        br#"{"cmd":"no.such.command"}"#,
        br#"{"target":"active"}"#,
        br#"{"cmd":"version","target":5}"#,
        br#"{"cmd":"events.read","args":{"limit":"5"}}"#,
    ] {
        let response = answer(&fixture.socket, line)?;
        assert_eq!(response, Response::failure("could not decode request"), "{line:?}");
    }
    assert_version_ok(&fixture.socket)
}

#[test]
fn unimplemented_command_is_refused_by_name() -> TestResult {
    let fixture = start_core()?;
    let response = answer(&fixture.socket, br#"{"cmd":"session.new","args":{"name":"x"}}"#)?;
    assert_eq!(response, Response::failure("session.new is not supported by this host"));
    Ok(())
}

#[test]
fn request_without_trailing_newline_is_answered_at_eof() -> TestResult {
    let fixture = start_core()?;
    let mut stream = connect(&fixture.socket)?;
    stream.write_all(VERSION)?;
    stream.shutdown(Shutdown::Write)?;
    assert!(read_response(&stream)?.ok_or("no reply")?.ok);
    Ok(())
}

#[test]
fn request_line_limit_is_inclusive() -> TestResult {
    let fixture = start_core()?;
    let head = br#"{"cmd":"version","target":""#;
    let tail = br#""}"#;
    let mut line = head.to_vec();
    line.resize(MAX_REQUEST_LINE_BYTES - tail.len(), b'a');
    line.extend_from_slice(tail);
    assert_eq!(line.len(), MAX_REQUEST_LINE_BYTES);
    assert!(answer(&fixture.socket, &line)?.ok);

    let stream = connect(&fixture.socket)?;
    let writer = {
        let mut stream = stream.try_clone()?;
        // the server stops reading at the cap, so this write may fail with a broken pipe
        thread::spawn(move || stream.write_all(&vec![b'a'; MAX_REQUEST_LINE_BYTES + 1]).is_ok())
    };
    assert_eq!(read_response(&stream)?, None);
    writer.join().map_err(|_| "writer panicked")?;
    stream.shutdown(Shutdown::Both).ok();
    assert_version_ok(&fixture.socket)
}

fn socket_path_of(dir: &Path, len: usize) -> TestResult<PathBuf> {
    let base = dir.as_os_str().len() + 1;
    let name = "s".repeat(len.checked_sub(base).ok_or("temp dir too long")?);
    Ok(dir.join(name))
}

#[test]
fn socket_paths_of_104_bytes_or_more_are_refused() -> TestResult {
    let dir = short_dir()?;
    let main_loop: Arc<dyn MainLoop<Core>> = Arc::new(FakeMainLoop::spawn(core())?);

    let too_long = socket_path_of(dir.path(), MAX_SOCKET_PATH_BYTES)?;
    let mut server = ControlServer::new(&too_long);
    let error = server.start(Arc::clone(&main_loop), core_handler()).err();
    assert!(
        matches!(error, Some(ServerError::PathTooLong { len: 104, .. })),
        "{error:?}"
    );
    assert!(!server.is_running());
    assert!(!too_long.exists());

    let longest = socket_path_of(dir.path(), MAX_SOCKET_PATH_BYTES - 1)?;
    let mut server = ControlServer::new(&longest);
    server.start(main_loop, core_handler())?;
    assert_version_ok(&longest)
}

#[test]
fn panicking_handler_answers_and_keeps_serving() -> TestResult {
    let fixture = start(core(), |core: &mut Core, request: Request| {
        assert!(request.cmd != agterm_proto::Command::Tree, "handler blew up");
        core.dispatch(&request).into()
    })?;
    let response = answer(&fixture.socket, br#"{"cmd":"tree"}"#)?;
    assert_eq!(response, Response::failure("internal error handling tree"));
    assert_version_ok(&fixture.socket)?;
    assert!(
        fixture.main_loop.shutdown().is_some(),
        "the main loop died with the handler"
    );
    Ok(())
}

#[test]
fn closed_main_loop_answers_shutting_down() -> TestResult {
    let fixture = start_core()?;
    fixture.main_loop.shutdown().ok_or("main loop lost")?;
    let response = answer(&fixture.socket, VERSION)?;
    assert_eq!(response, Response::failure("the app is shutting down"));
    Ok(())
}

#[test]
fn second_server_refuses_and_the_first_survives() -> TestResult {
    let fixture = start_core()?;
    let lock = ownership_lock_path(&fixture.socket);
    assert!(!fixture.server.is_refused());
    assert_eq!(fixture.server.advertised_path(), fixture.socket);

    let mut second = ControlServer::new(&fixture.socket);
    assert!(second.is_refused());
    assert_eq!(
        second.advertised_path(),
        dir_join(&fixture.dir, "agterm.sock.unavailable")
    );
    let main_loop: Arc<dyn MainLoop<Core>> = Arc::new(FakeMainLoop::spawn(core())?);
    let error = second.start(main_loop, core_handler()).err();
    assert!(matches!(error, Some(ServerError::Refused(_))), "{error:?}");
    assert!(!second.is_running());
    drop(second);

    assert!(fixture.socket.exists());
    assert!(lock.exists());
    assert_version_ok(&fixture.socket)?;
    assert!(
        ControlServer::new(&fixture.socket).is_refused(),
        "the first server lost its lock"
    );
    Ok(())
}

fn dir_join(dir: &TempDir, name: &str) -> PathBuf {
    dir.path().join(name)
}

#[test]
fn stop_removes_the_socket_and_releases_the_lock() -> TestResult {
    let mut fixture = start_core()?;
    fixture.server.stop();
    assert!(!fixture.server.is_running());
    assert!(!fixture.socket.exists());
    assert!(ownership_lock_path(&fixture.socket).exists(), "the lock file must stay");

    let mut next = ControlServer::new(&fixture.socket);
    assert!(!next.is_refused());
    next.start(
        Arc::clone(&fixture.main_loop) as Arc<dyn MainLoop<Core>>,
        core_handler(),
    )?;
    assert_version_ok(&fixture.socket)
}

#[test]
fn stale_socket_file_is_replaced() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    fs::write(&socket, b"left over by a crashed instance")?;
    let mut server = ControlServer::new(&socket);
    server.start(
        Arc::new(FakeMainLoop::spawn(core())?) as Arc<dyn MainLoop<Core>>,
        core_handler(),
    )?;
    assert_version_ok(&socket)
}

/// Echoes everything the client sends after the request line, then closes.
struct Echo;

impl StreamOwner for Echo {
    fn adopt(self: Box<Self>, mut stream: UnixStream, pending: Vec<u8>) {
        let mut rest = pending;
        if stream.read_to_end(&mut rest).is_ok() {
            let mut line = b"echo:".to_vec();
            line.extend_from_slice(&rest);
            line.push(b'\n');
            stream.write_all(&line).ok();
        }
    }
}

/// Records that it was told it will never get the connection.
struct Abandoned(Arc<AtomicBool>);

impl StreamOwner for Abandoned {
    fn adopt(self: Box<Self>, _stream: UnixStream, _pending: Vec<u8>) {}

    fn abandon(self: Box<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn stream_owner_takes_only_an_ok_connection() -> TestResult {
    let abandoned = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&abandoned);
    let fixture = start((), move |(), request: Request| {
        if request.cmd == agterm_proto::Command::Version {
            Reply::streaming(Response::success(), Box::new(Echo))
        } else {
            Reply::streaming(Response::failure("refused"), Box::new(Abandoned(Arc::clone(&flag))))
        }
    })?;

    let mut stream = connect(&fixture.socket)?;
    stream.write_all(VERSION)?;
    stream.write_all(b"\nframe-1 frame-2")?;
    stream.shutdown(Shutdown::Write)?;
    let mut reader = BufReader::new(&stream);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    assert_eq!(serde_json::from_str::<Response>(&first)?, Response::success());
    let mut second = String::new();
    reader.read_line(&mut second)?;
    assert_eq!(second, "echo:frame-1 frame-2\n");

    let response = answer(&fixture.socket, br#"{"cmd":"tree"}"#)?;
    assert_eq!(response, Response::failure("refused"));
    // the owner hears of it right after the response is written
    let deadline = std::time::Instant::now() + CLIENT_TIMEOUT;
    while !abandoned.load(Ordering::SeqCst) {
        assert!(std::time::Instant::now() < deadline, "the owner was never abandoned");
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
