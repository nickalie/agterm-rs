//! The built `agtermctl` against an in-process control server over the fake main loop: socket resolution, `version`,
//! exit statuses and what lands on stdout and stderr.

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use agterm_core::{Core, CoreConfig};
use agterm_host::MainLoop;
use agterm_host::fakes::FakeMainLoop;
use agterm_proto::{AppIdentity, Request, Response};
use agterm_server::{ControlServer, Reply};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const AGTERMCTL: &str = env!("CARGO_BIN_EXE_agtermctl");
const NOT_RUNNING: &str = "agterm may be stopped or unable to accept connections";

/// A short temp dir: socket paths must stay under 104 bytes whatever `TMPDIR` is.
fn short_dir() -> io::Result<TempDir> {
    tempfile::Builder::new().prefix("agc").tempdir_in("/tmp")
}

/// A server on `<dir>/agterm.sock` answering through `handler` on a fake main loop. Fields drop top to bottom: the
/// server stops before its main loop and the directory holding its socket go.
struct Server<S: Send + 'static> {
    _server: ControlServer,
    _main_loop: Arc<FakeMainLoop<S>>,
}

fn serve<S, H>(socket: &Path, state: S, handler: H) -> TestResult<Server<S>>
where
    S: Send + 'static,
    H: Fn(&mut S, Request) -> Reply + Send + Sync + 'static,
{
    let main_loop = Arc::new(FakeMainLoop::spawn(state)?);
    let mut server = ControlServer::new(socket);
    server.start(Arc::clone(&main_loop) as Arc<dyn MainLoop<S>>, handler)?;
    Ok(Server {
        _server: server,
        _main_loop: main_loop,
    })
}

fn serve_core(socket: &Path) -> TestResult<Server<Core>> {
    let core = Core::new(CoreConfig {
        app: AppIdentity::from_recorded("0.1.0", Some("abc1234")),
        capabilities: vec!["posix-shell".to_owned()],
    });
    serve(socket, core, |core: &mut Core, request: Request| {
        core.dispatch(&request).into()
    })
}

/// The isolated environment every run gets: no `AGTERM_*` from the caller, a home and data dir under `root`.
fn isolated(root: &Path) -> Vec<(String, PathBuf)> {
    vec![
        ("HOME".to_owned(), root.join("home")),
        ("XDG_DATA_HOME".to_owned(), root.join("home/.local/share")),
    ]
}

/// Runs `agtermctl args` with `env` on top of the isolated environment of `root`.
fn run<I, S>(root: &Path, env: &[(&str, &Path)], args: I) -> TestResult<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(AGTERMCTL);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("AGTERM_") {
            command.env_remove(name);
        }
    }
    command.envs(isolated(root)).args(args);
    for (name, value) in env {
        command.env(name, value);
    }
    Ok(command.output()?)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Exit status, stdout and stderr of a run.
fn outcome(output: &Output) -> (Option<i32>, String, String) {
    (output.status.code(), text(&output.stdout), text(&output.stderr))
}

fn connect_failure(socket: &Path, reason: &str, hint: &str) -> String {
    format!("Error: connect({}) failed: {reason} — {hint}\n", socket.display())
}

/// The server's own `version` response line, newline included, read straight off the socket: what `--json` must
/// print byte for byte.
fn version_line(socket: &Path) -> TestResult<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(b"{\"cmd\":\"version\"}\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(line)
}

#[test]
fn version_prints_the_app_then_the_client() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let _server = serve_core(&socket)?;
    let output = run(
        dir.path(),
        &[],
        [OsStr::new("version"), OsStr::new("--socket"), socket.as_os_str()],
    )?;
    let client = fs::canonicalize(AGTERMCTL)?;
    let expected = format!("0.1.0 (abc1234)\nclient: {}\n", client.display());
    assert_eq!(outcome(&output), (Some(0), expected, String::new()));
    Ok(())
}

#[test]
fn version_json_prints_the_server_line_unchanged() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let _server = serve_core(&socket)?;
    let output = run(
        dir.path(),
        &[],
        [
            OsStr::new("version"),
            OsStr::new("--json"),
            OsStr::new("--socket"),
            socket.as_os_str(),
        ],
    )?;
    assert_eq!(outcome(&output), (Some(0), version_line(&socket)?, String::new()));
    Ok(())
}

#[test]
fn the_state_dir_names_the_socket() -> TestResult {
    let dir = short_dir()?;
    let state = dir.path().join("state");
    let _server = serve_core(&state.join("agterm.sock"))?;
    let output = run(dir.path(), &[("AGTERM_STATE_DIR", &state)], ["version", "--json"])?;
    assert_eq!(
        outcome(&output),
        (Some(0), version_line(&state.join("agterm.sock"))?, String::new())
    );
    Ok(())
}

#[test]
fn the_socket_option_wins_over_the_state_dir() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let other = dir.path().join("other");
    let _server = serve_core(&socket)?;
    let env = [("AGTERM_STATE_DIR", other.as_path())];
    let output = run(
        dir.path(),
        &env,
        [
            OsStr::new("version"),
            OsStr::new("--json"),
            OsStr::new("--socket"),
            socket.as_os_str(),
        ],
    )?;
    assert_eq!(outcome(&output), (Some(0), version_line(&socket)?, String::new()));
    let unreachable = other.join("agterm.sock");
    let failed = connect_failure(&unreachable, "No such file or directory", NOT_RUNNING);
    assert_eq!(
        outcome(&run(dir.path(), &env, ["version"])?),
        (Some(1), String::new(), failed)
    );
    Ok(())
}

#[test]
fn the_app_socket_variables_are_not_read() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let other = dir.path().join("other");
    let _server = serve_core(&socket)?;
    let env = [
        ("AGTERM_STATE_DIR", other.as_path()),
        ("AGTERM_SOCKET", socket.as_path()),
        ("AGTERM_CONTROL_SOCKET", socket.as_path()),
    ];
    let failed = connect_failure(&other.join("agterm.sock"), "No such file or directory", NOT_RUNNING);
    assert_eq!(
        outcome(&run(dir.path(), &env, ["version"])?),
        (Some(1), String::new(), failed)
    );
    Ok(())
}

#[test]
fn without_a_state_dir_the_xdg_data_home_is_the_default() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("home/.local/share/agterm/agterm.sock");
    let _server = serve_core(&socket)?;
    let output = run(dir.path(), &[], ["version", "--json"])?;
    assert_eq!(outcome(&output), (Some(0), version_line(&socket)?, String::new()));
    Ok(())
}

#[test]
fn a_refused_connection_exits_1_with_the_error() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    // a socket file nobody listens on: a stale socket of a dead instance
    drop(UnixListener::bind(&socket)?);
    let refused = connect_failure(&socket, "Connection refused", NOT_RUNNING);
    let output = run(
        dir.path(),
        &[],
        [OsStr::new("version"), OsStr::new("--socket"), socket.as_os_str()],
    )?;
    assert_eq!(outcome(&output), (Some(1), String::new(), refused));
    Ok(())
}

#[test]
fn a_present_owner_that_does_not_accept_is_named() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    // takes the ownership lock without binding, as an instance whose bind failed
    let owner = ControlServer::new(&socket);
    assert!(!owner.is_refused());
    let hint = "the socket owner is present but not accepting connections";
    let failed = connect_failure(&socket, "No such file or directory", hint);
    let output = run(
        dir.path(),
        &[],
        [OsStr::new("version"), OsStr::new("--socket"), socket.as_os_str()],
    )?;
    assert_eq!(outcome(&output), (Some(1), String::new(), failed));
    Ok(())
}

#[test]
fn an_over_long_socket_path_is_refused_locally() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("s".repeat(120));
    let len = socket.as_os_str().len();
    let output = run(
        dir.path(),
        &[],
        [OsStr::new("version"), OsStr::new("--socket"), socket.as_os_str()],
    )?;
    let failed = format!("Error: socket path too long ({len} bytes): {}\n", socket.display());
    assert_eq!(outcome(&output), (Some(1), String::new(), failed));
    Ok(())
}

#[test]
fn a_server_refusal_exits_1() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let _server = serve(&socket, (), |(), _| Response::failure("version is broken").into())?;
    let base = [OsStr::new("version"), OsStr::new("--socket"), socket.as_os_str()];
    let human = run(dir.path(), &[], base)?;
    assert_eq!(
        outcome(&human),
        (Some(1), String::new(), "error: version is broken\n".to_owned())
    );
    let json = run(dir.path(), &[], base.into_iter().chain([OsStr::new("--json")]))?;
    let line = "{\"ok\":false,\"error\":\"version is broken\"}\n".to_owned();
    assert_eq!(outcome(&json), (Some(1), line, String::new()));
    Ok(())
}

#[test]
fn usage_errors_exit_64_before_connecting() -> TestResult {
    let dir = short_dir()?;
    for args in [
        &["version", "--bogus"][..],
        &["version", "extra"],
        &["version", "--socket"],
        &["--socket", "/tmp/x.sock", "version"],
        &["--json", "version"],
        &["bogus"],
    ] {
        let output = run(dir.path(), &[], args)?;
        let (code, stdout, stderr) = outcome(&output);
        assert_eq!((code, stdout.as_str()), (Some(64), ""), "{args:?}: {stderr}");
        assert!(stderr.starts_with("Error: "), "{args:?}: {stderr}");
        assert!(stderr.contains("\nUsage: agtermctl "), "{args:?}: {stderr}");
    }
    Ok(())
}

/// Runs `agtermctl args` with stdout on a pipe whose reader is already gone, as `agtermctl tree | head -1` once
/// `head` exits.
fn run_into_closed_pipe(root: &Path, args: &[&OsStr]) -> TestResult<Output> {
    let (reader, writer) = io::pipe()?;
    drop(reader);
    let mut command = Command::new(AGTERMCTL);
    command.envs(isolated(root)).args(args).stdout(writer);
    Ok(command.output()?)
}

#[test]
fn a_closed_stdout_ends_the_run_by_sigpipe_not_a_panic() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let _server = serve_core(&socket)?;
    let version = [OsStr::new("version"), OsStr::new("--socket"), socket.as_os_str()];
    let json = [&version[..], &[OsStr::new("--json")]].concat();
    for args in [&[][..], &[OsStr::new("--help")], &version, &json] {
        let output = run_into_closed_pipe(dir.path(), args)?;
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGPIPE),
            "{args:?}: {}",
            output.status
        );
        assert_eq!(text(&output.stderr), "", "{args:?}");
    }
    Ok(())
}

#[test]
fn help_goes_to_stdout() -> TestResult {
    let dir = short_dir()?;
    for args in [&[][..], &["--help"], &["version", "--help"], &["help", "version"]] {
        let output = run(dir.path(), &[], args)?;
        let (code, stdout, stderr) = outcome(&output);
        assert_eq!((code, stderr.as_str()), (Some(0), ""), "{args:?}");
        assert!(stdout.contains("Usage: agtermctl"), "{args:?}: {stdout}");
    }
    Ok(())
}
