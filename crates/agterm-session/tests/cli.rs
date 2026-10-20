//! The `agterm-session` binary's client commands, each test in a private namespace that only `AGTERM_LIVE_DIR` names,
//! every child run with the test's environment without the live agterm's `AGTERM_*` and `ZMX_*` variables: the api
//! commands' output and exit status, and `attach` as a terminal client on a real PTY, the test standing in for the
//! outer terminal (it answers the client's probe, and reads what reaches it): a round trip with a resize, detach and
//! reattach, the gone guard, the role title on a lead change, `AGTERM_LIVE_SESSION`, and (on Unix, where no
//! pseudoconsole answers the program's queries itself, `server::replies`) the reply owner: split queries with two
//! clients attached, the outer terminal's colours, an OSC 52 read relayed.
//!
//! Every session a test leaves is killed through the api at its end, after which the namespace's server stops by
//! itself.

#[expect(
    dead_code,
    reason = "the helpers serve the pane, lead, snapshot, attach, api, client, host and cli tests alike"
)]
mod support;

#[expect(dead_code, reason = "the helpers serve the client, host and cli tests alike")]
mod peer;

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use agterm_pty::{Actor, ActorConfig, ActorHandle, Child, Size, Spawned};
use agterm_session::client::Client;
use agterm_session::endpoint::Endpoint;
use agterm_session::protocol::{AttachRequest, GridSize, Hello, ReplyMetadata, Role, SessionSpec};
use agterm_session::server::lifecycle::IDLE_GRACE;
use agterm_session::server::spawn::{StartOptions, connect_existing};
use peer::Peer;
use support::{PATIENCE, TestResult, shown, tagged};
use tempfile::TempDir;

/// The binary under test.
const EXE: &str = env!("CARGO_BIN_EXE_agterm-session");

/// The outer terminal's grid.
const ROWS: u16 = 24;
const COLS: u16 = 80;

/// A private namespace; its sessions are killed when it is dropped.
struct Namespace {
    _root: TempDir,
    directory: PathBuf,
    /// A real server's namespace, whose server is waited for at the end; a scripted peer's is not.
    server: bool,
}

impl Namespace {
    fn new() -> TestResult<Self> {
        // short on Unix, where a socket path holds about a hundred bytes
        #[cfg(unix)]
        let root = tempfile::Builder::new().prefix("agcli-").tempdir_in("/tmp")?;
        #[cfg(windows)]
        let root = tempfile::tempdir()?;
        let directory = root.path().join("live");
        Ok(Self {
            _root: root,
            directory,
            server: true,
        })
    }

    /// A scripted peer's namespace.
    fn of_peer(peer: &Peer) -> TestResult<Self> {
        Ok(Self {
            _root: tempfile::tempdir()?,
            directory: peer.directory().to_owned(),
            server: false,
        })
    }

    /// The test's environment without the live agterm's variables, this namespace named, and `extra`.
    fn environment(&self, extra: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
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
    fn run(&self, args: &[&str], stdin: &[u8], extra: &[(&str, &str)]) -> TestResult<Output> {
        let mut child = Command::new(EXE)
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
    fn create(&self, name: &str, spec: SessionSpec) -> TestResult {
        let attachment = Client::new(&self.directory)
            .starting(StartOptions::new(EXE))
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

    /// The text of `name`'s screen, waited for until `done` holds.
    fn screen_until(&self, name: &str, done: impl Fn(&str) -> bool) -> String {
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

    fn session_names(&self) -> Vec<String> {
        Client::new(&self.directory)
            .api()
            .ok()
            .flatten()
            .and_then(|mut api| api.list().ok())
            .map(|sessions| sessions.into_iter().map(|session| session.name).collect())
            .unwrap_or_default()
    }

    /// `agterm-session attach <args>` on a PTY of its own, the test its outer terminal answering as `answers` says.
    fn attach<S: AsRef<str>>(&self, args: &[S], extra: &[(&str, &str)], answers: &Answers) -> TestResult<Outer> {
        Outer::spawn(self, args, extra, answers)
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
struct Answers {
    /// OSC 10 and 11, as X11 colour specs.
    foreground: Option<&'static str>,
    background: Option<&'static str>,
    /// An OSC 52 read's clipboard.
    clipboard: Option<&'static str>,
}

impl Answers {
    /// The cursor, DA1 and the cell size: what every terminal answers.
    fn basic() -> Self {
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

/// `agterm-session attach` on a PTY, with the test as its outer terminal.
struct Outer {
    handle: ActorHandle,
    child: Child,
    output: Arc<Mutex<Vec<u8>>>,
    exited: Option<u32>,
}

impl Outer {
    fn spawn<S: AsRef<str>>(
        namespace: &Namespace,
        args: &[S],
        extra: &[(&str, &str)],
        answers: &Answers,
    ) -> TestResult<Self> {
        let mut command = agterm_pty::Command::new(EXE)
            .arg("attach")
            .args(args.iter().map(AsRef::as_ref))
            .env_clear();
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

    fn output(&self) -> Vec<u8> {
        self.output.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// What the outer terminal shows: the output without its control strings.
    fn text(&self) -> String {
        shown(&self.output())
    }

    /// Waits for `needle` in what the outer terminal shows.
    fn expect(&self, needle: &str) {
        support::wait_for(
            &format!("{needle:?} in the outer terminal"),
            || self.text(),
            |text| text.contains(needle),
        );
    }

    /// Waits for `needle` among the bytes the outer terminal read.
    #[cfg(unix)]
    fn expect_bytes(&self, needle: &[u8]) {
        support::wait_for(
            &format!("{:?} among the outer terminal's bytes", String::from_utf8_lossy(needle)),
            || String::from_utf8_lossy(&self.output()).into_owned(),
            |output| output.contains(&*String::from_utf8_lossy(needle)),
        );
    }

    fn write(&self, bytes: &[u8]) -> TestResult {
        self.handle.write_input(bytes)?;
        Ok(())
    }

    fn type_line(&self, line: &str) -> TestResult {
        self.write(format!("{line}\r").as_bytes())
    }

    fn resize(&self, rows: u16, cols: u16) {
        self.handle.resize(Size::cells(rows, cols));
    }

    /// Waits for the client to end; its exit status.
    fn wait(&mut self) -> TestResult<u32> {
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The program of a session that runs each line typed into it (`support::shell`), as argv words.
fn shell_argv() -> Vec<String> {
    support::shell().argv
}

/// An attach's arguments: the session `name`, then [`shell_argv`].
fn with_shell(name: &str) -> Vec<String> {
    std::iter::once(name.to_owned()).chain(shell_argv()).collect()
}

/// The argv of the gone guard (`agterm-core::remote::ssh`): a session that is gone runs it and fails.
fn gone_guard() -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["cmd.exe", "/d", "/c", "echo agterm: remote session is gone& exit /b 1"]
    } else {
        vec![
            "/bin/sh",
            "-c",
            "printf '%s\\n' 'agterm: remote session is gone'; exit 1",
        ]
    }
}

fn spec(argv: Vec<String>) -> SessionSpec {
    SessionSpec {
        argv,
        env: support::environment(),
        cwd: None,
        tracked_env: Vec::new(),
    }
}

#[test]
fn the_api_commands_print_and_exit_as_documented() -> TestResult {
    let namespace = Namespace::new()?;
    without_a_session(&namespace)?;
    namespace.create("cmd", spec(shell_argv()))?;
    with_a_session(&namespace)?;
    let killed = namespace.run(&["kill", "--force", "cmd", "other"], b"", &[])?;
    assert_eq!(killed.status.code(), Some(1), "one of the two was no session");
    assert_eq!(stdout(&killed), "killed session cmd\n");
    assert!(
        stderr(&killed).contains("no live session named other"),
        "{}",
        stderr(&killed)
    );
    assert_eq!(stdout(&namespace.run(&["list", "--short"], b"", &[])?), "");
    Ok(())
}

/// The commands where no server runs: nothing to list, no session to read or kill, usage errors.
fn without_a_session(namespace: &Namespace) -> TestResult {
    let version = namespace.run(&["version"], b"", &[])?;
    assert_eq!(version.status.code(), Some(0));
    assert!(stdout(&version).starts_with("agterm-session\t"), "{}", stdout(&version));
    // no server: no session, and nothing started
    let empty = namespace.run(&["list"], b"", &[])?;
    assert_eq!(empty.status.code(), Some(0));
    assert_eq!(stdout(&empty), "");
    assert!(
        stderr(&empty).starts_with("no sessions found in "),
        "{}",
        stderr(&empty)
    );
    let missing = namespace.run(&["screen", "none"], b"", &[])?;
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        stderr(&missing).contains("no live session named none"),
        "{}",
        stderr(&missing)
    );
    assert_eq!(namespace.run(&["kill", "none"], b"", &[])?.status.code(), Some(1));
    assert_eq!(namespace.run(&["bogus"], b"", &[])?.status.code(), Some(2));
    assert_eq!(namespace.run(&["screen"], b"", &[])?.status.code(), Some(2));
    let unset = namespace.run(&["list"], b"", &[("AGTERM_LIVE_DIR", "")])?;
    assert_eq!(unset.status.code(), Some(2));
    assert!(
        stderr(&unset).contains("AGTERM_LIVE_DIR is not set"),
        "{}",
        stderr(&unset)
    );
    Ok(())
}

/// The commands on the session `cmd`: listed, typed and printed into, its screen read.
fn with_a_session(namespace: &Namespace) -> TestResult {
    let listed = namespace.run(&["list"], b"", &[])?;
    assert_eq!(listed.status.code(), Some(0));
    assert!(stdout(&listed).starts_with("name=cmd\tpid="), "{}", stdout(&listed));
    assert_eq!(stdout(&namespace.run(&["list", "--short"], b"", &[])?), "cmd\n");
    // the prefix comes before every name
    let prefixed = namespace.run(&["screen", "d"], b"", &[("AGTERM_LIVE_SESSION_PREFIX", "cm")])?;
    assert_eq!(prefixed.status.code(), Some(0), "{}", stderr(&prefixed));

    let typed = namespace.run(&["type", "cmd"], format!("{}\r", tagged("typed")).as_bytes(), &[])?;
    assert_eq!(typed.status.code(), Some(0), "{}", stderr(&typed));
    namespace.screen_until("cmd", |text| text.contains("got:typed"));
    assert_eq!(namespace.run(&["type", "cmd"], b"", &[])?.status.code(), Some(1));
    let printed = namespace.run(&["print", "cmd", "printed", "words"], b"", &[])?;
    assert_eq!(printed.status.code(), Some(0), "{}", stderr(&printed));
    namespace.screen_until("cmd", |text| text.contains("printed words"));
    let from_stdin = namespace.run(&["print", "cmd"], b"from-stdin", &[])?;
    assert_eq!(from_stdin.status.code(), Some(0), "{}", stderr(&from_stdin));
    namespace.screen_until("cmd", |text| text.contains("from-stdin"));

    let screen = namespace.run(&["screen", "--all", "cmd"], b"", &[])?;
    assert_eq!(screen.status.code(), Some(0));
    let text = stdout(&screen);
    let (header, body) = text.split_once('\n').ok_or("no header")?;
    let fields: Vec<&str> = header.split(' ').collect();
    assert_eq!(fields.len(), 6, "{header}");
    assert_eq!(&fields[1..3], [COLS.to_string(), ROWS.to_string()]);
    assert!(body.contains("got:typed") && body.contains("from-stdin"), "{body}");
    Ok(())
}

#[test]
fn attach_round_trips_resizes_detaches_and_reattaches_to_its_snapshot() -> TestResult {
    let namespace = Namespace::new()?;
    let args = with_shell("trip");
    // an inherited session variable set empty is unset: the attach goes ahead
    let unset = [("AGTERM_LIVE_SESSION", "")];
    let mut first = namespace.attach(&args, &unset, &Answers::basic())?;
    first.type_line(&tagged("pong"))?;
    first.expect("got:pong");
    assert_eq!(namespace.session_names(), ["trip"]);

    first.resize(30, 100);
    namespace.screen_until("trip", |text| text.starts_with("100x30\n"));

    // Ctrl+\ detaches and leaves the session running
    first.write(b"\x1c")?;
    assert_eq!(first.wait()?, 0);
    assert_eq!(namespace.session_names(), ["trip"]);

    // a second attach ignores its argv (create-only) and shows the session's screen
    let mut second = namespace.attach(&["trip", "never-run"], &[], &Answers::basic())?;
    second.expect("got:pong");
    second.type_line(&tagged("again"))?;
    second.expect("got:again");
    // the session's program ends: the client exits with its status
    second.type_line("exit")?;
    if cfg!(unix) {
        // the read loop ends at the end of its input
        second.write(b"\x04")?;
    }
    assert_eq!(second.wait()?, 0);
    assert!(namespace.session_names().is_empty());
    Ok(())
}

#[test]
fn the_detach_key_goes_to_the_program_when_turned_off() -> TestResult {
    let namespace = Namespace::new()?;
    let off = [("AGTERM_LIVE_NO_DETACH_KEY", "1")];
    if cfg!(windows) {
        let args = with_shell("keyed");
        let mut client = namespace.attach(&args, &off, &Answers::basic())?;
        client.type_line(&tagged("ready"))?;
        client.expect("got:ready");
        // cmd shows the Ctrl+\ it got as ^\
        client.write(b"\x1c")?;
        namespace.screen_until("keyed", |text| text.contains("^\\"));
        client.write(b"\r")?;
        client.type_line(&tagged("still"))?;
        client.expect("got:still");
        assert!(client.child.try_wait()?.is_none(), "Ctrl+\\ detached");
    } else {
        // the program reads its terminal raw, so Ctrl+\ is a byte and no SIGQUIT
        let script = "stty raw -echo; printf 'READY\\r\\n'; \
                      b=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); printf 'GOT:%s\\r\\n' \"$b\"; \
                      sleep 600";
        let mut client = namespace.attach(&["keyed", "/bin/sh", "-c", script], &off, &Answers::basic())?;
        client.expect("READY");
        client.write(b"\x1c")?;
        client.expect("GOT:1c");
        assert!(client.child.try_wait()?.is_none(), "Ctrl+\\ detached");
    }
    Ok(())
}

/// Escape or an Alt chord and the keys after it, bunched as a slow link hands them over: what only starts like an
/// answer is typed once the input stays quiet, no answer being due, and a control after it is typed at once.
#[cfg(unix)]
#[test]
fn keys_that_start_like_an_answer_reach_the_program() -> TestResult {
    let namespace = Namespace::new()?;
    let script = "stty raw -echo; printf 'READY\\r\\n'; \
                  b=$(dd bs=1 count=10 2>/dev/null | od -An -tx1 | tr -d ' \\n'); printf 'GOT:%s\\r\\n' \"$b\"; \
                  sleep 600";
    let client = namespace.attach(&["bunched", "/bin/sh", "-c", script], &[], &Answers::basic())?;
    client.expect("READY");
    // Alt+_ and G, the start of a Kitty graphics answer, then a pause past the escape time
    client.write(b"\x1b_G")?;
    std::thread::sleep(Duration::from_millis(300));
    // readline's M-_ and Enter, then Alt+] and 1, the start of an OSC answer, ended by Backspace
    client.write(b"\x1b_\r\x1b]1\x7f")?;
    client.expect("GOT:1b5f471b5f0d1b5d317f");
    Ok(())
}

#[test]
fn a_gone_session_runs_the_guard_and_the_attach_fails() -> TestResult {
    let namespace = Namespace::new()?;
    let mut args = vec!["vanished"];
    args.extend(gone_guard());
    let mut client = namespace.attach(&args, &[], &Answers::basic())?;
    if cfg!(unix) {
        // on Windows the pseudoconsole in front of the client repaints what it shows, and may not paint a line the
        // client's reset at its exit clears at once
        client.expect("agterm: remote session is gone");
    }
    assert_eq!(client.wait()?, 1, "the guard's status");
    assert!(namespace.session_names().is_empty());
    Ok(())
}

#[test]
fn an_attach_inside_a_session_is_refused() -> TestResult {
    let namespace = Namespace::new()?;
    let refused = namespace.run(&["attach", "nested"], b"", &[("AGTERM_LIVE_SESSION", "outer")])?;
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused).contains("inside the live session outer"),
        "{}",
        stderr(&refused)
    );
    assert!(namespace.session_names().is_empty());
    Ok(())
}

/// Unix only: a Windows pseudoconsole between the client and the test passes on only the latest title it was given
/// when it paints, so a role title that the session's own title (its snapshot's) follows at once never reaches the
/// test there.
#[cfg(unix)]
#[test]
fn a_managed_attach_writes_its_role_title_on_every_lead_change() -> TestResult {
    let namespace = Namespace::new()?;
    let args = with_shell("led");
    let first = namespace.attach(
        &args,
        &[("AGTERM_LIVE_MANAGED", "na"), ("AGTERM_LIVE_MANAGED_CLAIM", "1")],
        &Answers::basic(),
    )?;
    first.expect_bytes(b"ngterm-live-role;na:leader:");
    let second = namespace.attach(
        &["led"],
        &[("AGTERM_LIVE_MANAGED", "nb"), ("AGTERM_LIVE_MANAGED_CLAIM", "1")],
        &Answers::basic(),
    )?;
    second.expect_bytes(b"ngterm-live-role;nb:leader:");
    first.expect_bytes(b"ngterm-live-role;na:follower:");
    // an unmanaged attach writes none
    let third = namespace.attach(&["led"], &[], &Answers::basic())?;
    third.type_line(&tagged("third"))?;
    first.expect("got:third");
    assert!(!String::from_utf8_lossy(&third.output()).contains("live-role"));
    Ok(())
}

/// The reply owner's tests: the session's program writes queries, prints what comes back as `REPLY<n>:<hex>` lines,
/// and is run by `/bin/sh` with its terminal raw and reads timed (`stty min 0 time 30`, `dd`, `od`).
#[cfg(unix)]
mod owner {
    use super::*;

    /// A script that waits for a typed line, then writes each query (a `printf` format, which may hold a `sleep`
    /// between two halves) and prints the answer's bytes.
    fn probe_script(queries: &[&str]) -> String {
        use std::fmt::Write as _;
        let mut script = String::from(
            "printf 'READY\\r\\n'; IFS= read -r go; stty raw -echo min 0 time 30; \
             reply() { dd bs=4096 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'; }; ",
        );
        for (index, query) in queries.iter().enumerate() {
            let _ = write!(script, "{query}; r=$(reply); printf 'REPLY{index}:%s\\r\\n' \"$r\"; ");
        }
        script.push_str("printf 'DONE\\r\\n'; stty sane; sleep 600");
        script
    }

    /// The ESCs in `bytes`: one per answer (two for one ended by ST).
    fn escapes(bytes: &[u8]) -> usize {
        String::from_utf8_lossy(bytes).matches('\x1b').count()
    }

    fn unhex(hex: &str) -> Vec<u8> {
        (0..hex.len() / 2)
            .filter_map(|at| u8::from_str_radix(hex.get(at * 2..at * 2 + 2)?, 16).ok())
            .collect()
    }

    /// The answer the program printed for query `index`.
    fn reply(client: &Outer, index: usize) -> Vec<u8> {
        let prefix = format!("REPLY{index}:");
        client.expect(&prefix);
        let text = support::wait_for(
            &format!("the whole {prefix} line"),
            || client.text(),
            |text| {
                text.split_once(prefix.as_str())
                    .is_some_and(|(_, rest)| rest.contains('\n'))
            },
        );
        let (_, rest) = text.split_once(prefix.as_str()).unwrap_or_default();
        unhex(rest.lines().next().unwrap_or_default().trim())
    }

    fn attach_probe(namespace: &Namespace, name: &str, queries: &[&str], answers: &Answers) -> TestResult<Outer> {
        let script = probe_script(queries);
        let client = namespace.attach(&[name, "/bin/sh", "-c", &script], &[], answers)?;
        client.expect("READY");
        Ok(client)
    }

    #[test]
    fn split_queries_reach_no_outer_terminal_and_get_one_answer() -> TestResult {
        let namespace = Namespace::new()?;
        let queries = [
            // DA2 cut after its prefix, DECRQM inside its parameters, XTGETTCAP inside its string
            "printf '\\033[>'; sleep 0.3; printf 'c'",
            "printf '\\033[?20'; sleep 0.3; printf '26$p'",
            "printf '\\033P+q54'; sleep 0.3; printf '4e\\033\\\\'",
        ];
        let first = attach_probe(&namespace, "split", &queries, &Answers::basic())?;
        let second = namespace.attach(&["split"], &[], &Answers::basic())?;
        second.expect("READY");
        first.type_line("go")?;
        first.expect("DONE");
        second.expect("DONE");
        let da2 = reply(&first, 0);
        assert!(
            da2.starts_with(b"\x1b[>") && da2.ends_with(b"c"),
            "{:?}",
            String::from_utf8_lossy(&da2)
        );
        assert_eq!(escapes(&da2), 1, "one answer");
        let decrqm = reply(&first, 1);
        assert!(
            decrqm.starts_with(b"\x1b[?2026;") && decrqm.ends_with(b"$y"),
            "{decrqm:?}"
        );
        assert_eq!(escapes(&decrqm), 1, "one answer");
        let xtgettcap = reply(&first, 2);
        assert!(
            xtgettcap.to_ascii_lowercase().starts_with(b"\x1bp1+r544e="),
            "{:?}",
            String::from_utf8_lossy(&xtgettcap)
        );
        assert_eq!(escapes(&xtgettcap), 2, "one answer, ended by ST");
        for client in [&first, &second] {
            let output = String::from_utf8_lossy(&client.output()).into_owned();
            for query in ["\x1b[>c", "26$p", "\x1bP+q", "+q544e"] {
                assert!(!output.contains(query), "{query:?} reached an outer terminal");
            }
        }
        Ok(())
    }

    #[test]
    fn the_outer_terminals_colours_answer_osc_10_and_11() -> TestResult {
        let namespace = Namespace::new()?;
        let answers = Answers {
            foreground: Some("rgb:1111/2222/3333"),
            background: Some("rgb:4444/5555/6666"),
            ..Answers::default()
        };
        let queries = ["printf '\\033]10;?\\033\\\\'", "printf '\\033]11;?\\007'"];
        let client = attach_probe(&namespace, "themed", &queries, &answers)?;
        client.type_line("go")?;
        client.expect("DONE");
        let foreground = String::from_utf8_lossy(&reply(&client, 0)).into_owned();
        assert!(foreground.contains("]10;rgb:1111/2222/3333"), "{foreground:?}");
        let background = String::from_utf8_lossy(&reply(&client, 1)).into_owned();
        assert!(background.contains("]11;rgb:4444/5555/6666"), "{background:?}");
        Ok(())
    }

    #[test]
    fn an_osc_52_read_is_relayed_to_the_outer_terminal_and_back() -> TestResult {
        use base64::Engine as _;
        let namespace = Namespace::new()?;
        let answers = Answers {
            clipboard: Some("clip-text"),
            ..Answers::default()
        };
        let client = attach_probe(&namespace, "clip", &["printf '\\033]52;c;?\\007'"], &answers)?;
        client.type_line("go")?;
        client.expect("DONE");
        let answer = String::from_utf8_lossy(&reply(&client, 0)).into_owned();
        let data = base64::engine::general_purpose::STANDARD.encode("clip-text");
        assert!(answer.contains(&format!("]52;c;{data}")), "{answer:?}");
        // the read reached the outer terminal from the client, not from the program
        assert!(String::from_utf8_lossy(&client.output()).contains("\x1b]52;c;?\x1b\\"));
        Ok(())
    }

    /// An outer terminal that asks its user first answers the read late, and over a slow link the answer can come in
    /// two reads with a pause between them: the client waits for it, holds the cut answer, and types none of it.
    #[test]
    fn a_late_osc_52_answer_cut_by_a_pause_is_relayed_whole() -> TestResult {
        use base64::Engine as _;
        let namespace = Namespace::new()?;
        let client = attach_probe(&namespace, "late", &["printf '\\033]52;c;?\\007'"], &Answers::basic())?;
        client.type_line("go")?;
        client.expect_bytes(b"\x1b]52;c;?\x1b\\");
        let data = base64::engine::general_purpose::STANDARD.encode("clip-text");
        let (head, tail) = data.split_at(4);
        // the user's consent takes a while; the pause inside the answer is past the input's escape time
        std::thread::sleep(Duration::from_millis(400));
        client.write(format!("\x1b]52;c;{head}").as_bytes())?;
        std::thread::sleep(Duration::from_millis(300));
        client.write(format!("{tail}\x1b\\").as_bytes())?;
        client.expect("DONE");
        let answer = reply(&client, 0);
        let text = String::from_utf8_lossy(&answer).into_owned();
        assert!(text.contains(&format!("]52;c;{data}")), "{text:?}");
        assert_eq!(
            escapes(&answer),
            1 + usize::from(text.ends_with('\\')),
            "one answer, nothing typed: {text:?}"
        );
        Ok(())
    }
}

/// A scripted server for the reattach tests: the first attach is sent a snapshot and output, then its connection ends
/// without the session's exit; a later one is answered as `again` says. Every attach request is recorded.
fn dropping_peer(
    again: fn(&mut agterm_session::endpoint::Stream),
) -> TestResult<(Peer, Arc<Mutex<Vec<AttachRequest>>>)> {
    use agterm_session::protocol::{AttachClientMessage, PROTOCOL_VERSION, read_frame};
    let requests: Arc<Mutex<Vec<AttachRequest>>> = Arc::default();
    let recorded = Arc::clone(&requests);
    let peer = Peer::start(move |mut stream, role| {
        if role != Role::Attach {
            return;
        }
        let Ok(AttachClientMessage::Attach(request)) =
            read_frame(&mut stream).and_then(|frame| frame.message::<AttachClientMessage>(PROTOCOL_VERSION))
        else {
            return;
        };
        let first = {
            let mut requests = recorded.lock().unwrap_or_else(PoisonError::into_inner);
            requests.push(request);
            requests.len() == 1
        };
        if first {
            send_snapshot(&mut stream, b"first-snap", b"first-output");
            // the connection ends here, the session's exit unsent
        } else {
            again(&mut stream);
        }
    })?;
    Ok((peer, requests))
}

/// A snapshot holding `snapshot`, then `output`.
fn send_snapshot(stream: &mut agterm_session::endpoint::Stream, snapshot: &[u8], output: &[u8]) {
    use agterm_session::protocol::{AttachServerMessage, write_message};
    let size = GridSize { cols: COLS, rows: ROWS };
    let offset = 100;
    for message in [
        AttachServerMessage::ReplyOwner {
            owner: true,
            generation: 1,
        },
        AttachServerMessage::SnapshotBegin { epoch: 1, offset, size },
        AttachServerMessage::SnapshotChunk {
            epoch: 1,
            data: snapshot.to_vec(),
        },
        AttachServerMessage::SnapshotEnd { epoch: 1 },
        AttachServerMessage::Output {
            offset,
            data: output.to_vec(),
        },
    ] {
        if write_message(stream, &message).is_err() {
            return;
        }
    }
}

#[test]
fn a_connection_that_ends_without_the_exit_is_attached_again_from_a_reset() -> TestResult {
    let (peer, requests) = dropping_peer(|stream| {
        use agterm_session::protocol::{AttachServerMessage, ExitStatus, write_message};
        send_snapshot(stream, b"second-snap", b"second-output");
        std::thread::sleep(Duration::from_millis(300));
        let _ = write_message(
            stream,
            &AttachServerMessage::Exit {
                status: ExitStatus::Exited { code: 3, signal: None },
            },
        );
        // the exit is read before the connection's end
        std::thread::sleep(Duration::from_secs(2));
    })?;
    let namespace = Namespace::of_peer(&peer)?;
    let mut client = namespace.attach(
        &["dropped", "never-run"],
        &[("AGTERM_LIVE_MANAGED", "nd"), ("AGTERM_LIVE_MANAGED_CLAIM", "1")],
        &Answers::basic(),
    )?;
    assert_eq!(client.wait()?, 3, "{:?}", client.text());
    client.expect("second-snap");
    if cfg!(unix) {
        // the bytes as written (a pseudoconsole repaints them on Windows): the first output, the reset with the
        // colour scheme reports turned on again after its RIS, the second snapshot
        let output = String::from_utf8_lossy(&client.output()).into_owned();
        let second = output.find("second-snap").ok_or("no second snapshot")?;
        let reset = String::from_utf8_lossy(agterm_session::server::snapshot::RESYNC_RESET).replacen(
            "\x1bc",
            "\x1bc\x1b[?2031h",
            1,
        );
        let reset_at = output.find(&reset).ok_or("no reset")?;
        assert!(
            output.find("first-output").is_some_and(|first| first < reset_at) && reset_at < second,
            "{output:?}"
        );
    }
    let requests = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].create.is_some());
    assert_eq!(requests[1].name, "dropped");
    assert!(requests[1].create.is_none(), "a reattach creates nothing");
    assert_eq!(
        requests[1].managed.as_ref().map(|managed| managed.nonce.as_str()),
        Some("nd")
    );
    Ok(())
}

#[test]
fn a_reattach_to_a_gone_session_fails() -> TestResult {
    let (peer, _) = dropping_peer(|stream| {
        use agterm_session::protocol::{AttachRefusal, AttachServerMessage, write_message};
        let _ = write_message(
            stream,
            &AttachServerMessage::Refused {
                reason: AttachRefusal::Gone,
                message: "no such session".to_owned(),
            },
        );
    })?;
    let namespace = Namespace::of_peer(&peer)?;
    let mut client = namespace.attach(&["vanishing"], &[], &Answers::basic())?;
    assert_eq!(client.wait()?, 1);
    if cfg!(unix) {
        // printed as the client exits, which a pseudoconsole may not paint before it closes (Windows)
        client.expect("the live session vanishing is gone");
    }
    Ok(())
}

/// A path that names no directory yet is fine for every command: the server makes it.
#[test]
fn an_attach_starts_the_namespaces_server() -> TestResult {
    let namespace = Namespace::new()?;
    assert!(!Path::new(&namespace.directory).exists());
    let args = with_shell("started");
    let mut client = namespace.attach(&args, &[], &Answers::basic())?;
    client.type_line(&tagged("up"))?;
    client.expect("got:up");
    client.write(b"\x1c")?;
    assert_eq!(client.wait()?, 0);
    assert_eq!(namespace.session_names(), ["started"]);
    Ok(())
}
