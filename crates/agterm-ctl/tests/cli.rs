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

use agterm_core::model::WindowLibrary;
use agterm_core::{Core, CoreConfig, CoreState};
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
struct Server<S: 'static> {
    _server: ControlServer,
    _main_loop: Arc<FakeMainLoop<S>>,
}

fn serve<S, H>(socket: &Path, state: S, handler: H) -> TestResult<Server<S>>
where
    S: Send + 'static,
    H: Fn(&mut S, Request) -> Reply + Send + Sync + 'static,
{
    serve_on(socket, FakeMainLoop::spawn(state)?, handler)
}

fn serve_on<S, H>(socket: &Path, main_loop: FakeMainLoop<S>, handler: H) -> TestResult<Server<S>>
where
    S: 'static,
    H: Fn(&mut S, Request) -> Reply + Send + Sync + 'static,
{
    let main_loop = Arc::new(main_loop);
    let mut server = ControlServer::new(socket);
    server.start(Arc::clone(&main_loop) as Arc<dyn MainLoop<S>>, handler)?;
    Ok(Server {
        _server: server,
        _main_loop: main_loop,
    })
}

/// A core over `state`, built on its main loop's thread, which it never leaves.
fn serve_core_with(socket: &Path, state: impl FnOnce() -> CoreState + Send + 'static) -> TestResult<Server<Core>> {
    let main_loop = FakeMainLoop::spawn_with(move || {
        Core::new(
            CoreConfig {
                app: AppIdentity::from_recorded("0.1.0", Some("abc1234")),
                capabilities: vec!["posix-shell".to_owned()],
            },
            state(),
        )
    })?;
    serve_on(socket, main_loop, |core: &mut Core, request: Request| {
        core.dispatch(&request).into()
    })
}

fn serve_core(socket: &Path) -> TestResult<Server<Core>> {
    serve_core_with(socket, CoreState::default)
}

/// The isolated environment every run gets: no `AGTERM_*` or `COLUMNS` from the caller (help wraps at 80 on a pipe),
/// a home and data dir under `root`.
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
        if name.to_string_lossy().starts_with("AGTERM_") || name == "COLUMNS" {
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
fn tree_lists_the_frontmost_window_and_refuses_an_unknown_one() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let _server = serve_core_with(&socket, || CoreState {
        library: WindowLibrary::seeded("/home/u"),
        ..CoreState::default()
    })?;
    let with_socket = |args: &[&str]| -> TestResult<Output> {
        let argv = args
            .iter()
            .map(OsStr::new)
            .chain([OsStr::new("--socket"), socket.as_os_str()]);
        run(dir.path(), &[], argv)
    };

    let json = with_socket(&["tree", "--json"])?;
    let (code, stdout, stderr) = outcome(&json);
    assert_eq!((code, stderr.as_str()), (Some(0), ""));
    let response: Response = serde_json::from_str(&stdout)?;
    let tree = &response.result.ok_or("no result")?["tree"];
    let workspace = &tree["workspaces"][0];
    let session = &workspace["sessions"][0];
    let (wid, sid) = (
        workspace["id"].as_str().ok_or("no workspace id")?,
        session["id"].as_str().ok_or("no session id")?,
    );
    let human = format!("* workspace 1  [{wid}]\n  * u (not realized)  [{sid}]  /home/u\n");
    assert_eq!(
        outcome(&with_socket(&["tree"])?),
        (Some(0), human.clone(), String::new())
    );
    assert_eq!(
        outcome(&with_socket(&["tree", "--window", "active"])?),
        (Some(0), human, String::new())
    );

    let refusal = "no such window: zzz";
    assert_eq!(
        outcome(&with_socket(&["tree", "--window", "zzz"])?),
        (Some(1), String::new(), format!("error: {refusal}\n"))
    );
    let refused_json = with_socket(&["tree", "--json", "--window", "zzz"])?;
    assert_eq!(refused_json.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&refused_json.stdout)?,
        serde_json::json!({"ok": false, "error": refusal})
    );
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
fn a_tree_that_does_not_decode_fails_the_reply_with_or_without_json() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let _server = serve(&socket, (), |(), _| {
        Response::with_result(serde_json::json!({"tree": {"workspaces": 5}})).into()
    })?;
    let base = [OsStr::new("tree"), OsStr::new("--socket"), socket.as_os_str()];
    for output in [
        run(dir.path(), &[], base)?,
        run(dir.path(), &[], base.into_iter().chain([OsStr::new("--json")]))?,
    ] {
        let (code, stdout, stderr) = outcome(&output);
        assert_eq!((code, stdout.as_str()), (Some(1), ""));
        assert!(stderr.starts_with("Error: could not decode response: "), "{stderr}");
    }
    Ok(())
}

const VERSION_USAGE: &str =
    "Usage: agtermctl version [--socket <socket>] [--json]\n  See 'agtermctl version --help' for more information.\n";
const ROOT_USAGE: &str = "Usage: agtermctl <subcommand>\n  See 'agtermctl --help' for more information.\n";
const HELP_USAGE: &str = "Usage: agtermctl help [<subcommands> ...]\n";

#[test]
fn usage_errors_are_reported_as_upstream_before_connecting() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str, &str); 11] = [
        (&["version", "--bogus"], "Unknown option '--bogus'\n", VERSION_USAGE),
        (
            &["version", "--jsn"],
            "Unknown option '--jsn'. Did you mean '--json'?\n",
            VERSION_USAGE,
        ),
        (&["version", "extra"], "Unexpected argument 'extra'\n", VERSION_USAGE),
        (
            &["version", "extra", "more"],
            "2 unexpected arguments: 'extra', 'more'\n",
            VERSION_USAGE,
        ),
        (
            &["version", "--socket"],
            "Missing value for '--socket <socket>'\nHelp:  --socket <socket>  Override the control socket path.\n",
            VERSION_USAGE,
        ),
        (
            &["version", "--json=3"],
            "The option '--json' does not take any value, but '3' was specified.\n",
            VERSION_USAGE,
        ),
        (
            &["--socket", "/tmp/x.sock", "version"],
            "Unknown option '--socket'\n",
            ROOT_USAGE,
        ),
        (&["--json", "version"], "Unknown option '--json'\n", ROOT_USAGE),
        (&["bogus"], "Unexpected argument 'bogus'\n", ROOT_USAGE),
        (
            &["help", "version", "--bogus"],
            "Unknown option '--bogus'\n",
            HELP_USAGE,
        ),
        (&["help", "version", "--json"], "Unknown option '--json'\n", HELP_USAGE),
    ];
    for (args, message, usage) in cases {
        let expected = format!("Error: {message}{usage}");
        assert_eq!(
            outcome(&run(dir.path(), &[], args)?),
            (Some(64), String::new(), expected),
            "{args:?}"
        );
    }
    Ok(())
}

const ROOT_HELP: &str = "\
OVERVIEW: Drive agterm over its control socket, and install its terminfo entry
on other hosts.

USAGE: agtermctl <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  tree                    Print the workspace/session tree.
  version                 Print the version of the app serving the socket.

  See 'agtermctl help <subcommand>' for detailed help.
";

const TREE_HELP: &str = "\
OVERVIEW: Print the workspace/session tree.

USAGE: agtermctl tree [--socket <socket>] [--json] [--window <window>]

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const VERSION_HELP: &str = "\
OVERVIEW: Print the version of the app serving the socket.

USAGE: agtermctl version [--socket <socket>] [--json]

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

#[test]
fn help_goes_to_stdout_in_the_argument_parser_layout() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str); 12] = [
        (&["tree", "--help"], TREE_HELP),
        (&["help", "tree"], TREE_HELP),
        (&[], ROOT_HELP),
        (&["--help"], ROOT_HELP),
        (&["help"], ROOT_HELP),
        (&["bogus", "--help"], ROOT_HELP),
        (&["version", "--help"], VERSION_HELP),
        (&["version", "-h"], VERSION_HELP),
        (&["help", "version"], VERSION_HELP),
        (&["help", "--", "version"], VERSION_HELP),
        (&["help", "version", "--help"], VERSION_HELP),
        (&["version", "--bogus", "--help"], VERSION_HELP),
    ];
    for (args, help) in cases {
        assert_eq!(
            outcome(&run(dir.path(), &[], args)?),
            (Some(0), help.to_owned(), String::new()),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn help_wraps_to_the_columns_variable() -> TestResult {
    let dir = short_dir()?;
    let output = run(dir.path(), &[("COLUMNS", Path::new("200"))], ["--help"])?;
    let overview = "OVERVIEW: Drive agterm over its control socket, and install its terminfo entry on other hosts.\n\n";
    assert!(text(&output.stdout).starts_with(overview), "{}", text(&output.stdout));
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
