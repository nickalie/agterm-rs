//! The update handoff through the `agterm-session` binary, on every OS: servers started detached from the test, as a
//! client starts them ([`connect_or_start`]), each in a private namespace of its own, sessions created and read over
//! the wire. A handoff keeps the shell, its screen and its scrollback, and an attached client reattaches to the new
//! server; a failed one leaves the old server serving, and a client does not ask for it again; a client of another
//! build hands the server over by itself, with the staged binary replaced while the server runs (on Windows, which
//! cannot replace a running executable, the server runs a private copy); continuous output loses no byte across it;
//! and a server that does not hand over refuses and keeps serving (on Windows, where the `ConPTY` layout gate refuses
//! an unknown layout before any handle is read, as `server::handoff`'s unit tests show).
//!
//! Every session gets the test's environment without the live agterm's `AGTERM_*` and `ZMX_*` variables, and every
//! server is the test's own: no handoff here ever reaches another one.

#[expect(
    dead_code,
    reason = "the helpers serve the pane, lead, snapshot, attach and handoff tests alike"
)]
mod support;

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use agterm_session::build_id;
use agterm_session::endpoint::{self, Endpoint, Stream};
use agterm_session::protocol::{
    ApiCall, ApiReply, ApiRequest, ApiResponse, AttachClientMessage, AttachRequest, AttachServerMessage,
    CAPABILITY_HANDOFF_REQUEST, Control, ErrorCode, GridSize, HandoffRequest, HandoffResult, Hello, PROTOCOL_VERSION,
    ReplyMetadata, Role, SessionSpec, read_frame, write_control, write_message,
};
use agterm_session::server::handoff::Support;
use agterm_session::server::lifecycle::Graces;
use agterm_session::server::spawn::{Connected, HandoffAttempt, StartOptions, connect_existing, connect_or_start};
use agterm_session::server::{self, BUILD_ID, ServerConfig};
use support::{EXE, PATIENCE, SIZE, TestResult, shell, spec, stage};

/// A server's sessions: each one's name and its program's pid.
type Sessions = Vec<(String, Option<i32>)>;

fn hello(role: Role) -> Hello {
    Hello {
        role,
        build: "handoff-test".to_owned(),
        capabilities: Vec::new(),
    }
}

/// A private namespace; dropping it ends its sessions, so its server stops by itself.
struct Namespace {
    _root: tempfile::TempDir,
    endpoint: Endpoint,
}

impl Namespace {
    fn new() -> TestResult<Self> {
        // short on Unix, where a socket path holds about a hundred bytes
        #[cfg(unix)]
        let root = tempfile::Builder::new().prefix("aglh-").tempdir_in("/tmp")?;
        #[cfg(windows)]
        let root = tempfile::tempdir()?;
        let endpoint = Endpoint::new(root.path().join("live"));
        Ok(Self { _root: root, endpoint })
    }

    /// An api call, starting a server from `options` when none runs; the reply and the serving server's pid.
    fn api(&self, options: &StartOptions, request: ApiRequest) -> TestResult<(ApiResponse, u32)> {
        let Connected { stream, welcome, .. } = connect_or_start(&self.endpoint, &hello(Role::Api), options)?;
        let mut stream = stream;
        write_message(&mut stream, &ApiCall { id: 1, request })?;
        let reply: ApiReply = read_frame(&mut stream)?.message(PROTOCOL_VERSION)?;
        let result = reply.result.map_err(|err| format!("{err:?}"))?;
        Ok((result, welcome.pid))
    }

    /// The screen of `name`, all of it, from the serving server's terminal.
    fn screen(&self, name: &str) -> TestResult<String> {
        let request = ApiRequest::Screen {
            name: name.to_owned(),
            all: true,
        };
        match self.api(&StartOptions::new(EXE), request)? {
            (ApiResponse::Screen(screen), _) => Ok(screen.text),
            (other, _) => Err(format!("a screen read answered {other:?}").into()),
        }
    }

    /// Until the screen of `name` holds `needle`; the screen.
    fn wait_screen(&self, name: &str, needle: &str) -> TestResult<String> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let screen = self.screen(name)?;
            if screen.contains(needle) {
                return Ok(screen);
            }
            if Instant::now() >= deadline {
                return Err(format!("{needle:?} never showed in {name}: {screen}").into());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// The serving server's sessions and pid.
    fn sessions(&self) -> TestResult<(Sessions, u32)> {
        match self.api(&StartOptions::new(EXE), ApiRequest::List)? {
            (ApiResponse::Sessions(sessions), pid) => Ok((
                sessions
                    .into_iter()
                    .map(|session| (session.name, session.leader_pid))
                    .collect(),
                pid,
            )),
            (other, _) => Err(format!("a list answered {other:?}").into()),
        }
    }

    fn print(&self, name: &str, text: &str) -> TestResult {
        let request = ApiRequest::Print {
            name: name.to_owned(),
            data: text.as_bytes().to_vec(),
        };
        self.api(&StartOptions::new(EXE), request)?;
        Ok(())
    }

    fn type_line(&self, name: &str, line: &str) -> TestResult {
        let enter = if cfg!(windows) { "\r" } else { "\n" };
        let request = ApiRequest::Type {
            name: name.to_owned(),
            data: format!("{line}{enter}").into_bytes(),
        };
        self.api(&StartOptions::new(EXE), request)?;
        Ok(())
    }

    /// Ask the running server, on a control connection, to hand over to `exe` of `build`; its answer.
    fn hand_over(&self, exe: &Path, build: &str) -> TestResult<Result<(), agterm_session::protocol::ControlError>> {
        let Connected { mut stream, .. } =
            connect_existing(&self.endpoint, &hello(Role::Control), PATIENCE)?.ok_or("no server to hand over")?;
        let request = HandoffRequest {
            import_exe: exe.to_string_lossy().into_owned(),
            expected_build: build.to_owned(),
            expected_protocol: PROTOCOL_VERSION,
        };
        write_control(&mut stream, &Control::HandoffRequest(request))?;
        match read_frame(&mut stream)?.control()? {
            Control::HandoffResult(HandoffResult(result)) => Ok(result),
            other => Err(format!("a handoff request answered {other:?}").into()),
        }
    }
}

impl Drop for Namespace {
    /// End every session, so the server stops by itself (once idle); wait for its process to be gone (a server in
    /// this process, a test's own, stops with its test).
    fn drop(&mut self) {
        let Ok(Some(_)) = connect_existing(&self.endpoint, &hello(Role::Api), Duration::from_secs(5)) else {
            return;
        };
        let Ok((sessions, pid)) = self.sessions() else {
            return;
        };
        let names = sessions.into_iter().map(|(name, _)| name).collect();
        let _ = self.api(&StartOptions::new(EXE), ApiRequest::Kill { names, force: true });
        if pid == std::process::id() {
            return;
        }
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline && support::process_runs(pid) != Some(false) {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// An attached client over the wire: what the server sends arrives on a channel, read by a thread of its own; it
/// starts over from each snapshot, as a client's terminal does.
struct Client {
    stream: Arc<Stream>,
    messages: Receiver<AttachServerMessage>,
    reader: Option<JoinHandle<()>>,
    /// The text received since the last snapshot began, snapshot included.
    text: Vec<u8>,
}

impl Drop for Client {
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

impl Client {
    /// Attach to `name` (creating it with `create`), starting a server from `options` when none runs.
    fn attach(
        namespace: &Namespace,
        options: &StartOptions,
        name: &str,
        create: Option<SessionSpec>,
    ) -> TestResult<Self> {
        let Connected { stream, .. } = connect_or_start(&namespace.endpoint, &hello(Role::Attach), options)?;
        let stream = Arc::new(stream);
        let request = AttachRequest {
            name: name.to_owned(),
            create,
            size: SIZE,
            managed: None,
            metadata: ReplyMetadata::default(),
        };
        write_message(&mut &*stream, &AttachClientMessage::Attach(request))?;
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
        let mut client = Self {
            stream,
            messages,
            reader: Some(reader),
            text: Vec::new(),
        };
        // attached (or refused) once the server answers: the session exists from then on
        let first = client
            .messages
            .recv_timeout(PATIENCE)
            .map_err(|_| format!("the attach to {name} was never answered"))?;
        client.take(first)?;
        Ok(client)
    }

    fn take(&mut self, message: AttachServerMessage) -> TestResult {
        match message {
            AttachServerMessage::SnapshotBegin { .. } => self.text.clear(),
            AttachServerMessage::SnapshotChunk { data, .. } | AttachServerMessage::Output { data, .. } => {
                self.text.extend_from_slice(&data);
            }
            AttachServerMessage::Refused { message, .. } => {
                return Err(format!("the attach was refused: {message}").into());
            }
            _ => {}
        }
        Ok(())
    }

    /// Until the text received holds `needle`.
    fn expect(&mut self, needle: &str) -> TestResult {
        let deadline = Instant::now() + PATIENCE;
        while !String::from_utf8_lossy(&self.text).contains(needle) {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(message) = self.messages.recv_timeout(left) else {
                let text = String::from_utf8_lossy(&self.text).into_owned();
                return Err(format!("{needle:?} never arrived: {text}").into());
            };
            self.take(message)?;
        }
        Ok(())
    }

    /// Resize the session to `size`, as its leader; until the server reports the session at that size.
    fn resize(&mut self, size: GridSize) -> TestResult {
        write_message(&mut &*self.stream, &AttachClientMessage::Resize { size })?;
        loop {
            let message = self
                .messages
                .recv_timeout(PATIENCE)
                .map_err(|_| format!("the resize to {size:?} was never reported"))?;
            if matches!(message, AttachServerMessage::Resized { size: got, .. } if got == size) {
                return Ok(());
            }
            self.take(message)?;
        }
    }

    /// Until the server closed the connection (reading what came before it).
    fn closed(&mut self) -> TestResult {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.messages.recv_timeout(left) {
                Ok(message) => self.take(message)?,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
                Err(RecvTimeoutError::Timeout) => return Err("the server never closed the connection".into()),
            }
        }
    }
}

/// The line that makes [`shell`] print `got:<value of the variable>` (`got^:` or `got"":`, not matched by its echo).
fn print_variable(variable: &str) -> String {
    if cfg!(windows) {
        format!("echo got^:%{variable}%")
    } else {
        format!("echo got\"\":${variable}")
    }
}

fn set_variable(variable: &str, value: &str) -> String {
    if cfg!(windows) {
        format!("set {variable}={value}")
    } else {
        format!("{variable}={value}")
    }
}

/// The line that makes [`shell`] print `line1` … `line<count>`.
fn numbered(count: u32) -> String {
    if cfg!(windows) {
        format!("for /L %i in (1,1,{count}) do @echo line%i")
    } else {
        format!("i=1; while [ $i -le {count} ]; do echo line$i; i=$((i+1)); done")
    }
}

/// Text `print` wrote on the screen, the shell's output after it.
const PRINTED: &str = "printed-mark";

#[test]
fn a_handoff_keeps_the_shell_its_screen_and_its_scrollback() -> TestResult {
    let namespace = Namespace::new()?;
    let options = StartOptions::new(EXE);
    let mut before = Client::attach(&namespace, &options, "kept", Some(shell()))?;
    namespace.type_line("kept", &set_variable("MARK", "kept-state"))?;
    namespace.type_line("kept", &numbered(80))?;
    namespace.wait_screen("kept", "line80")?;
    before.expect("line80")?;
    namespace.print("kept", PRINTED)?;
    namespace.type_line("kept", &support::tagged("printed"))?;
    namespace.wait_screen("kept", "got:printed")?;
    let (sessions, old) = namespace.sessions()?;
    assert_eq!(sessions.len(), 1, "{sessions:?}");

    assert_eq!(namespace.hand_over(Path::new(EXE), BUILD_ID)?, Ok(()));

    // the client attached before is disconnected, and reattaches to the new server
    before.closed()?;
    let (after, new) = namespace.sessions()?;
    assert_ne!(new, old, "another server serves");
    assert_eq!(after, sessions, "the same session, its shell the same process");
    let screen = namespace.screen("kept")?;
    for line in ["line1\n", "line40\n", "line80", PRINTED] {
        assert!(screen.contains(line), "{line:?} lost from the scrollback: {screen}");
    }
    let mut again = Client::attach(&namespace, &options, "kept", None)?;
    again.expect("line80")?;
    namespace.type_line("kept", &print_variable("MARK"))?;
    again.expect("got:kept-state")?;
    let screen = namespace.wait_screen("kept", "got:kept-state")?;
    assert!(screen.contains(PRINTED), "the printed text did not last: {screen}");

    // the new server drives the PTY it took over: a real resize reaches the shell
    let size = GridSize {
        cols: SIZE.cols + 20,
        rows: SIZE.rows + 10,
    };
    again.resize(size)?;
    namespace.type_line("kept", &support::size_line())?;
    namespace.wait_screen("kept", &format!("SIZE={}x{}", size.rows, size.cols))?;
    Ok(())
}

#[test]
fn a_failed_handoff_leaves_the_old_server_serving() -> TestResult {
    let namespace = Namespace::new()?;
    let options = StartOptions::new(EXE);
    let created = Client::attach(&namespace, &options, "stays", Some(shell()))?;
    namespace.type_line("stays", &set_variable("MARK", "still-here"))?;
    let (sessions, old) = namespace.sessions()?;

    let refused = namespace
        .hand_over(Path::new(EXE), "a build no replacement is")?
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::HandoffFailed);
    assert!(refused.message.contains("expected build"), "{}", refused.message);
    drop(created);

    let (after, pid) = namespace.sessions()?;
    assert_eq!(pid, old, "the old server serves");
    assert_eq!(after, sessions);
    let mut client = Client::attach(&namespace, &options, "stays", None)?;
    namespace.type_line("stays", &print_variable("MARK"))?;
    client.expect("got:still-here")?;
    // a replacement that cannot be started fails the same way
    let missing = namespace.hand_over(&std::env::temp_dir().join("no-such-agterm-session"), BUILD_ID)?;
    assert!(missing.is_err());
    assert_eq!(namespace.sessions()?.1, old);
    Ok(())
}

/// A client of a build no replacement can be asks the server once: its failed attempt is reported again to a later
/// call, without a second handoff that would disconnect the clients attached meanwhile.
#[test]
fn a_failed_handoff_is_not_asked_again_and_the_clients_stay_attached() -> TestResult {
    let namespace = Namespace::new()?;
    let plain = StartOptions::new(EXE);
    let mut before = Client::attach(&namespace, &plain, "once", Some(shell()))?;
    let unreachable = StartOptions {
        build: Some("a build no replacement is".to_owned()),
        ..StartOptions::new(EXE)
    };
    let first = connect_or_start(&namespace.endpoint, &hello(Role::Api), &unreachable)?;
    let Some(HandoffAttempt::Failed { from, reason }) = first.handoff.clone() else {
        return Err(format!("the handoff did not fail: {:?}", first.handoff).into());
    };
    assert_eq!(from, BUILD_ID);
    assert!(reason.contains("expected build"), "{reason}");
    drop(first);
    // the attempt disconnected the client attached before; it reattaches
    before.closed()?;
    let mut after = Client::attach(&namespace, &plain, "once", None)?;

    let second = connect_or_start(&namespace.endpoint, &hello(Role::Api), &unreachable)?;
    assert_eq!(second.handoff, Some(HandoffAttempt::Failed { from, reason }));
    let (_, pid) = namespace.sessions()?;
    assert_eq!(second.welcome.pid, pid, "the old server serves");
    drop(second);
    // asked nothing, the server left the reattached client attached
    namespace.type_line("once", &support::tagged("still-attached"))?;
    after.expect("got:still-attached")?;
    Ok(())
}

#[test]
fn a_client_of_another_build_hands_the_server_over_and_the_staged_binary_can_be_replaced_meanwhile() -> TestResult {
    let namespace = Namespace::new()?;
    let staged = tempfile::tempdir()?;
    let first = stage(staged.path(), "build-a")?;
    let options = StartOptions::new(&first);
    assert_eq!(options.build.as_deref(), Some("build-a"));
    let _created = Client::attach(&namespace, &options, "carried", Some(shell()))?;
    namespace.type_line("carried", &set_variable("MARK", "carried-over"))?;
    let (sessions, old) = namespace.sessions()?;

    // the staged binary replaced while the server runs
    let second = stage(staged.path(), "build-b")?;
    assert_eq!(second, first);
    let options = StartOptions::new(&second);
    assert_eq!(options.build.as_deref(), Some("build-b"));
    let connected = connect_or_start(&namespace.endpoint, &hello(Role::Api), &options)?;
    assert_eq!(
        connected.handoff,
        Some(HandoffAttempt::Committed {
            from: "build-a".to_owned()
        })
    );
    assert_eq!(connected.welcome.build, "build-b");
    assert_ne!(connected.welcome.pid, old);
    drop(connected);
    assert_eq!(namespace.sessions()?.0, sessions);
    let mut client = Client::attach(&namespace, &StartOptions::new(EXE), "carried", None)?;
    namespace.type_line("carried", &print_variable("MARK"))?;
    client.expect("got:carried-over")?;

    // the same build found: nothing to hand over
    let same = connect_or_start(&namespace.endpoint, &hello(Role::Api), &options)?;
    assert_eq!(same.handoff, None);
    Ok(())
}

#[test]
fn an_update_hands_a_running_server_of_another_build_over_and_starts_none() -> TestResult {
    let namespace = Namespace::new()?;
    let staged = tempfile::tempdir()?;
    let first = stage(staged.path(), "build-a")?;
    let update = agterm_session::client::Client::new(namespace.endpoint.directory());
    // no server: nothing to hand over, and none is started
    assert_eq!(update.update(&StartOptions::new(&first))?, None);
    assert!(connect_existing(&namespace.endpoint, &hello(Role::Api), PATIENCE)?.is_none());
    let _created = Client::attach(&namespace, &StartOptions::new(&first), "updated", Some(shell()))?;
    let (sessions, old) = namespace.sessions()?;
    // the server's own build: nothing either
    assert_eq!(update.update(&StartOptions::new(&first))?, None);
    // an unstaged executable expects no build: no handoff asked
    let mut unstaged = StartOptions::new(EXE);
    unstaged.build = None;
    assert_eq!(update.update(&unstaged)?, None);
    // the app launching a new build hands the sessions over to it
    let second = stage(staged.path(), "build-b")?;
    assert_eq!(
        update.update(&StartOptions::new(&second))?,
        Some(HandoffAttempt::Committed {
            from: "build-a".to_owned()
        })
    );
    let (now, pid) = namespace.sessions()?;
    assert_eq!(now, sessions);
    assert_ne!(pid, old);
    assert_eq!(update.update(&StartOptions::new(&second))?, None, "and once only");
    Ok(())
}

#[test]
fn continuous_output_across_a_handoff_loses_no_byte() -> TestResult {
    const COUNT: u32 = 300;
    let namespace = Namespace::new()?;
    let options = StartOptions::new(EXE);
    let program = if cfg!(windows) {
        spec(&[
            "powershell.exe",
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "1..{COUNT} | ForEach-Object {{ 'n' + $_; Start-Sleep -Milliseconds 10 }}; 'done'; Start-Sleep 600"
            ),
        ])
    } else {
        spec(&[
            "/bin/sh",
            "-c",
            &format!("i=1; while [ $i -le {COUNT} ]; do echo n$i; i=$((i+1)); sleep 0.01; done; echo done; sleep 600"),
        ])
    };
    let _created = Client::attach(&namespace, &options, "stream", Some(program))?;
    namespace.wait_screen("stream", "n20\n")?;
    assert_eq!(namespace.hand_over(Path::new(EXE), BUILD_ID)?, Ok(()));
    let screen = namespace.wait_screen("stream", "done")?;
    let numbers: Vec<u32> = screen
        .lines()
        .filter_map(|line| line.trim().strip_prefix('n')?.parse().ok())
        .collect();
    let expected: Vec<u32> = (1..=COUNT).collect();
    assert_eq!(numbers, expected, "{screen}");
    Ok(())
}

#[test]
fn a_server_that_does_not_hand_over_refuses_and_keeps_serving() -> TestResult {
    let namespace = Namespace::new()?;
    let directory = namespace.endpoint.directory().to_path_buf();
    let config = ServerConfig {
        handoff: Support::Refused("the test's server does not hand over".to_owned()),
        graces: Graces {
            startup: PATIENCE,
            idle: Duration::from_millis(100),
        },
        ..ServerConfig::default()
    };
    let server = thread::spawn(move || server::run(&directory, &config));
    // the test's own server listens before any client could start another
    let deadline = Instant::now() + PATIENCE;
    while connect_existing(&namespace.endpoint, &hello(Role::Control), PATIENCE)?.is_none() {
        assert!(Instant::now() < deadline, "the test's server never listened");
        thread::sleep(Duration::from_millis(20));
    }
    let options = StartOptions::new(EXE);
    let _created = Client::attach(&namespace, &options, "refuser", Some(shell()))?;
    let connected = connect_existing(&namespace.endpoint, &hello(Role::Control), PATIENCE)?.ok_or("no server")?;
    assert!(
        !connected
            .welcome
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_HANDOFF_REQUEST),
        "{:?}",
        connected.welcome.capabilities
    );
    drop(connected);
    let refused = namespace.hand_over(Path::new(EXE), BUILD_ID)?.unwrap_err();
    assert_eq!(refused.code, ErrorCode::HandoffFailed);
    assert!(refused.message.contains("does not hand over"), "{}", refused.message);
    let mut client = Client::attach(&namespace, &options, "refuser", None)?;
    namespace.type_line("refuser", &support::tagged("served"))?;
    client.expect("got:served")?;
    drop(client);
    drop(namespace);
    assert!(server.join().is_ok());
    Ok(())
}

#[test]
fn this_hosts_server_advertises_its_handoff() -> TestResult {
    let namespace = Namespace::new()?;
    let connected = connect_or_start(&namespace.endpoint, &hello(Role::Control), &StartOptions::new(EXE))?;
    let advertised = connected
        .welcome
        .capabilities
        .iter()
        .any(|capability| capability == CAPABILITY_HANDOFF_REQUEST);
    assert_eq!(advertised, Support::of_this_host() == Support::Supported);
    // the runners' Windows passes the ConPTY layout gate; one that does not would fail here, naming its ConPTY
    assert_eq!(Support::of_this_host(), Support::Supported);
    assert_eq!(build_id::of_this_process(), build_id::UNSTAGED);
    Ok(())
}
