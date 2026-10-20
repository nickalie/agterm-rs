//! The attach role through a real server, on every OS: an attach that creates its session, typing into it and reading
//! its output over the connection, a detach that leaves it running, a reattach that starts from its snapshot, the end
//! reaching the client (one that reads only after the session ended too), and an attach to no session refused as gone.
//! The server runs in the test process (`server::run`) on a private namespace, and stops by itself once its session
//! ended and its clients left.
//!
//! Every session gets the test's environment without the live agterm's `AGTERM_*` and `ZMX_*` variables.

#[expect(
    dead_code,
    reason = "the helpers serve the pane, lead, snapshot and attach tests alike"
)]
mod support;

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use agterm_session::endpoint::{self, Endpoint, Stream};
use agterm_session::protocol::{
    AttachClientMessage, AttachRefusal, AttachRequest, AttachServerMessage, Control, ExitStatus, FramingError, Hello,
    PROTOCOL_VERSION, ReplyMetadata, Role, SessionSpec, read_frame, write_control, write_message,
};
use agterm_session::server::clients::ResyncBudget;
use agterm_session::server::lifecycle::{Graces, StopReason};
use agterm_session::server::snapshot::RESYNC_RESET;
use agterm_session::server::{self, ServerConfig};
use support::{PATIENCE, SIZE, TestResult, shell, shown, tagged};

/// A server of a private namespace, run on a thread of the test.
struct Server {
    root: tempfile::TempDir,
    endpoint: Endpoint,
    thread: Option<JoinHandle<std::io::Result<StopReason>>>,
}

impl Server {
    fn start() -> TestResult<Self> {
        Self::start_with(&ServerConfig::default())
    }

    /// A server of `config`, its build and graces the tests' own.
    fn start_with(config: &ServerConfig) -> TestResult<Self> {
        // short on Unix, where a socket path holds about a hundred bytes
        #[cfg(unix)]
        let root = tempfile::Builder::new().prefix("agls-").tempdir_in("/tmp")?;
        #[cfg(windows)]
        let root = tempfile::tempdir()?;
        let directory = root.path().join("live");
        let endpoint = Endpoint::new(&directory);
        let config = ServerConfig {
            build: "attach-test".to_owned(),
            graces: Graces {
                startup: PATIENCE,
                idle: Duration::from_millis(100),
            },
            ..config.clone()
        };
        let thread = thread::spawn(move || server::run(&directory, &config));
        Ok(Self {
            root,
            endpoint,
            thread: Some(thread),
        })
    }

    /// A connection in the attach role, its hello welcomed.
    fn connect(&self) -> TestResult<Connection> {
        let deadline = Instant::now() + PATIENCE;
        let stream = loop {
            match self.endpoint.connect() {
                Ok(stream) => break stream,
                Err(err) if Instant::now() < deadline => {
                    let _ = err;
                    thread::sleep(Duration::from_millis(20));
                }
                Err(err) => return Err(err.into()),
            }
        };
        welcomed(stream)?.ok_or_else(|| "the server closed the connection before its welcome".into())
    }

    /// Wait for the server to stop by itself; why it did.
    fn stopped(mut self) -> TestResult<StopReason> {
        let thread = self.thread.take().ok_or("no server thread")?;
        let deadline = Instant::now() + PATIENCE;
        while !thread.is_finished() {
            assert!(Instant::now() < deadline, "the server never stopped");
            thread::sleep(Duration::from_millis(20));
        }
        Ok(thread.join().map_err(|_| "the server panicked")??)
    }
}

/// `stream` in the attach role, its hello welcomed; `None` when the server closed it first, as a stopping one does
/// (it drops a connection it accepted, or one still in its backlog, unread: the hello's write or the welcome's read
/// then finds the connection closed, reset or its pipe broken).
fn welcomed(mut stream: Stream) -> TestResult<Option<Connection>> {
    let hello = Hello {
        role: Role::Attach,
        build: "attach-test".to_owned(),
        capabilities: Vec::new(),
    };
    match write_control(&mut stream, &Control::Hello(hello)) {
        Ok(()) => {}
        Err(err) if server_closed(&err) => return Ok(None),
        Err(err) => return Err(err.into()),
    }
    let frame = match read_frame(&mut stream) {
        Ok(frame) => frame,
        Err(err) if server_closed(&err) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let Control::Welcome(welcome) = frame.control()? else {
        return Err("no welcome".into());
    };
    assert_eq!(welcome.error, None);
    Ok(Some(Connection::new(stream)))
}

/// Whether `err` means the server closed the connection, as the server's own roles read a vanished client.
fn server_closed(err: &FramingError) -> bool {
    match err {
        FramingError::Closed => true,
        FramingError::Io(err) => endpoint::is_connection_closed_error(err),
        _ => false,
    }
}

/// The client's end of an attach connection: what the server sends arrives on a channel, read by a thread of its own.
struct Connection {
    stream: Arc<Stream>,
    messages: Receiver<AttachServerMessage>,
    reader: Option<JoinHandle<()>>,
    /// While set, the reader reads nothing more, so the server's writes back up.
    paused: Arc<(Mutex<bool>, Condvar)>,
    /// The output text received.
    output: Vec<u8>,
    /// The reply-ownership reports received: owner, generation.
    owners: Vec<(bool, u32)>,
}

impl Drop for Connection {
    /// Close the connection: its reader, blocked in a read, is woken (again, should it start its next read after a
    /// wake), and the last handle goes with it.
    fn drop(&mut self) {
        let Some(reader) = self.reader.take() else {
            return;
        };
        self.pause(false);
        let deadline = Instant::now() + PATIENCE;
        while !reader.is_finished() && Instant::now() < deadline {
            endpoint::disconnect(&self.stream);
            thread::sleep(Duration::from_millis(10));
        }
        let _ = reader.join();
    }
}

impl Connection {
    fn new(stream: Stream) -> Self {
        let stream = Arc::new(stream);
        let (sender, messages) = mpsc::channel();
        let reading = Arc::clone(&stream);
        let paused = Arc::new((Mutex::new(false), Condvar::new()));
        let gate = Arc::clone(&paused);
        let reader = thread::spawn(move || {
            while let Ok(message) = {
                wait_unpaused(&gate);
                read_frame(&mut &*reading).and_then(|frame| frame.message::<AttachServerMessage>(PROTOCOL_VERSION))
            } {
                if sender.send(message).is_err() {
                    return;
                }
            }
        });
        Self {
            stream,
            messages,
            reader: Some(reader),
            paused,
            output: Vec::new(),
            owners: Vec::new(),
        }
    }

    /// Stop or resume reading what the server sends.
    fn pause(&self, paused: bool) {
        let (flag, changed) = &*self.paused;
        *flag.lock().unwrap_or_else(PoisonError::into_inner) = paused;
        changed.notify_all();
    }

    fn send(&self, message: &AttachClientMessage) -> Result<(), FramingError> {
        write_message(&mut &*self.stream, message)
    }

    fn attach(&self, name: &str, create: Option<SessionSpec>) -> Result<(), FramingError> {
        self.send(&AttachClientMessage::Attach(AttachRequest {
            name: name.to_owned(),
            create,
            size: SIZE,
            managed: None,
            metadata: ReplyMetadata::default(),
        }))
    }

    fn type_line(&self, line: &str) -> Result<(), FramingError> {
        let enter = if cfg!(windows) { "\r" } else { "\n" };
        self.send(&AttachClientMessage::Input {
            data: format!("{line}{enter}").into_bytes(),
        })
    }

    /// The next message but the reply-ownership reports (kept in `owners`); `None` once the server closed the
    /// connection.
    fn next(&mut self) -> TestResult<Option<AttachServerMessage>> {
        match self.messages.recv_timeout(PATIENCE) {
            Ok(AttachServerMessage::ReplyOwner { owner, generation }) => {
                self.owners.push((owner, generation));
                self.next()
            }
            Ok(message) => {
                if let AttachServerMessage::Output { data, .. } = &message {
                    self.output.extend_from_slice(data);
                }
                Ok(Some(message))
            }
            Err(RecvTimeoutError::Disconnected) => Ok(None),
            Err(RecvTimeoutError::Timeout) => Err(format!("the server sent nothing in {PATIENCE:?}").into()),
        }
    }

    /// A whole snapshot: its epoch, offset and bytes.
    fn snapshot(&mut self) -> TestResult<(u64, u64, Vec<u8>)> {
        match self.next()? {
            Some(AttachServerMessage::SnapshotBegin { epoch, offset, size }) => {
                assert_eq!(size, SIZE);
                Ok((epoch, offset, snapshot_body(self, epoch)?))
            }
            other => Err(format!("no snapshot began: {other:?}").into()),
        }
    }

    /// Read until `needle` is in the output shown.
    fn expect(&mut self, needle: &str) -> TestResult {
        while !shown(&self.output).contains(needle) {
            match self.next()? {
                Some(
                    AttachServerMessage::Output { .. }
                    | AttachServerMessage::Resized { .. }
                    | AttachServerMessage::Role { .. },
                ) => {}
                other => {
                    return Err(format!("no {needle:?} before {other:?}: {:?}", shown(&self.output)).into());
                }
            }
        }
        Ok(())
    }

    /// Read up to the session's exit.
    fn finish(&mut self) -> TestResult<ExitStatus> {
        loop {
            match self.next()? {
                Some(AttachServerMessage::Exit { status }) => return Ok(status),
                Some(AttachServerMessage::Output { .. } | AttachServerMessage::Resized { .. }) => {}
                other => return Err(format!("no exit before {other:?}").into()),
            }
        }
    }

    /// Read to the connection's end, which must come without another message.
    fn closed(&mut self) -> TestResult {
        assert_eq!(self.next()?, None, "the server closed the connection");
        Ok(())
    }
}

/// Wait while `gate` holds the reader paused.
fn wait_unpaused(gate: &(Mutex<bool>, Condvar)) {
    let (flag, changed) = gate;
    let mut paused = flag.lock().unwrap_or_else(PoisonError::into_inner);
    while *paused {
        paused = changed.wait(paused).unwrap_or_else(PoisonError::into_inner);
    }
}

fn quit(connection: &Connection) -> Result<(), FramingError> {
    if cfg!(windows) {
        connection.type_line("exit")
    } else {
        connection.send(&AttachClientMessage::Input { data: b"\x04".to_vec() })
    }
}

#[test]
fn an_attach_creates_its_session_and_a_reattach_starts_from_its_snapshot() -> TestResult {
    let server = Server::start()?;
    let mut first = server.connect()?;
    first.attach("one", Some(shell()))?;
    let (epoch, offset, data) = first.snapshot()?;
    assert_eq!((epoch, offset), (1, 0));
    assert!(data.is_empty(), "a new session has nothing to show");
    first.type_line(&tagged("hi"))?;
    first.expect("got:hi")?;
    first.send(&AttachClientMessage::Detach)?;
    // the queued output may still come first, then the end of the connection
    while first.next()?.is_some() {}

    // a reattach, spec given: the running session is attached to, not created again
    let mut second = server.connect()?;
    second.attach("one", Some(shell()))?;
    let (epoch, offset, data) = second.snapshot()?;
    assert_eq!(epoch, 1);
    assert!(offset > 0);
    assert!(shown(&data).contains("got:hi"), "{:?}", shown(&data));
    second.type_line(&tagged("again"))?;
    second.expect("got:again")?;
    quit(&second)?;
    match second.finish()? {
        ExitStatus::Exited { code, .. } => assert_eq!(code, 0),
        ExitStatus::Unknown => panic!("the status is unknown"),
    }
    // the server ends the connection after the exit; the client need not close it first
    second.closed()?;
    drop(second);
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

/// Whether the session `name` is gone: an attach to it is refused as such, or the server has stopped, which it does
/// once its last session ended and its last client left. One that attaches leaves at once.
fn gone(server: &Server, name: &str) -> TestResult<bool> {
    let Ok(stream) = server.endpoint.connect() else {
        return Ok(true);
    };
    let Some(mut probe) = welcomed(stream)? else {
        return Ok(true);
    };
    match probe.attach(name, None) {
        Ok(()) => {}
        // stopped between its welcome and the attach
        Err(err) if server_closed(&err) => return Ok(true),
        Err(err) => return Err(err.into()),
    }
    Ok(matches!(
        probe.next()?,
        Some(AttachServerMessage::Refused {
            reason: AttachRefusal::Gone,
            ..
        })
    ))
}

#[test]
fn a_client_that_reads_late_still_gets_the_sessions_last_output_and_exit() -> TestResult {
    let server = Server::start()?;
    let mut late = server.connect()?;
    late.attach("late", Some(shell()))?;
    assert_eq!(late.snapshot()?.0, 1);
    // the session prints its last line and ends while the client reads nothing
    late.pause(true);
    let done = server.root.path().join("late-done");
    let ending = if cfg!(windows) {
        format!("{} & type nul > \"{}\" & exit", tagged("last"), done.display())
    } else {
        format!("{}; : > '{}'; exit", tagged("last"), done.display())
    };
    late.type_line(&ending)?;
    let deadline = Instant::now() + PATIENCE;
    while !done.exists() || !gone(&server, "late")? {
        assert!(Instant::now() < deadline, "the session never ended");
        thread::sleep(Duration::from_millis(50));
    }
    // by now the writer has sent the exit; a server that disconnected right after it lost what the client had not read
    thread::sleep(Duration::from_millis(500));
    late.pause(false);
    loop {
        match late.next()? {
            Some(AttachServerMessage::Exit { .. }) => break,
            Some(
                AttachServerMessage::Output { .. }
                | AttachServerMessage::Resized { .. }
                | AttachServerMessage::Role { .. },
            ) => {}
            other => return Err(format!("no exit before {other:?}: {:?}", shown(&late.output)).into()),
        }
    }
    assert!(shown(&late.output).contains("got:last"), "{:?}", shown(&late.output));
    late.closed()?;
    drop(late);
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

#[test]
fn an_attach_to_no_session_is_refused_as_gone() -> TestResult {
    let server = Server::start()?;
    let mut client = server.connect()?;
    client.attach("missing", None)?;
    match client.next()? {
        Some(AttachServerMessage::Refused { reason, message }) => {
            assert_eq!(reason, AttachRefusal::Gone);
            assert_eq!(message, "no live session named \"missing\"");
        }
        other => panic!("not refused: {other:?}"),
    }
    client.closed()?;
    drop(client);
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

#[test]
fn a_session_that_cannot_start_is_refused() -> TestResult {
    let server = Server::start()?;
    let mut client = server.connect()?;
    let mut spec = shell();
    spec.argv.clear();
    client.attach("broken", Some(spec))?;
    match client.next()? {
        Some(AttachServerMessage::Refused { reason, message }) => {
            assert_eq!(reason, AttachRefusal::CreateFailed);
            assert!(message.contains("could not be started"), "{message}");
        }
        other => panic!("not refused: {other:?}"),
    }
    client.closed()?;
    drop(client);
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

/// The line that makes [`shell`] print about a megabyte, far more than a connection buffers, then `flood-end` (which
/// the echoed line does not hold), then create the file `done`.
fn flood(done: &Path) -> String {
    let line = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    let done = done.display();
    if cfg!(windows) {
        format!("(for /l %i in (1,1,16000) do @echo line-%i-{line}) & echo flood^-end & type nul > \"{done}\"")
    } else {
        format!(
            "i=0; while [ $i -lt 16000 ]; do echo line-$i-{line}; i=$((i+1)); done; echo flood\"\"-end; : > '{done}'"
        )
    }
}

/// A server whose clients queue 4 KiB under `budget`, and a session flooded while the client that created it read
/// nothing: its writer was held up by the full connection, and its queue overflowed. The client reads again.
fn fallen_behind(budget: ResyncBudget) -> TestResult<(Server, Connection)> {
    let server = Server::start_with(&ServerConfig {
        client_queue_limit: 4 * 1024,
        resync_budget: budget,
        ..ServerConfig::default()
    })?;
    let mut slow = server.connect()?;
    slow.attach("flood", Some(shell()))?;
    assert_eq!(slow.snapshot()?.0, 1);
    slow.pause(true);
    let done = server.root.path().join("flood-done");
    slow.type_line(&flood(&done))?;
    let deadline = Instant::now() + PATIENCE;
    while !done.exists() {
        assert!(Instant::now() < deadline, "the flood never ended");
        thread::sleep(Duration::from_millis(20));
    }
    slow.pause(false);
    Ok((server, slow))
}

/// The rest of a snapshot of `epoch` whose begin was read: its bytes.
fn snapshot_body(connection: &mut Connection, epoch: u64) -> TestResult<Vec<u8>> {
    let mut data = Vec::new();
    loop {
        match connection.next()? {
            Some(AttachServerMessage::SnapshotChunk {
                epoch: chunk,
                data: part,
            }) if chunk == epoch => {
                data.extend_from_slice(&part);
            }
            Some(AttachServerMessage::SnapshotEnd { epoch: end }) if end == epoch => return Ok(data),
            other => return Err(format!("the snapshot of epoch {epoch} was cut by {other:?}").into()),
        }
    }
}

#[test]
fn a_client_that_falls_behind_is_resynced_in_band() -> TestResult {
    // the 4 KiB queue overflows whenever the writer lags, before the connection is full too: every overflow is resynced
    let budget = ResyncBudget {
        resyncs: u32::MAX,
        window: Duration::from_secs(3600),
    };
    let (server, mut slow) = fallen_behind(budget)?;
    slow.type_line(&tagged("after"))?;
    // the output is contiguous between snapshots, each snapshot of a later epoch and resetting what came before it;
    // what the client shows is the last snapshot and the output after it
    let (mut epoch, mut next, mut shows) = (1, 0, Vec::new());
    while !shown(&shows).contains("got:after") {
        match slow.next()? {
            Some(AttachServerMessage::SnapshotBegin {
                epoch: resync,
                offset,
                size,
            }) => {
                assert!(
                    resync > epoch && size == SIZE,
                    "epoch {resync} after {epoch}, size {size:?}"
                );
                epoch = resync;
                next = offset;
                shows = snapshot_body(&mut slow, epoch)?;
                assert!(
                    shows.starts_with(RESYNC_RESET),
                    "a resync resets what the client was sent"
                );
            }
            Some(AttachServerMessage::Output { offset, data }) => {
                assert_eq!(
                    offset, next,
                    "the output is contiguous with the snapshot and the output before it"
                );
                next += data.len() as u64;
                shows.extend_from_slice(&data);
            }
            other => panic!("no got:after before {other:?}"),
        }
    }
    assert!(epoch >= 2, "the client was resynced");
    assert!(shown(&shows).contains("flood-end"), "the flood's end is shown");
    quit(&slow)?;
    slow.finish()?;
    drop(slow);
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

#[test]
fn a_client_past_its_resync_budget_is_disconnected() -> TestResult {
    let budget = ResyncBudget {
        resyncs: 0,
        window: Duration::from_secs(3600),
    };
    let (server, mut slow) = fallen_behind(budget)?;
    // what was written before its writer was held up, then the end of the connection: no snapshot, no exit
    while let Some(message) = slow.next()? {
        assert!(
            matches!(message, AttachServerMessage::Output { .. }),
            "a dropped client got {message:?}"
        );
    }
    drop(slow);
    // the session runs on, for the next client
    let mut next = server.connect()?;
    next.attach("flood", None)?;
    let (epoch, offset, _) = next.snapshot()?;
    assert!(epoch == 1 && offset > 500_000, "{epoch} {offset}");
    next.type_line(&tagged("still"))?;
    next.expect("got:still")?;
    quit(&next)?;
    next.finish()?;
    drop(next);
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_clipboard_read_is_delegated_over_the_connection_and_answered_back() -> TestResult {
    use agterm_session::protocol::{DelegatedReply, DelegatedRequest};

    let server = Server::start()?;
    let mut owner = server.connect()?;
    // the program reads the clipboard and prints the answer's 12 bytes, hex
    let program = "stty raw -echo; printf 'go\\r\\n\\033]52;c;?\\007'; \
                   printf 'got:%s\\r\\n' \"$(head -c 12 | od -An -tx1 | tr -d ' \\n')\"; stty sane";
    owner.attach("delegate", Some(support::script(program)))?;
    let (epoch, _, _) = owner.snapshot()?;
    assert_eq!(owner.owners, [(true, 1)], "the creator owns the replies");
    let (id, request_epoch) = loop {
        match owner.next()? {
            Some(AttachServerMessage::DelegatedRequest { id, epoch, request }) => {
                let selection = "c".to_owned();
                assert_eq!(
                    request,
                    DelegatedRequest::ClipboardRead {
                        selection,
                        list_only: false
                    }
                );
                break (id, epoch);
            }
            Some(AttachServerMessage::Output { .. }) => {}
            other => return Err(format!("no delegated read before {other:?}").into()),
        }
    };
    assert_eq!(request_epoch, epoch);
    assert!(
        shown(&owner.output).contains("go"),
        "the output before the read came first"
    );
    owner.send(&AttachClientMessage::DelegatedReply {
        id,
        epoch,
        reply: DelegatedReply::Clipboard {
            text: Some("hi".to_owned()),
        },
    })?;
    // `ESC ] 5 2 ; c ; aGk= BEL`
    owner.expect("got:1b5d35323b633b61476b3d07")?;
    owner.finish()?;
    owner.closed()?;
    drop(owner);
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}
