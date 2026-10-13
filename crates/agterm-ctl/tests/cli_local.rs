//! The two local-only subcommands, `clipboard set` and `terminfo install`, run as the built `agtermctl` next to an
//! in-process control server they must never talk to. Everything they touch is sandboxed: the CLI runs from a stage
//! under cargo's test temp dir with a fake `zmx` beside it, `ssh` is a fake first on `PATH` whose remote side is a
//! temp `HOME`, and no `AGTERM_*` or `ZMX_*` variable of the caller reaches a run, so neither a real pane nor the
//! desktop clipboard nor the account's own terminfo is ever written.

use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agterm_host::MainLoop;
use agterm_host::fakes::FakeMainLoop;
use agterm_proto::Response;
use agterm_server::ControlServer;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const AGTERMCTL: &str = env!("CARGO_BIN_EXE_agtermctl");
const ENTRY: &str = "xterm-ghostty";
const NOT_LIVE: &str = "this pane has no zmx daemon; run it in a main or split pane started under Live sessions";
const DAEMON: &str = "agterm-357e181888f34450b5c225bd00239b9b";

/// A server on `<state>/agterm.sock`, the socket the CLI would resolve from `AGTERM_STATE_DIR`, counting the requests
/// it is sent. Fields drop top to bottom: the server stops before its main loop.
struct Watched {
    _server: ControlServer,
    _main_loop: Arc<FakeMainLoop<()>>,
    requests: Arc<AtomicUsize>,
}

impl Watched {
    fn start(state: &Path) -> TestResult<Self> {
        let main_loop = Arc::new(FakeMainLoop::spawn(())?);
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&requests);
        let mut server = ControlServer::new(state.join("agterm.sock"));
        server.start(Arc::clone(&main_loop) as Arc<dyn MainLoop<()>>, move |(), _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Response::failure("a local command reached the socket").into()
        })?;
        Ok(Self {
            _server: server,
            _main_loop: main_loop,
            requests,
        })
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// One sandbox: a short state dir in `/tmp` for the socket, and a stage (`bin/agtermctl`, optionally `bin/zmx` and
/// `share/terminfo`) plus a remote `HOME` under cargo's test temp dir, on the build's own filesystem so the CLI can be
/// hard-linked rather than copied.
struct Sandbox {
    state: TempDir,
    root: TempDir,
}

impl Sandbox {
    fn new() -> TestResult<Self> {
        let state = tempfile::Builder::new().prefix("agl").tempdir_in("/tmp")?;
        let root = tempfile::Builder::new()
            .prefix("cli-local")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
        let bin = root.path().join("stage/bin");
        fs::create_dir_all(&bin)?;
        let client = bin.join("agtermctl");
        if fs::hard_link(AGTERMCTL, &client).is_err() {
            fs::copy(AGTERMCTL, &client)?;
        }
        fs::create_dir_all(root.path().join("home"))?;
        Ok(Self { state, root })
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    fn script(path: &Path, body: &str) -> io::Result<()> {
        fs::create_dir_all(path.parent().unwrap_or(Path::new("/")))?;
        fs::write(path, format!("#!/bin/sh\n{body}\n"))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
    }

    /// A `zmx` beside the CLI that records its arguments, its `ZMX_*` environment and its stdin, then exits `code`.
    fn fake_zmx(&self, code: i32) -> io::Result<()> {
        Self::script(
            &self.path("stage/bin/zmx"),
            &format!(
                "printf '%s\\n' \"$@\" > '{args}'\nenv | grep '^ZMX_' | sort > '{env}'\ncat > '{stdin}'\nexit {code}",
                args = self.path("zmx.args").display(),
                env = self.path("zmx.env").display(),
                stdin = self.path("zmx.stdin").display(),
            ),
        )
    }

    /// The stage's `share/terminfo`, compiled by the real `tic` from a small entry of the right name.
    fn stage_terminfo(&self) -> TestResult {
        let database = self.path("stage/share/terminfo");
        fs::create_dir_all(&database)?;
        let mut tic = Command::new("/usr/bin/tic")
            .args(["-x", "-o"])
            .arg(&database)
            .arg("-")
            .stdin(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        tic.stdin
            .take()
            .ok_or("no stdin")?
            .write_all(format!("{ENTRY}|agterm test entry,\n\tcols#80, lines#24,\n").as_bytes())?;
        if !tic.wait()?.success() {
            return Err("tic could not compile the fixture entry".into());
        }
        Ok(())
    }

    /// A fake `ssh` in `fake-ssh/`: records its argv, then acts on `FAKE_SSH` (`exit<N>`, `signal`) or runs the remote
    /// command line the way sshd hands it to the account's shell, with `HOME` the sandbox's and nothing else inherited.
    fn fake_ssh(&self) -> io::Result<()> {
        Self::script(
            &self.path("fake-ssh/ssh"),
            &format!(
                "for arg; do printf '%s\\n' \"$arg\"; done > '{record}'\ncase \"$FAKE_SSH\" in exit*) exit \
                 \"${{FAKE_SSH#exit}}\" ;; signal) kill -TERM $$ ;; esac\nfor last; do :; done\nexec /usr/bin/env -i \
                 HOME='{home}' PATH=/usr/bin:/bin /bin/sh -c \"$last\"",
                record = self.path("ssh.args").display(),
                home = self.path("home").display(),
            ),
        )
    }

    /// Runs the staged CLI (through `/bin/sh -c 'PREFIX exec agtermctl "$@"'` when `prefix` is given) with stdin
    /// `input`, the caller's `AGTERM_*` and `ZMX_*` removed, `AGTERM_STATE_DIR` naming the watched server, the fake
    /// ssh first on `PATH`, and `env` on top.
    fn run(&self, prefix: Option<&str>, args: &[&str], env: &[(&str, &str)], input: &[u8]) -> TestResult<Output> {
        let client = self.path("stage/bin/agtermctl");
        let mut command = match prefix {
            Some(prefix) => {
                let mut shell = Command::new("/bin/sh");
                shell.arg("-c").arg(format!("{prefix} exec \"$0\" \"$@\"")).arg(&client);
                shell
            }
            None => Command::new(&client),
        };
        for (name, _) in std::env::vars_os() {
            let name_text = name.to_string_lossy();
            if name_text.starts_with("AGTERM_") || name_text.starts_with("ZMX_") || name_text == "TERMINFO" {
                command.env_remove(&name);
            }
        }
        let path = format!(
            "{}:{}",
            self.path("fake-ssh").display(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned())
        );
        command
            .args(args)
            .env("AGTERM_STATE_DIR", self.state.path())
            .env("HOME", self.path("home"))
            .env("PATH", path)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            // a CLI that refuses before reading closes its stdin; the refusal is what the test checks
            let _ = stdin.write_all(input);
        }
        Ok(child.wait_with_output()?)
    }
}

fn outcome(output: &Output) -> (Option<i32>, String, String) {
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn failed(message: &str) -> (Option<i32>, String, String) {
    (Some(1), String::new(), format!("Error: {message}\n"))
}

// -- clipboard set

#[test]
fn clipboard_set_refuses_outside_a_live_pane_without_connecting() -> TestResult {
    let sandbox = Sandbox::new()?;
    let server = Watched::start(sandbox.state.path())?;
    sandbox.fake_zmx(0)?;
    assert_eq!(
        outcome(&sandbox.run(None, &["clipboard", "set", "hello"], &[], b"")?),
        failed(NOT_LIVE)
    );
    assert_eq!(
        outcome(&sandbox.run(None, &["clipboard", "set"], &[], b"hello")?),
        failed(NOT_LIVE)
    );
    let foreign = [
        ("ZMX_SESSION", "someone-elses-session"),
        ("ZMX_DIR", "/tmp/agterm-zmx-1"),
    ];
    assert_eq!(
        outcome(&sandbox.run(None, &["clipboard", "set", "hello"], &foreign, b"")?),
        failed(NOT_LIVE)
    );
    assert!(!sandbox.path("zmx.args").exists());
    assert_eq!(server.requests(), 0);
    Ok(())
}

#[test]
fn clipboard_set_checks_the_text_before_the_pane() -> TestResult {
    let sandbox = Sandbox::new()?;
    let server = Watched::start(sandbox.state.path())?;
    assert_eq!(
        outcome(&sandbox.run(None, &["clipboard", "set"], &[], b"")?),
        failed("nothing to copy")
    );
    assert_eq!(
        outcome(&sandbox.run(None, &["clipboard", "set", ""], &[], b"")?),
        failed("nothing to copy")
    );
    let large = vec![b'x'; 6_300_000];
    assert_eq!(
        outcome(&sandbox.run(None, &["clipboard", "set"], &[], &large)?),
        failed("text is too large to copy through the terminal: 8400000 bytes encoded, the limit is 8388544")
    );
    let socket = sandbox.state.path().join("agterm.sock");
    let refused = sandbox.run(
        None,
        &["clipboard", "set", "--socket", &socket.display().to_string(), "hello"],
        &[],
        b"",
    )?;
    assert_eq!(
        outcome(&refused),
        (
            Some(64),
            String::new(),
            "Error: Unknown option '--socket'\nUsage: agtermctl clipboard set [<text>]\n  See 'agtermctl clipboard set \
             --help' for more information.\n"
                .to_owned()
        )
    );
    assert_eq!(server.requests(), 0);
    Ok(())
}

#[test]
fn clipboard_set_prints_the_write_into_the_panes_daemon() -> TestResult {
    let sandbox = Sandbox::new()?;
    let server = Watched::start(sandbox.state.path())?;
    sandbox.fake_zmx(0)?;
    let live = [
        ("ZMX_SESSION", DAEMON),
        ("ZMX_DIR", "/tmp/agterm-zmx-1"),
        ("ZMX_SESSION_PREFIX", "work-"),
    ];
    let piped = sandbox.run(None, &["clipboard", "set"], &live, "héllo\n".as_bytes())?;
    assert_eq!(outcome(&piped), (Some(0), String::new(), String::new()));
    assert_eq!(
        fs::read_to_string(sandbox.path("zmx.args"))?,
        format!("print\n{DAEMON}\n")
    );
    assert_eq!(fs::read(sandbox.path("zmx.stdin"))?, b"\x1b]52;c;aMOpbGxvCg==\x07");
    assert_eq!(
        fs::read_to_string(sandbox.path("zmx.env"))?,
        "ZMX_DIR=/tmp/agterm-zmx-1\nZMX_SESSION=\nZMX_SESSION_PREFIX=\n"
    );
    let dashed = sandbox.run(None, &["clipboard", "set", "--", "-1"], &live, b"")?;
    assert_eq!(outcome(&dashed), (Some(0), String::new(), String::new()));
    assert_eq!(fs::read(sandbox.path("zmx.stdin"))?, b"\x1b]52;c;LTE=\x07");
    assert_eq!(server.requests(), 0);
    Ok(())
}

#[test]
fn clipboard_set_reports_zmx_failing_or_missing() -> TestResult {
    let sandbox = Sandbox::new()?;
    let live = [("ZMX_SESSION", DAEMON), ("ZMX_DIR", "/tmp/agterm-zmx-1")];
    let zmx = fs::canonicalize(sandbox.path("stage/bin"))?.join("zmx");
    assert_eq!(
        outcome(&sandbox.run(None, &["clipboard", "set", "hi"], &live, b"")?),
        failed(&format!("no zmx next to this agtermctl; looked for {}", zmx.display()))
    );
    Sandbox::script(&zmx, "cat >/dev/null\necho 'session not found' >&2\nexit 3")?;
    assert_eq!(
        outcome(&sandbox.run(None, &["clipboard", "set", "hi"], &live, b"")?),
        failed("zmx print exited 3: session not found")
    );
    Ok(())
}

#[test]
fn clipboard_set_reports_a_zmx_that_exits_without_reading_a_large_text() -> TestResult {
    // the built CLI runs SIGPIPE at its default: a write past the pipe buffer into a zmx that has gone must get EPIPE
    // and report zmx's own status, not end the CLI by the signal (141 under a shell)
    let sandbox = Sandbox::new()?;
    let live = [("ZMX_SESSION", DAEMON), ("ZMX_DIR", "/tmp/agterm-zmx-1")];
    let zmx = sandbox.path("stage/bin/zmx");
    Sandbox::script(&zmx, "echo 'session not found' >&2\nexit 4")?;
    let large = vec![b'x'; 1024 * 1024];
    let output = sandbox.run(None, &["clipboard", "set"], &live, &large)?;
    assert_eq!(outcome(&output), failed("zmx print exited 4: session not found"));
    Ok(())
}

// -- terminfo install

#[test]
fn terminfo_install_compiles_the_entry_into_the_remote_home_without_connecting() -> TestResult {
    let sandbox = Sandbox::new()?;
    let server = Watched::start(sandbox.state.path())?;
    sandbox.stage_terminfo()?;
    sandbox.fake_ssh()?;
    let install = [
        "terminfo", "install", "-p", "2222", "-i", "id_one", "-i", "id_two", "-J", "jump", "-F", "cfg", "box",
    ];
    let output = sandbox.run(None, &install, &[], b"")?;
    let (code, stdout, stderr) = outcome(&output);
    assert_eq!(
        (code, stdout.as_str()),
        (Some(0), "installed xterm-ghostty on box\n"),
        "{stderr}"
    );
    let installed = sandbox.path("home/.terminfo/x").join(ENTRY);
    assert!(installed.metadata()?.len() > 0);

    let recorded = fs::read_to_string(sandbox.path("ssh.args"))?;
    let argv: Vec<&str> = recorded.lines().collect();
    assert_eq!(argv[argv.len() - 2], "box");
    assert!(argv[argv.len() - 1].starts_with("'/bin/sh' '-c' '"));
    assert_eq!(
        argv[..argv.len() - 2],
        [
            "-T",
            "-o",
            "StdinNull=no",
            "-o",
            "SessionType=default",
            "-o",
            "ForkAfterAuthentication=no",
            "-o",
            "RemoteCommand=none",
            "-p",
            "2222",
            "-i",
            "id_one",
            "-i",
            "id_two",
            "-J",
            "jump",
            "-F",
            "cfg"
        ]
    );
    assert_eq!(server.requests(), 0);
    Ok(())
}

#[test]
fn terminfo_install_reports_how_ssh_ended() -> TestResult {
    let sandbox = Sandbox::new()?;
    sandbox.stage_terminfo()?;
    sandbox.fake_ssh()?;
    let install = ["terminfo", "install", "box"];
    assert_eq!(
        outcome(&sandbox.run(None, &install, &[("FAKE_SSH", "exit255")], b"")?),
        (
            Some(255),
            String::new(),
            "ssh exited 255; xterm-ghostty was not installed\n".to_owned()
        )
    );
    let killed = (
        Some(128 + 15),
        String::new(),
        "ssh was killed by signal 15; xterm-ghostty was not installed\n".to_owned(),
    );
    assert_eq!(
        outcome(&sandbox.run(None, &install, &[("FAKE_SSH", "signal")], b"")?),
        killed
    );
    // a caller ignoring SIGTERM does not leave ssh deaf to it
    assert_eq!(
        outcome(&sandbox.run(Some("trap '' TERM;"), &install, &[("FAKE_SSH", "signal")], b"")?),
        killed
    );
    assert!(!sandbox.path("home/.terminfo").exists());
    Ok(())
}

#[test]
fn terminfo_install_refuses_a_destination_ssh_would_misread() -> TestResult {
    let sandbox = Sandbox::new()?;
    sandbox.stage_terminfo()?;
    sandbox.fake_ssh()?;
    for destination in ["-oProxyCommand=x", "two words", ""] {
        let output = sandbox.run(None, &["terminfo", "install", "--", destination], &[], b"")?;
        let expected = format!(
            "Error: destination must be a host, user@host or an ssh alias, not '{destination}'\nUsage: agtermctl \
             terminfo install <destination> [-p <port>] [-i <file> ...] [-J <host>] [-F <file>]\n  See 'agtermctl \
             terminfo install --help' for more information.\n"
        );
        assert_eq!(outcome(&output), (Some(64), String::new(), expected), "{destination:?}");
    }
    assert!(!sandbox.path("ssh.args").exists());
    Ok(())
}

#[test]
fn terminfo_install_without_an_entry_says_where_it_looked() -> TestResult {
    let sandbox = Sandbox::new()?;
    sandbox.fake_ssh()?;
    let database = fs::canonicalize(sandbox.path("stage"))?.join("share/terminfo");
    let empty = sandbox.path("empty");
    fs::create_dir_all(&empty)?;
    let empty_text = empty.display().to_string();
    let output = sandbox.run(None, &["terminfo", "install", "box"], &[("TERMINFO", &empty_text)], b"")?;
    let message = format!(
        "no xterm-ghostty terminfo entry next to this agtermctl; looked in {}, {empty_text}",
        database.display()
    );
    assert_eq!(outcome(&output), failed(&message));
    assert!(!sandbox.path("ssh.args").exists());
    Ok(())
}
