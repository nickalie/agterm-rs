//! [`LineStream`] over socket pairs, and through the control server over real sockets in a temp dir.

use std::error::Error;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use agterm_host::MainLoop;
use agterm_host::fakes::FakeMainLoop;
use agterm_proto::{Command, Request, Response};
use agterm_server::{ControlServer, LineStream, LineStreamLimits, Reply, StreamOwner};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const TIMEOUT: Duration = Duration::from_secs(20);
const VERSION: &[u8] = br#"{"cmd":"version"}"#;
/// The request the test handler answers with a stream.
const OPEN: &[u8] = br#"{"cmd":"tree"}"#;
const LIMITS: LineStreamLimits = LineStreamLimits {
    max_line_bytes: 1024,
    max_pending_lines: 16,
    write_timeout: Duration::from_secs(5),
};

/// What an echoing stream reports.
#[derive(Debug, PartialEq, Eq)]
enum Event {
    /// The owner took the connection, with these bytes read past the request line.
    Adopted(Vec<u8>),
    /// `on_close` ran.
    Closed,
}

/// Runs `stream` over `socket`, answering every line with `echo:<line>` and reporting to `events`.
fn echo(stream: &LineStream, socket: UnixStream, pending: &[u8], events: &Sender<Event>) {
    events.send(Event::Adopted(pending.to_vec())).ok();
    let replies = stream.clone();
    let on_line = |line: &[u8]| {
        let mut reply = b"echo:".to_vec();
        reply.extend_from_slice(line);
        if !replies.send(reply) {
            replies.shutdown();
        }
    };
    stream.run(socket, pending, on_line, || {
        events.send(Event::Closed).ok();
    });
}

/// The only holder of its stream's event sender, so the events channel disconnects once the owner is done.
struct EchoOwner {
    stream: LineStream,
    events: Sender<Event>,
}

impl StreamOwner for EchoOwner {
    fn adopt(self: Box<Self>, socket: UnixStream, pending: Vec<u8>) {
        echo(&self.stream, socket, &pending, &self.events);
    }
}

/// One stream the server opened, as the app would register it.
struct Opened {
    stream: LineStream,
    events: Receiver<Event>,
}

impl Opened {
    fn next(&self) -> TestResult<Event> {
        Ok(self.events.recv_timeout(TIMEOUT)?)
    }

    /// `on_close` ran, and nothing follows it: the owner has let go of the only sender.
    fn assert_closed_once(&self) -> TestResult {
        assert_eq!(self.next()?, Event::Closed);
        assert_eq!(self.events.recv_timeout(TIMEOUT), Err(RecvTimeoutError::Disconnected));
        assert!(!self.stream.send(b"late".to_vec()), "a closed stream took a line");
        Ok(())
    }
}

/// Fields drop top to bottom: the server stops before the temp dir holding its socket is deleted.
struct Fixture {
    _server: ControlServer,
    opened: Receiver<Opened>,
    socket: PathBuf,
    _main_loop: Arc<FakeMainLoop<()>>,
    _dir: TempDir,
}

/// A server answering [`OPEN`] with an echoing [`LineStream`] and everything else with a plain `ok`.
fn start(limits: LineStreamLimits) -> TestResult<Fixture> {
    let dir = tempfile::Builder::new().prefix("agt").tempdir_in("/tmp")?;
    let socket = dir.path().join("agterm.sock");
    let main_loop = Arc::new(FakeMainLoop::spawn(())?);
    let (opened_tx, opened) = mpsc::channel();
    let handler = move |(): &mut (), request: Request| -> Reply {
        if request.cmd != Command::Tree {
            return Response::success().into();
        }
        let stream = LineStream::new(limits);
        let (events_tx, events) = mpsc::channel();
        opened_tx
            .send(Opened {
                stream: stream.clone(),
                events,
            })
            .ok();
        let owner = EchoOwner {
            stream,
            events: events_tx,
        };
        Reply::streaming(Response::success(), Box::new(owner))
    };
    let mut server = ControlServer::new(&socket);
    server.start(Arc::clone(&main_loop) as Arc<dyn MainLoop<()>>, handler)?;
    Ok(Fixture {
        _server: server,
        opened,
        socket,
        _main_loop: main_loop,
        _dir: dir,
    })
}

/// A plain request on a fresh connection.
fn assert_ordinary_request_answered(fixture: &Fixture) -> TestResult {
    let mut stream = UnixStream::connect(&fixture.socket)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.write_all(VERSION)?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    assert_eq!(serde_json::from_str::<Response>(&line)?, Response::success());
    Ok(())
}

/// The client side of a stream connection.
struct Client {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    fn new(socket: UnixStream) -> io::Result<Self> {
        socket.set_read_timeout(Some(TIMEOUT))?;
        Ok(Self {
            reader: BufReader::new(socket.try_clone()?),
            writer: socket,
        })
    }

    /// Sends [`OPEN`] with `first` right behind it in the same write, and takes the `ok`.
    fn open(fixture: &Fixture, first: &[u8]) -> TestResult<(Self, Opened)> {
        let mut client = Self::new(UnixStream::connect(&fixture.socket)?)?;
        let mut request = OPEN.to_vec();
        request.push(b'\n');
        request.extend_from_slice(first);
        client.writer.write_all(&request)?;
        assert_eq!(serde_json::from_str::<Response>(&client.line()?)?, Response::success());
        let opened = fixture.opened.recv_timeout(TIMEOUT)?;
        Ok((client, opened))
    }

    fn say(&mut self, text: &str) -> io::Result<()> {
        self.writer.write_all(format!("{text}\n").as_bytes())
    }

    /// The next line, newline included; empty at EOF.
    fn line(&mut self) -> io::Result<String> {
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        Ok(line)
    }
}

/// Runs an echoing `stream` over one end of a socket pair on a thread of its own; the other end is the peer.
fn run_on_pair(stream: &LineStream) -> TestResult<(Client, Opened, JoinHandle<()>)> {
    let (peer, ours) = UnixStream::pair()?;
    let (events_tx, events) = mpsc::channel();
    let runner = {
        let stream = stream.clone();
        thread::spawn(move || echo(&stream, ours, &[], &events_tx))
    };
    let opened = Opened {
        stream: stream.clone(),
        events,
    };
    assert_eq!(opened.next()?, Event::Adopted(Vec::new()));
    Ok((Client::new(peer)?, opened, runner))
}

#[test]
fn a_stream_survives_other_requests_and_closes_on_client_hang_up() -> TestResult {
    let fixture = start(LIMITS)?;
    let (mut client, opened) = Client::open(&fixture, b"")?;
    assert_eq!(opened.next()?, Event::Adopted(Vec::new()));
    client.say("one")?;
    assert_eq!(client.line()?, "echo:one\n");

    for _ in 0..3 {
        assert_ordinary_request_answered(&fixture)?;
    }
    // the app may speak first as well
    assert!(opened.stream.send(b"pushed".to_vec()));
    assert_eq!(client.line()?, "pushed\n");
    client.say("two")?;
    assert_eq!(client.line()?, "echo:two\n");

    drop(client);
    opened.assert_closed_once()?;
    assert_ordinary_request_answered(&fixture)
}

#[test]
fn bytes_sent_with_the_request_reach_on_line_first() -> TestResult {
    let fixture = start(LIMITS)?;
    let (mut client, opened) = Client::open(&fixture, b"first\nsec")?;
    assert_eq!(opened.next()?, Event::Adopted(b"first\nsec".to_vec()));
    client.writer.write_all(b"ond\n")?;
    assert_eq!(client.line()?, "echo:first\n");
    assert_eq!(client.line()?, "echo:second\n");
    Ok(())
}

#[test]
fn server_side_shutdown_gives_the_client_eof() -> TestResult {
    let fixture = start(LIMITS)?;
    let (mut client, opened) = Client::open(&fixture, b"")?;
    assert_eq!(opened.next()?, Event::Adopted(Vec::new()));
    client.say("ping")?;
    assert_eq!(client.line()?, "echo:ping\n");

    opened.stream.shutdown();
    opened.stream.shutdown();
    assert_eq!(client.line()?, "", "the client did not see EOF");
    opened.assert_closed_once()?;
    assert_ordinary_request_answered(&fixture)
}

#[test]
fn lines_queued_before_run_are_bounded_and_written_once_it_runs() -> TestResult {
    let stream = LineStream::new(LineStreamLimits {
        max_pending_lines: 2,
        ..LIMITS
    });
    assert!(stream.send(b"a".to_vec()));
    assert!(stream.send(b"b".to_vec()));
    assert!(!stream.send(b"c".to_vec()), "a full queue took a line");

    let (mut peer, opened, runner) = run_on_pair(&stream)?;
    assert_eq!(peer.line()?, "a\n");
    assert_eq!(peer.line()?, "b\n");
    // drained, it takes lines again
    assert!(stream.send(b"d".to_vec()));
    assert_eq!(peer.line()?, "d\n");

    stream.shutdown();
    assert_eq!(peer.line()?, "");
    opened.assert_closed_once()?;
    runner.join().map_err(|_| "runner panicked")?;
    Ok(())
}

#[test]
fn shutdown_before_run_closes_at_once() -> TestResult {
    let stream = LineStream::new(LIMITS);
    assert!(stream.send(b"never written".to_vec()));
    stream.shutdown();
    assert!(!stream.send(b"refused".to_vec()));

    let (mut peer, opened, runner) = run_on_pair(&stream)?;
    let mut rest = Vec::new();
    peer.reader.read_to_end(&mut rest)?;
    assert_eq!(rest, b"");
    opened.assert_closed_once()?;
    runner.join().map_err(|_| "runner panicked")?;
    Ok(())
}

#[test]
fn a_peer_that_never_reads_is_dropped_at_the_write_timeout() -> TestResult {
    let stream = LineStream::new(LineStreamLimits {
        write_timeout: Duration::from_millis(100),
        ..LIMITS
    });
    let (peer, opened, runner) = run_on_pair(&stream)?;
    let started = Instant::now();
    // far more than an AF_UNIX socket buffers, so the writer blocks on a peer that takes nothing
    let big = vec![b'x'; 4 << 20];
    assert!(stream.send(big.clone()));
    assert!(stream.send(big));
    opened.assert_closed_once()?;
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    runner.join().map_err(|_| "runner panicked")?;
    drop(peer);
    Ok(())
}

#[test]
fn lines_split_across_reads_are_joined() -> TestResult {
    let stream = LineStream::new(LIMITS);
    let (mut peer, opened, runner) = run_on_pair(&stream)?;
    for part in [&b"ab"[..], b"c\nd", b"\n\n"] {
        peer.writer.write_all(part)?;
    }
    assert_eq!(peer.line()?, "echo:abc\n");
    assert_eq!(peer.line()?, "echo:d\n");
    assert_eq!(peer.line()?, "echo:\n");
    drop(peer);
    opened.assert_closed_once()?;
    runner.join().map_err(|_| "runner panicked")?;
    Ok(())
}

#[test]
fn an_over_long_line_closes_the_stream() -> TestResult {
    let limits = LineStreamLimits {
        max_line_bytes: 8,
        ..LIMITS
    };
    // a finished line over the limit, and an unfinished one already past it
    for over in [&b"123456789\n"[..], b"123456789"] {
        let stream = LineStream::new(limits);
        let (mut peer, opened, runner) = run_on_pair(&stream)?;
        peer.say("12345678")?;
        assert_eq!(peer.line()?, "echo:12345678\n", "a line at the limit is taken");
        peer.writer.write_all(over)?;
        assert_eq!(peer.line()?, "", "{over:?} did not close the stream");
        opened.assert_closed_once()?;
        runner.join().map_err(|_| "runner panicked")?;
    }
    Ok(())
}
