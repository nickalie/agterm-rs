//! The api role through a real server, on every OS: the listing with each record's fields, the screen read from the
//! server's terminal with a revision that moves only when the screen does, typing encoded and queued without taking
//! the lead, a print reaching every attached client, kills (one, a batch with a missing name, plain and forced) and
//! the requests for a session that ended, and a shell's foreground job read, waited for and ended. The server runs in
//! the test process (`server::run`) on a private namespace, and stops by itself once its sessions ended and its
//! clients left.
//!
//! Every session gets the test's environment without the live agterm's `AGTERM_*` and `ZMX_*` variables.

#[expect(
    dead_code,
    reason = "the helpers serve the pane, lead, snapshot, attach and api tests alike"
)]
mod support;

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use agterm_session::endpoint::{self, Endpoint, Stream};
use agterm_session::protocol::{
    ApiCall, ApiError, ApiErrorKind, ApiReply, ApiRequest, ApiResponse, AttachClientMessage, AttachRequest,
    AttachServerMessage, Control, Hello, JobProcess, KillOutcome, KillResult, LeadRole, Managed, PROTOCOL_VERSION,
    ReplyMetadata, Role, Screen, SessionRecord, SessionSpec, read_frame, write_control, write_message,
};
use agterm_session::server::lifecycle::{Graces, StopReason};
use agterm_session::server::{self, ServerConfig};
use support::{PATIENCE, SIZE, TestResult, shell, shown, spec, tagged};

/// A server of a private namespace, run on a thread of the test.
struct Server {
    _root: tempfile::TempDir,
    endpoint: Endpoint,
    thread: Option<JoinHandle<std::io::Result<StopReason>>>,
}

impl Server {
    fn start() -> TestResult<Self> {
        // short on Unix, where a socket path holds about a hundred bytes
        #[cfg(unix)]
        let root = tempfile::Builder::new().prefix("agls-").tempdir_in("/tmp")?;
        #[cfg(windows)]
        let root = tempfile::tempdir()?;
        let directory = root.path().join("live");
        let endpoint = Endpoint::new(&directory);
        let config = ServerConfig {
            build: "api-test".to_owned(),
            graces: Graces {
                startup: PATIENCE,
                idle: Duration::from_millis(100),
            },
            ..ServerConfig::default()
        };
        let thread = thread::spawn(move || server::run(&directory, &config));
        Ok(Self {
            _root: root,
            endpoint,
            thread: Some(thread),
        })
    }

    /// A connection in the `role` given, its hello welcomed.
    fn connect(&self, role: Role) -> TestResult<Stream> {
        let deadline = Instant::now() + PATIENCE;
        let mut stream = loop {
            match self.endpoint.connect() {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                Err(err) => return Err(err.into()),
            }
        };
        let hello = Hello {
            role,
            build: "api-test".to_owned(),
            capabilities: Vec::new(),
        };
        write_control(&mut stream, &Control::Hello(hello))?;
        let Control::Welcome(welcome) = read_frame(&mut stream)?.control()? else {
            return Err("no welcome".into());
        };
        assert_eq!(welcome.error, None);
        Ok(stream)
    }

    fn api(&self) -> TestResult<Api> {
        Ok(Api {
            stream: self.connect(Role::Api)?,
            next: 1,
        })
    }

    /// An attach to `name`, creating it from `create` when given, under explicit leadership when `managed` is given.
    fn attach(&self, name: &str, create: Option<SessionSpec>, managed: Option<Managed>) -> TestResult<Attach> {
        let stream = self.connect(Role::Attach)?;
        write_message(
            &mut &stream,
            &AttachClientMessage::Attach(AttachRequest {
                name: name.to_owned(),
                create,
                size: SIZE,
                managed,
                metadata: ReplyMetadata::default(),
            }),
        )?;
        Ok(Attach::new(stream))
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

/// The client's end of an api connection.
struct Api {
    stream: Stream,
    next: u64,
}

impl Api {
    /// The server's answer to `request`, its reply's id checked.
    fn call(&mut self, request: ApiRequest) -> TestResult<Result<ApiResponse, ApiError>> {
        let id = self.next;
        self.next += 1;
        write_message(&mut self.stream, &ApiCall { id, request })?;
        let reply: ApiReply = read_frame(&mut self.stream)?.message(PROTOCOL_VERSION)?;
        assert_eq!(reply.id, id, "the reply answers the call");
        Ok(reply.result)
    }

    fn list(&mut self) -> TestResult<Vec<SessionRecord>> {
        match self.call(ApiRequest::List)? {
            Ok(ApiResponse::Sessions(sessions)) => Ok(sessions),
            other => Err(format!("no listing: {other:?}").into()),
        }
    }

    fn screen(&mut self, name: &str, all: bool) -> TestResult<Result<Screen, ApiError>> {
        Ok(
            match self.call(ApiRequest::Screen {
                name: name.to_owned(),
                all,
            })? {
                Ok(ApiResponse::Screen(screen)) => Ok(screen),
                Ok(other) => return Err(format!("not a screen: {other:?}").into()),
                Err(err) => Err(err),
            },
        )
    }

    /// Wait for `needle` on the session's screen; that screen.
    fn expect_screen(&mut self, name: &str, all: bool, needle: &str) -> TestResult<Screen> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let screen = self.screen(name, all)?.map_err(|err| err.message)?;
            if screen.text.contains(needle) {
                return Ok(screen);
            }
            assert!(
                Instant::now() < deadline,
                "no {needle:?} on the screen: {:?}",
                screen.text
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// The active screen of `name` once two reads in a row agree, its revision included: the session wrote nothing
    /// between them, and reading moved nothing. Fails when they never agree within [`PATIENCE`].
    fn settled_screen(&mut self, name: &str) -> TestResult<Screen> {
        let deadline = Instant::now() + PATIENCE;
        let mut last = self.screen(name, false)?.map_err(|err| err.message)?;
        loop {
            thread::sleep(Duration::from_millis(50));
            let screen = self.screen(name, false)?.map_err(|err| err.message)?;
            if screen == last {
                return Ok(screen);
            }
            assert!(
                Instant::now() < deadline,
                "the screen never settled: revision {} then {}",
                last.revision,
                screen.revision
            );
            last = screen;
        }
    }

    fn type_text(&mut self, name: &str, text: &str) -> TestResult<Result<ApiResponse, ApiError>> {
        self.call(ApiRequest::Type {
            name: name.to_owned(),
            data: text.as_bytes().to_vec(),
        })
    }

    fn print(&mut self, name: &str, data: &[u8]) -> TestResult<Result<ApiResponse, ApiError>> {
        self.call(ApiRequest::Print {
            name: name.to_owned(),
            data: data.to_vec(),
        })
    }

    fn kill(&mut self, names: &[&str], force: bool) -> TestResult<Vec<KillResult>> {
        let names = names.iter().map(|name| (*name).to_owned()).collect();
        match self.call(ApiRequest::Kill { names, force })? {
            Ok(ApiResponse::Killed(results)) => Ok(results),
            other => Err(format!("no kill results: {other:?}").into()),
        }
    }

    /// Wait until the listing no longer holds `name`.
    fn expect_unlisted(&mut self, name: &str) -> TestResult {
        let deadline = Instant::now() + PATIENCE;
        while self.list()?.iter().any(|record| record.name == name) {
            assert!(Instant::now() < deadline, "{name} is still listed");
            thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
}

/// The client's end of an attach connection: what the server sends arrives on a channel, read by a thread of its own.
struct Attach {
    stream: Arc<Stream>,
    messages: Receiver<AttachServerMessage>,
    reader: Option<JoinHandle<()>>,
    /// Every message received.
    received: Vec<AttachServerMessage>,
    /// The output received.
    output: Vec<u8>,
}

impl Drop for Attach {
    /// Close the connection, waking its reader.
    fn drop(&mut self) {
        let Some(reader) = self.reader.take() else {
            return;
        };
        let deadline = Instant::now() + PATIENCE;
        while !reader.is_finished() && Instant::now() < deadline {
            endpoint::disconnect(&self.stream);
            thread::sleep(Duration::from_millis(10));
        }
        let _ = reader.join();
    }
}

impl Attach {
    fn new(stream: Stream) -> Self {
        let stream = Arc::new(stream);
        let (sender, messages) = mpsc::channel();
        let reading = Arc::clone(&stream);
        let reader = thread::spawn(move || {
            while let Ok(message) =
                read_frame(&mut &*reading).and_then(|frame| frame.message::<AttachServerMessage>(PROTOCOL_VERSION))
            {
                if sender.send(message).is_err() {
                    return;
                }
            }
        });
        Self {
            stream,
            messages,
            reader: Some(reader),
            received: Vec::new(),
            output: Vec::new(),
        }
    }

    /// The next message; `None` once the server closed the connection.
    fn next(&mut self) -> TestResult<Option<AttachServerMessage>> {
        match self.messages.recv_timeout(PATIENCE) {
            Ok(message) => {
                if let AttachServerMessage::Output { data, .. } = &message {
                    self.output.extend_from_slice(data);
                }
                self.received.push(message.clone());
                Ok(Some(message))
            }
            Err(RecvTimeoutError::Disconnected) => Ok(None),
            Err(RecvTimeoutError::Timeout) => Err(format!("the server sent nothing in {PATIENCE:?}").into()),
        }
    }

    /// Read until the first snapshot ended.
    fn attached(&mut self) -> TestResult {
        loop {
            match self.next()? {
                Some(AttachServerMessage::SnapshotEnd { .. }) => return Ok(()),
                Some(
                    AttachServerMessage::Role { .. }
                    | AttachServerMessage::SnapshotBegin { .. }
                    | AttachServerMessage::SnapshotChunk { .. },
                ) => {}
                other => return Err(format!("no snapshot before {other:?}").into()),
            }
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

    /// Read until the output holds `bytes` exactly.
    fn expect_bytes(&mut self, bytes: &[u8]) -> TestResult {
        while !self.output.windows(bytes.len()).any(|window| window == bytes) {
            match self.next()? {
                Some(AttachServerMessage::Output { .. } | AttachServerMessage::Resized { .. }) => {}
                other => return Err(format!("no {bytes:?} before {other:?}").into()),
            }
        }
        Ok(())
    }

    /// The roles it was told so far.
    fn roles(&self) -> Vec<(LeadRole, u32)> {
        self.received
            .iter()
            .filter_map(|message| match message {
                AttachServerMessage::Role { role, generation } => Some((*role, *generation)),
                _ => None,
            })
            .collect()
    }

    /// Leave the session running.
    fn detach(mut self) -> TestResult {
        write_message(&mut &*self.stream, &AttachClientMessage::Detach)?;
        // the queued output may still come first, then the end of the connection
        while self.next()?.is_some() {}
        Ok(())
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn managed(nonce: &str, claim: bool) -> Managed {
    Managed {
        nonce: nonce.to_owned(),
        claim,
    }
}

fn no_such_session(name: &str) -> ApiError {
    ApiError {
        kind: ApiErrorKind::NoSuchSession,
        message: format!("no live session named {name:?}"),
    }
}

#[test]
fn sessions_are_listed_with_their_clients_shell_and_creation_time() -> TestResult {
    let server = Server::start()?;
    let mut api = server.api()?;
    assert_eq!(api.list()?, [], "a new namespace holds no session");
    let before = now();
    let mut b = server.attach("b", Some(shell()), None)?;
    b.attached()?;
    let mut a = server.attach("a", Some(shell()), None)?;
    a.attached()?;
    let mut second = server.attach("a", None, None)?;
    second.attached()?;
    let sessions = api.list()?;
    assert_eq!(
        sessions
            .iter()
            .map(|record| (record.name.as_str(), record.clients))
            .collect::<Vec<_>>(),
        [("a", 2), ("b", 1)],
        "sorted by name, with their attached clients"
    );
    for record in &sessions {
        assert!(record.leader_pid.is_some_and(|pid| pid > 0), "{record:?}");
        assert!((before..=now()).contains(&record.created_at), "{record:?}");
    }
    assert_ne!(sessions[0].leader_pid, sessions[1].leader_pid);
    second.detach()?;
    let deadline = Instant::now() + PATIENCE;
    while api.list()?[0].clients != 1 {
        assert!(Instant::now() < deadline, "the detached client is still counted");
        thread::sleep(Duration::from_millis(20));
    }
    let killed = api.kill(&["a", "b"], true)?;
    assert!(
        killed.iter().all(|result| result.outcome == KillOutcome::Killed),
        "{killed:?}"
    );
    api.expect_unlisted("a")?;
    api.expect_unlisted("b")?;
    drop((a, b, api));
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

#[test]
fn the_screen_comes_from_the_servers_terminal_and_its_revision_moves_only_with_it() -> TestResult {
    let server = Server::start()?;
    let mut client = server.attach("screen", Some(shell()), None)?;
    client.attached()?;
    let mut api = server.api()?;
    api.type_text("screen", &format!("{}\r", tagged("shown")))?
        .map_err(|err| err.message)?;
    client.expect("got:shown")?;
    let screen = api.expect_screen("screen", false, "got:shown")?;
    assert_eq!(
        (screen.columns, screen.rows),
        (u32::from(SIZE.cols), u32::from(SIZE.rows))
    );
    assert!(!screen.alternate);
    assert!(screen.cursor_row < screen.rows && screen.cursor_column < screen.columns);
    assert!(screen.revision > 0);
    // nothing written, nothing moved: once the shell is quiet, reads give the same screen and the same revision
    let again = api.settled_screen("screen")?;

    // output moves it
    api.type_text("screen", &format!("{}\r", tagged("more")))?
        .map_err(|err| err.message)?;
    let moved = api.expect_screen("screen", false, "got:more")?;
    assert!(
        moved.revision > again.revision,
        "{} after {}",
        moved.revision,
        again.revision
    );

    let all = scrollback_then_screen(&mut api, "screen")?;
    alternate_screen(&mut api, "screen", all.revision)?;

    assert_eq!(api.kill(&["screen"], true)?[0].outcome, KillOutcome::Killed);
    drop((client, api));
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

/// More lines than the screen of `name` holds: the first only in the scrollback, which `all` reads before the screen.
/// The last read of everything.
fn scrollback_then_screen(api: &mut Api, name: &str) -> TestResult<Screen> {
    let lines = if cfg!(windows) {
        "for /l %i in (1,1,60) do @echo row-%i-end".to_owned()
    } else {
        "i=1; while [ $i -le 60 ]; do echo row-$i-end; i=$((i+1)); done".to_owned()
    };
    api.type_text(name, &format!("{lines}\r"))?.map_err(|err| err.message)?;
    api.expect_screen(name, false, "row-60-end")?;
    let active = api.screen(name, false)?.map_err(|err| err.message)?;
    assert!(!active.text.contains("row-1-end\n"), "{:?}", active.text);
    let all = api.expect_screen(name, true, "row-60-end")?;
    assert!(all.text.contains("row-1-end\n"), "{:?}", all.text);
    assert!(
        all.text.ends_with(active.text.as_str()),
        "the scrollback, then the screen"
    );
    Ok(all)
}

/// The session `name` switched to the alternate screen and back, as its program would: the screen says so, and the
/// revision moves past `revision`.
fn alternate_screen(api: &mut Api, name: &str, revision: u64) -> TestResult {
    api.print(name, b"\x1b[?1049h")?.map_err(|err| err.message)?;
    let alternate = api.screen(name, false)?.map_err(|err| err.message)?;
    assert!(alternate.alternate);
    assert!(alternate.revision > revision);
    api.print(name, b"\x1b[?1049l")?.map_err(|err| err.message)?;
    assert!(!api.screen(name, false)?.map_err(|err| err.message)?.alternate);
    Ok(())
}

#[test]
fn typing_is_queued_without_taking_the_lead() -> TestResult {
    let server = Server::start()?;
    let mut leader = server.attach("typed", Some(shell()), Some(managed("leader-1", false)))?;
    leader.attached()?;
    let mut follower = server.attach("typed", None, Some(managed("follower-1", false)))?;
    follower.attached()?;
    let mut api = server.api()?;
    assert_eq!(
        api.type_text("typed", &format!("{}\r", tagged("typed")))?,
        Ok(ApiResponse::Done)
    );
    leader.expect("got:typed")?;
    follower.expect("got:typed")?;
    assert_eq!(leader.roles(), [(LeadRole::Leader, 1)], "the leader keeps the lead");
    assert_eq!(
        follower.roles(),
        [(LeadRole::Follower, 1)],
        "the follower is not promoted"
    );
    assert_eq!(
        api.type_text("typed", "")?,
        Err(ApiError {
            kind: ApiErrorKind::Failed,
            message: "nothing to type into the live session \"typed\"".to_owned(),
        })
    );
    assert_eq!(api.kill(&["typed"], true)?[0].outcome, KillOutcome::Killed);
    drop((leader, follower, api));
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

#[test]
fn a_print_reaches_every_attached_client_in_the_output() -> TestResult {
    let server = Server::start()?;
    let mut one = server.attach("printed", Some(shell()), None)?;
    one.attached()?;
    let mut two = server.attach("printed", None, None)?;
    two.attached()?;
    let mut api = server.api()?;
    let before = api.screen("printed", false)?.map_err(|err| err.message)?;
    // `ngtermctl clipboard set`'s write: OSC 52 of "copied"
    let write = b"\x1b]52;c;Y29waWVk\x1b\\";
    assert_eq!(api.print("printed", write)?, Ok(ApiResponse::Done));
    one.expect_bytes(write)?;
    two.expect_bytes(write)?;
    let after = api.screen("printed", false)?.map_err(|err| err.message)?;
    assert!(after.revision > before.revision, "a print moves the revision");
    assert_eq!(after.text, before.text, "a clipboard write shows nothing");
    // an empty print writes nothing and moves nothing
    assert_eq!(api.print("printed", b"")?, Ok(ApiResponse::Done));
    assert_eq!(
        api.screen("printed", false)?.map_err(|err| err.message)?.revision,
        after.revision
    );
    assert_eq!(api.kill(&["printed"], true)?[0].outcome, KillOutcome::Killed);
    drop((one, two, api));
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

#[test]
fn a_batch_kills_the_running_and_reports_the_missing_as_gone() -> TestResult {
    let server = Server::start()?;
    let mut first = server.attach("first", Some(shell()), None)?;
    first.attached()?;
    first.detach()?;
    let mut second = server.attach("second", Some(shell()), None)?;
    second.attached()?;
    let mut api = server.api()?;
    let results = api.kill(&["first", "missing", "second"], false)?;
    assert_eq!(
        results,
        [
            KillResult {
                name: "first".to_owned(),
                outcome: KillOutcome::Killed,
            },
            KillResult {
                name: "missing".to_owned(),
                outcome: KillOutcome::Gone,
            },
            KillResult {
                name: "second".to_owned(),
                outcome: KillOutcome::Killed,
            },
        ]
    );
    // the attached client is told the end
    loop {
        match second.next()? {
            Some(AttachServerMessage::Exit { .. }) => break,
            Some(_) => {}
            None => return Err("no exit".into()),
        }
    }
    // a killed session is gone for every request
    for name in ["first", "second"] {
        assert_eq!(api.screen(name, false)?, Err(no_such_session(name)));
        assert_eq!(api.type_text(name, "x")?, Err(no_such_session(name)));
        assert_eq!(api.print(name, b"x")?, Err(no_such_session(name)));
        assert_eq!(api.kill(&[name], true)?[0].outcome, KillOutcome::Gone);
        api.expect_unlisted(name)?;
    }
    drop((second, api));
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

/// A program that ignores the hangup outlives a plain kill, which fails, and not a forced one. On Windows either ends
/// the session's job, the whole tree.
#[cfg(unix)]
#[test]
fn a_program_ignoring_the_hangup_survives_a_plain_kill_and_not_a_forced_one() -> TestResult {
    let server = Server::start()?;
    let stubborn = spec(&["/bin/sh", "-c", "trap '' HUP; while read line; do eval \"$line\"; done"]);
    let mut client = server.attach("stubborn", Some(stubborn), None)?;
    client.attached()?;
    let mut api = server.api()?;
    api.type_text("stubborn", &format!("{}\r", tagged("ready")))?
        .map_err(|err| err.message)?;
    client.expect("got:ready")?;
    assert_eq!(
        api.kill(&["stubborn"], false)?[0].outcome,
        KillOutcome::Failed("the program of the live session \"stubborn\" did not end after its hangup".to_owned())
    );
    assert!(
        api.list()?
            .iter()
            .any(|record| record.name == "stubborn" && record.leader_pid.is_some())
    );
    assert_eq!(api.kill(&["stubborn"], true)?[0].outcome, KillOutcome::Killed);
    api.expect_unlisted("stubborn")?;
    drop((client, api));
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

/// An interactive shell's foreground job: on Unix the shell's job control puts it in a process group of its own, which
/// becomes the terminal's foreground; on Windows the shell's descendants.
#[test]
fn a_shells_foreground_job_is_read_waited_for_and_ended() -> TestResult {
    let server = Server::start()?;
    let interactive = if cfg!(windows) {
        spec(&["cmd.exe", "/d", "/q", "/k"])
    } else {
        spec(&["/bin/sh", "-i"])
    };
    let mut client = server.attach("jobs", Some(interactive), None)?;
    client.attached()?;
    let mut api = server.api()?;
    let shell = api.list()?[0].leader_pid.ok_or("no shell")?;
    let long = if cfg!(windows) {
        "ping -n 300 127.0.0.1 >nul"
    } else {
        "sleep 300"
    };
    api.type_text("jobs", &format!("{long}\r"))?
        .map_err(|err| err.message)?;
    let deadline = Instant::now() + PATIENCE;
    let job: Vec<JobProcess> = loop {
        match api.call(ApiRequest::ForegroundJob { shell })? {
            Ok(ApiResponse::ForegroundJob(Some(job))) if !job.is_empty() => break job,
            Ok(ApiResponse::ForegroundJob(_)) => {}
            other => return Err(format!("no foreground job: {other:?}").into()),
        }
        assert!(Instant::now() < deadline, "the job never started");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(job.iter().all(|process| process.pid != shell), "{job:?}");
    assert_eq!(
        api.call(ApiRequest::JobRunning { job: job.clone() })?,
        Ok(ApiResponse::JobRunning(true))
    );
    assert_eq!(
        api.call(ApiRequest::EndJob { job: job.clone() })?,
        Ok(ApiResponse::Done)
    );
    let deadline = Instant::now() + PATIENCE;
    while api.call(ApiRequest::JobRunning { job: job.clone() })? != Ok(ApiResponse::JobRunning(false)) {
        assert!(Instant::now() < deadline, "the job outlived its end");
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(api.kill(&["jobs"], true)?[0].outcome, KillOutcome::Killed);
    drop((client, api));
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}
