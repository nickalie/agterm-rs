//! The `agterm-session` binary driven as its users drive it, for the cli and e2e tests: a private namespace that only
//! `AGTERM_LIVE_DIR` names, the api commands run to their end, and `attach` as a terminal client on a real PTY, the
//! test standing in for the outer terminal (it answers the client's probe, and keeps what reaches it). Every child
//! runs with the test's environment without the live agterm's `AGTERM_*` and `ZMX_*` variables.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use agterm_pty::{Actor, ActorConfig, ActorHandle, Child, Size, Spawned};
use agterm_session::client::Client;
use agterm_session::endpoint::Endpoint;
use agterm_session::protocol::{AttachRequest, GridSize, Hello, ReplyMetadata, Role, SessionSpec, Welcome};
use agterm_session::server::lifecycle::IDLE_GRACE;
use agterm_session::server::spawn::{StartOptions, connect_existing};
use tempfile::TempDir;

use crate::support::{self, EXE, PATIENCE, TestResult, shown};

/// The outer terminal's grid.
pub const ROWS: u16 = 24;
pub const COLS: u16 = 80;

/// A private namespace, its commands run by `executable`; its sessions are killed when it is dropped.
pub struct Namespace {
    _root: Option<TempDir>,
    pub directory: PathBuf,
    /// The binary every command and attach runs, and every server start starts.
    pub executable: PathBuf,
    /// A real server's namespace, whose server is waited for at the end; a scripted peer's is not.
    server: bool,
}

impl Namespace {
    pub fn new() -> TestResult<Self> {
        // short on Unix, where a socket path holds about a hundred bytes
        #[cfg(unix)]
        let root = tempfile::Builder::new().prefix("agcli-").tempdir_in("/tmp")?;
        #[cfg(windows)]
        let root = tempfile::tempdir()?;
        let directory = root.path().join("live");
        Ok(Self {
            _root: Some(root),
            directory,
            executable: PathBuf::from(EXE),
            server: true,
        })
    }

    /// A scripted peer's namespace, at its `directory`.
    pub fn of_peer(directory: &Path) -> Self {
        Self {
            _root: None,
            directory: directory.to_owned(),
            executable: PathBuf::from(EXE),
            server: false,
        }
    }

    /// The test's environment without the live agterm's variables, this namespace named, and `extra`.
    pub fn environment(&self, extra: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        let mut environment: Vec<(OsString, OsString)> = std::env::vars_os()
            .filter(|(name, _)| {
                let name = name.to_string_lossy().to_ascii_uppercase();
                !name.starts_with("AGTERM_") && !name.starts_with("ZMX_")
            })
            .collect();
        environment.push(("AGTERM_LIVE_DIR".into(), self.directory.clone().into()));
        environment.push(("TERM".into(), "xterm-256color".into()));
        environment.extend(extra.iter().map(|(name, value)| ((*name).into(), (*value).into())));
        environment
    }

    /// `agterm-session <args>` with `stdin`, to its end.
    pub fn run(&self, args: &[&str], stdin: &[u8], extra: &[(&str, &str)]) -> TestResult<Output> {
        let mut child = Command::new(&self.executable)
            .args(args)
            .env_clear()
            .envs(self.environment(extra))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut input) = child.stdin.take() {
            input.write_all(stdin)?;
        }
        Ok(child.wait_with_output()?)
    }

    /// Creates the session `name` running `spec` through the client library, and detaches.
    pub fn create(&self, name: &str, spec: SessionSpec) -> TestResult {
        let attachment = Client::new(&self.directory)
            .starting(StartOptions::new(&self.executable))
            .attach(AttachRequest {
                name: name.to_owned(),
                create: Some(spec),
                size: GridSize { cols: COLS, rows: ROWS },
                managed: None,
                metadata: ReplyMetadata::default(),
            })?;
        attachment.writer.detach()?;
        Ok(())
    }

    /// The running server's welcome (its build and pid), asked on an api connection; `None` when none runs.
    pub fn server(&self) -> TestResult<Option<Welcome>> {
        Ok(Client::new(&self.directory).api()?.map(|api| api.welcome))
    }

    /// The text of `name`'s screen, waited for until `done` holds.
    pub fn screen_until(&self, name: &str, done: impl Fn(&str) -> bool) -> String {
        support::wait_for(
            &format!("the screen of {name}"),
            || {
                Client::new(&self.directory)
                    .api()
                    .ok()
                    .flatten()
                    .and_then(|mut api| api.screen(name, false).ok())
                    .map(|screen| format!("{}x{}\n{}", screen.columns, screen.rows, screen.text))
                    .unwrap_or_default()
            },
            |text| done(text),
        )
    }

    pub fn session_names(&self) -> Vec<String> {
        Client::new(&self.directory)
            .api()
            .ok()
            .flatten()
            .and_then(|mut api| api.list().ok())
            .map(|sessions| sessions.into_iter().map(|session| session.name).collect())
            .unwrap_or_default()
    }

    /// `agterm-session attach <args>` on a PTY of its own, the test its outer terminal answering as `answers` says.
    pub fn attach<S: AsRef<str>>(&self, args: &[S], extra: &[(&str, &str)], answers: &Answers) -> TestResult<Outer> {
        let command = agterm_pty::Command::new(&self.executable)
            .arg("attach")
            .args(args.iter().map(AsRef::as_ref));
        Outer::spawn(self, command, extra, answers)
    }

    /// `program <args>` on a PTY of its own in this namespace's environment, the test its outer terminal: what runs
    /// an attach of its own (a remote attach line, say).
    pub fn on_terminal<S: AsRef<str>>(
        &self,
        program: &str,
        args: &[S],
        extra: &[(&str, &str)],
        answers: &Answers,
    ) -> TestResult<Outer> {
        let command = agterm_pty::Command::new(program).args(args.iter().map(AsRef::as_ref));
        Outer::spawn(self, command, extra, answers)
    }
}

impl Drop for Namespace {
    fn drop(&mut self) {
        if !self.server {
            return;
        }
        let client = Client::new(&self.directory);
        if let Ok(Some(mut api)) = client.api()
            && let Ok(sessions) = api.list()
        {
            let names: Vec<String> = sessions.into_iter().map(|session| session.name).collect();
            let _ = api.kill(&names, true);
        }
        // the server stops by itself once it held no session and no client for its idle grace; each look is a client,
        // so the next look waits that grace out
        let endpoint = Endpoint::new(&self.directory);
        let hello = Hello {
            role: Role::Control,
            build: "cli-test".to_owned(),
            capabilities: Vec::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline
            && matches!(connect_existing(&endpoint, &hello, Duration::from_secs(1)), Ok(Some(_)))
        {
            std::thread::sleep(IDLE_GRACE + Duration::from_secs(1));
        }
    }
}

/// What the test answers as the outer terminal.
#[derive(Debug, Clone, Default)]
pub struct Answers {
    /// OSC 10 and 11, as X11 colour specs.
    pub foreground: Option<&'static str>,
    pub background: Option<&'static str>,
    /// An OSC 52 read's clipboard.
    pub clipboard: Option<&'static str>,
}

impl Answers {
    /// The cursor, DA1 and the cell size: what every terminal answers.
    pub fn basic() -> Self {
        Self::default()
    }

    /// The queries this outer terminal answers, and their answers.
    fn table(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        use base64::Engine as _;
        let mut table = vec![
            // the pseudoconsole asks for the cursor before it runs the client (Windows); the client asks for none
            (b"\x1b[6n".to_vec(), b"\x1b[1;1R".to_vec()),
            (b"\x1b[c".to_vec(), b"\x1b[?62;22c".to_vec()),
            (b"\x1b[16t".to_vec(), b"\x1b[6;20;10t".to_vec()),
        ];
        if let Some(color) = self.foreground {
            table.push((b"\x1b]10;?".to_vec(), format!("\x1b]10;{color}\x1b\\").into_bytes()));
        }
        if let Some(color) = self.background {
            table.push((b"\x1b]11;?".to_vec(), format!("\x1b]11;{color}\x1b\\").into_bytes()));
        }
        if let Some(text) = self.clipboard {
            let data = base64::engine::general_purpose::STANDARD.encode(text);
            table.push((b"\x1b]52;c;?".to_vec(), format!("\x1b]52;c;{data}\x1b\\").into_bytes()));
        }
        table
    }
}

/// The read callback of the outer terminal: everything read kept, each query of `answers` answered in order.
fn outer_terminal(output: Arc<Mutex<Vec<u8>>>, answers: &Answers) -> agterm_pty::ReadCallback {
    let table = answers.table();
    let longest = table.iter().map(|(query, _)| query.len()).max().unwrap_or(1);
    let mut tail: Vec<u8> = Vec::new();
    Box::new(move |bytes| {
        output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        tail.extend_from_slice(bytes);
        let mut replies = Vec::new();
        let mut at = 0;
        loop {
            let next = table
                .iter()
                .filter_map(|(query, reply)| {
                    tail[at..]
                        .windows(query.len())
                        .position(|window| window == query.as_slice())
                        .map(|found| (at + found, query.len(), reply))
                })
                .min_by_key(|(found, ..)| *found);
            let Some((found, length, reply)) = next else {
                break;
            };
            replies.push(reply.clone());
            at = found + length;
        }
        let keep_from = tail.len().saturating_sub(longest - 1).max(at);
        tail.drain(..keep_from);
        replies
    })
}

/// `agterm-session attach` (or what runs one) on a PTY, with the test as its outer terminal.
pub struct Outer {
    handle: ActorHandle,
    pub child: Child,
    output: Arc<Mutex<Vec<u8>>>,
    exited: Option<u32>,
}

impl Outer {
    fn spawn(
        namespace: &Namespace,
        command: agterm_pty::Command,
        extra: &[(&str, &str)],
        answers: &Answers,
    ) -> TestResult<Self> {
        let mut command = command.env_clear();
        for (name, value) in namespace.environment(extra) {
            command = command.env(name, value);
        }
        let Spawned { master, child } = agterm_pty::spawn(&command, Size::cells(ROWS, COLS))?;
        let output = Arc::new(Mutex::new(Vec::new()));
        let config = ActorConfig::new("cli-test", master, outer_terminal(Arc::clone(&output), answers));
        Ok(Self {
            handle: Actor::spawn(config)?,
            child,
            output,
            exited: None,
        })
    }

    pub fn output(&self) -> Vec<u8> {
        self.output.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// What the outer terminal shows: the output without its control strings.
    pub fn text(&self) -> String {
        shown(&self.output())
    }

    /// Waits for `needle` in what the outer terminal shows.
    pub fn expect(&self, needle: &str) {
        support::wait_for(
            &format!("{needle:?} in the outer terminal"),
            || self.text(),
            |text| text.contains(needle),
        );
    }

    /// Waits for `needle` among the bytes the outer terminal read.
    #[cfg(unix)]
    pub fn expect_bytes(&self, needle: &[u8]) {
        support::wait_for(
            &format!("{:?} among the outer terminal's bytes", String::from_utf8_lossy(needle)),
            || String::from_utf8_lossy(&self.output()).into_owned(),
            |output| output.contains(&*String::from_utf8_lossy(needle)),
        );
    }

    pub fn write(&self, bytes: &[u8]) -> TestResult {
        self.handle.write_input(bytes)?;
        Ok(())
    }

    pub fn type_line(&self, line: &str) -> TestResult {
        self.write(format!("{line}\r").as_bytes())
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        self.handle.resize(Size::cells(rows, cols));
    }

    /// Waits for the client to end; its exit status.
    pub fn wait(&mut self) -> TestResult<u32> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait()? {
                let code = status.exit_code();
                self.exited = Some(code);
                return Ok(code);
            }
            if Instant::now() >= deadline {
                return Err(format!("the client never ended; it showed {:?}", self.text()).into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Ends the client's process tree at once, as a crash or a closed terminal window does: no detach is sent, and the
    /// server sees only its connection end.
    pub fn kill(&mut self) -> TestResult {
        self.child.killer().kill_tree()?;
        let status = self.child.wait()?;
        self.exited = Some(status.exit_code());
        Ok(())
    }
}

impl Drop for Outer {
    fn drop(&mut self) {
        if self.exited.is_none() {
            let _ = self.child.killer().kill_tree();
            let _ = self.child.wait();
        }
        self.handle.child_exited();
        self.handle.shutdown();
    }
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The program of a session that runs each line typed into it (`support::shell`), as argv words.
pub fn shell_argv() -> Vec<String> {
    support::shell().argv
}

/// An attach's arguments: the session `name`, then [`shell_argv`].
pub fn with_shell(name: &str) -> Vec<String> {
    std::iter::once(name.to_owned()).chain(shell_argv()).collect()
}

pub fn spec(argv: Vec<String>) -> SessionSpec {
    SessionSpec {
        argv,
        env: support::environment(),
        cwd: None,
        tracked_env: Vec::new(),
    }
}
