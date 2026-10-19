//! The control server over real sockets in a temp dir, with the fake main loop standing in for the frontend.

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
use std::time::{Duration, Instant};

use agterm_core::paths::ownership_lock_path;
use agterm_core::{Core, CoreConfig, CoreState, decode_request};
use agterm_host::MainLoop;
use agterm_host::fakes::FakeMainLoop;
use agterm_proto::{AppIdentity, MAX_REQUEST_LINE_BYTES, Request, Response, VersionResult};
use agterm_server::{ConnectionDeadlines, ControlServer, MAX_SOCKET_PATH_BYTES, Reply, ServerError, StreamOwner};
use serde_json::json;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const CLIENT_TIMEOUT: Duration = Duration::from_secs(20);
const VERSION: &[u8] = br#"{"cmd":"version"}"#;
/// Upstream's answer to a request line that cannot be read.
const UNREADABLE: &str = "request too large or read failed";
/// A response well over what an `AF_UNIX` socket buffers, so writing it blocks on a client that does not read.
const BIG_RESPONSE_BYTES: usize = 4 << 20;

/// A short temp dir: socket paths must stay under 104 bytes whatever `TMPDIR` is.
fn short_dir() -> io::Result<TempDir> {
    tempfile::Builder::new().prefix("agt").tempdir_in("/tmp")
}

/// The app's core, which holds its terminals and so stays on the thread that built it: a fake main loop builds it.
fn core() -> Core {
    Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", Some("abc1234")),
            capabilities: vec!["posix-shell".to_owned()],
        },
        CoreState::default(),
    )
}

fn core_loop() -> io::Result<FakeMainLoop<Core>> {
    FakeMainLoop::spawn_with(core)
}

/// The app's handler: the core answers on the main loop.
fn core_handler() -> impl Fn(&mut Core, Request) -> Reply + Send + Sync + 'static {
    |core: &mut Core, request: Request| core.dispatch(&request).into()
}

/// A running server over a fake main loop owning `state`.
/// Fields drop top to bottom: the server stops (waking and joining its accept thread through the socket) before
/// the temp dir holding that socket is deleted.
struct Fixture<S: 'static> {
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
    start_with(state, handler, ConnectionDeadlines::default())
}

fn start_with<S, H>(state: S, handler: H, deadlines: ConnectionDeadlines) -> TestResult<Fixture<S>>
where
    S: Send + 'static,
    H: Fn(&mut S, Request) -> Reply + Send + Sync + 'static,
{
    start_on(FakeMainLoop::spawn(state)?, handler, deadlines)
}

fn start_on<S, H>(main_loop: FakeMainLoop<S>, handler: H, deadlines: ConnectionDeadlines) -> TestResult<Fixture<S>>
where
    S: 'static,
    H: Fn(&mut S, Request) -> Reply + Send + Sync + 'static,
{
    let dir = short_dir()?;
    let socket = dir.path().join("ngterm.sock");
    let main_loop = Arc::new(main_loop);
    let mut server = ControlServer::new(&socket).with_deadlines(deadlines);
    server.start(Arc::clone(&main_loop) as Arc<dyn MainLoop<S>>, handler)?;
    Ok(Fixture {
        server,
        main_loop,
        socket,
        dir,
    })
}

fn start_core() -> TestResult<Fixture<Core>> {
    start_on(core_loop()?, core_handler(), ConnectionDeadlines::default())
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
        let error = decode_request(line).err().ok_or("decoded")?;
        assert_eq!(response, Response::from(error), "{line:?}");
        let text = response.error.ok_or("no error text")?;
        assert!(text.starts_with("invalid request: "), "{text}");
    }
    assert_eq!(
        answer(&fixture.socket, br#"{"cmd":"session.nope"}"#)?,
        Response::failure("invalid request: Cannot initialize Command from invalid String value session.nope")
    );
    assert_version_ok(&fixture.socket)
}

#[test]
fn a_refusal_is_answered_with_its_reason() -> TestResult {
    let fixture = start_core()?;
    let response = answer(&fixture.socket, br#"{"cmd":"live.attach"}"#)?;
    assert_eq!(response, Response::failure("live.attach requires a host"));
    // upstream's name for the group is no command of this build (conformance/docs/rust-build.md, The `live` group)
    assert_eq!(
        answer(&fixture.socket, br#"{"cmd":"zmx.attach"}"#)?,
        Response::failure("invalid request: Cannot initialize Command from invalid String value zmx.attach")
    );
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
    assert_eq!(read_response(&stream)?, Some(Response::failure(UNREADABLE)));
    writer.join().map_err(|_| "writer panicked")?;
    stream.shutdown(Shutdown::Both).ok();
    assert_version_ok(&fixture.socket)
}

#[test]
fn empty_connection_is_answered_before_it_closes() -> TestResult {
    let fixture = start_core()?;
    let stream = connect(&fixture.socket)?;
    stream.shutdown(Shutdown::Write)?;
    assert_eq!(read_response(&stream)?, Some(Response::failure(UNREADABLE)));
    assert_version_ok(&fixture.socket)
}

#[test]
fn idle_connection_is_answered_at_the_read_timeout() -> TestResult {
    let fixture = start_on(
        core_loop()?,
        core_handler(),
        ConnectionDeadlines {
            read_timeout: Duration::from_millis(100),
            ..ConnectionDeadlines::default()
        },
    )?;
    let started = Instant::now();
    let mut stream = connect(&fixture.socket)?;
    stream.write_all(br#"{"cmd":"#)?;
    assert_eq!(read_response(&stream)?, Some(Response::failure(UNREADABLE)));
    // well before the 10 s overall deadline
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    Ok(())
}

#[test]
fn trickle_past_the_read_deadline_is_answered() -> TestResult {
    // the per-read timeout never trips on a byte every 20 ms; only the overall deadline ends the read
    let fixture = start_on(
        core_loop()?,
        core_handler(),
        ConnectionDeadlines {
            read_timeout: Duration::from_secs(30),
            read_deadline: Duration::from_millis(300),
            ..ConnectionDeadlines::default()
        },
    )?;
    let started = Instant::now();
    let stream = connect(&fixture.socket)?;
    let done = Arc::new(AtomicBool::new(false));
    let trickle = {
        let mut stream = stream.try_clone()?;
        let done = Arc::clone(&done);
        thread::spawn(move || {
            while !done.load(Ordering::SeqCst) && stream.write_all(b" ").is_ok() {
                thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let response = read_response(&stream);
    done.store(true, Ordering::SeqCst);
    trickle.join().map_err(|_| "trickle panicked")?;
    assert_eq!(response?, Some(Response::failure(UNREADABLE)));
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    Ok(())
}

/// A handler whose `ok` reply is too big to fit the socket buffer and claims the connection, so a write that never
/// completes shows as the owner being abandoned.
fn start_big_reply(deadlines: ConnectionDeadlines, abandoned: &Arc<AtomicBool>) -> TestResult<Fixture<()>> {
    let flag = Arc::clone(abandoned);
    start_with(
        (),
        move |(), _: Request| {
            let response = Response::with_result(json!("x".repeat(BIG_RESPONSE_BYTES)));
            Reply::streaming(response, Box::new(Abandoned(Arc::clone(&flag))))
        },
        deadlines,
    )
}

/// Waits until `flag` is set; the time it took.
fn wait_for(flag: &AtomicBool) -> TestResult<Duration> {
    let started = Instant::now();
    while !flag.load(Ordering::SeqCst) {
        if started.elapsed() > CLIENT_TIMEOUT {
            return Err("the flag was never set".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(started.elapsed())
}

#[test]
fn stalled_reader_times_the_response_write_out() -> TestResult {
    let abandoned = Arc::new(AtomicBool::new(false));
    let fixture = start_big_reply(
        ConnectionDeadlines {
            write_timeout: Duration::from_millis(100),
            write_deadline: Duration::from_secs(30),
            ..ConnectionDeadlines::default()
        },
        &abandoned,
    )?;
    let mut stream = connect(&fixture.socket)?;
    stream.write_all(VERSION)?;
    stream.write_all(b"\n")?;
    // the client never reads; one write stalls past its timeout, long before the overall deadline
    let waited = wait_for(&abandoned)?;
    assert!(waited < Duration::from_secs(10), "{waited:?}");
    Ok(())
}

#[test]
fn slow_drip_reader_hits_the_write_deadline() -> TestResult {
    let abandoned = Arc::new(AtomicBool::new(false));
    let fixture = start_big_reply(
        ConnectionDeadlines {
            write_timeout: Duration::from_secs(30),
            write_deadline: Duration::from_millis(300),
            ..ConnectionDeadlines::default()
        },
        &abandoned,
    )?;
    let mut stream = connect(&fixture.socket)?;
    stream.write_all(VERSION)?;
    stream.write_all(b"\n")?;
    // every write makes progress, so only the overall deadline ends the response
    let drip = {
        let mut stream = stream.try_clone()?;
        thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            let mut total = 0;
            while let Ok(read @ 1..) = stream.read(&mut chunk) {
                total += read;
                thread::sleep(Duration::from_millis(10));
            }
            total
        })
    };
    let waited = wait_for(&abandoned)?;
    assert!(waited < Duration::from_secs(10), "{waited:?}");
    let drained = drip.join().map_err(|_| "drip panicked")?;
    assert!(drained < BIG_RESPONSE_BYTES, "{drained}");
    Ok(())
}

fn socket_path_of(dir: &Path, len: usize) -> TestResult<PathBuf> {
    let base = dir.as_os_str().len() + 1;
    let name = "s".repeat(len.checked_sub(base).ok_or("temp dir too long")?);
    Ok(dir.join(name))
}

#[test]
fn socket_paths_of_104_bytes_or_more_are_refused() -> TestResult {
    let dir = short_dir()?;
    let main_loop: Arc<dyn MainLoop<Core>> = Arc::new(core_loop()?);

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
    let handler = |core: &mut Core, request: Request| -> Reply {
        assert!(request.cmd != agterm_proto::Command::Tree, "handler blew up");
        core.dispatch(&request).into()
    };
    let fixture = start_on(core_loop()?, handler, ConnectionDeadlines::default())?;
    let response = answer(&fixture.socket, br#"{"cmd":"tree"}"#)?;
    assert_eq!(response, Response::failure("internal error handling tree"));
    assert_version_ok(&fixture.socket)?;
    assert!(fixture.main_loop.close(), "the main loop died with the handler");
    Ok(())
}

#[test]
fn closed_main_loop_answers_shutting_down() -> TestResult {
    let fixture = start_core()?;
    assert!(fixture.main_loop.close(), "main loop lost");
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
        dir_join(&fixture.dir, "ngterm.sock.unavailable")
    );
    let main_loop: Arc<dyn MainLoop<Core>> = Arc::new(core_loop()?);
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
    let socket = dir.path().join("ngterm.sock");
    fs::write(&socket, b"left over by a crashed instance")?;
    let mut server = ControlServer::new(&socket);
    server.start(Arc::new(core_loop()?) as Arc<dyn MainLoop<Core>>, core_handler())?;
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

/// Reports the timeouts the stream came with as a first line, then echoes as [`Echo`].
struct TimeoutProbe;

impl StreamOwner for TimeoutProbe {
    fn adopt(self: Box<Self>, mut stream: UnixStream, pending: Vec<u8>) {
        let timeouts = format!("{:?} {:?}\n", stream.read_timeout().ok(), stream.write_timeout().ok());
        if stream.write_all(timeouts.as_bytes()).is_ok() {
            Box::new(Echo).adopt(stream, pending);
        }
    }
}

#[test]
fn an_adopted_stream_outlives_the_request_timeouts() -> TestResult {
    let short = Duration::from_millis(100);
    let fixture = start_with(
        (),
        |(), _request: Request| Reply::streaming(Response::success(), Box::new(TimeoutProbe)),
        ConnectionDeadlines {
            read_timeout: short,
            read_deadline: Duration::from_secs(1),
            write_timeout: short,
            write_deadline: Duration::from_secs(1),
        },
    )?;

    let mut stream = connect(&fixture.socket)?;
    stream.write_all(VERSION)?;
    stream.write_all(b"\n")?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    assert_eq!(serde_json::from_str::<Response>(&line)?, Response::success());
    line.clear();
    reader.read_line(&mut line)?;
    assert_eq!(line, "Some(None) Some(None)\n");

    // idle well past the request-phase read timeout and deadline before the owner hears more
    thread::sleep(Duration::from_millis(1200));
    stream.write_all(b"late frame")?;
    stream.shutdown(Shutdown::Write)?;
    line.clear();
    reader.read_line(&mut line)?;
    assert_eq!(line, "echo:late frame\n");
    Ok(())
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
