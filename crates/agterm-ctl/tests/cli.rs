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
  events                  Continuously print control events.
  workspace               Workspace commands.
  session                 Session commands.
  window                  Window commands.
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

const EVENTS_HELP: &str = "\
OVERVIEW: Continuously print control events.

USAGE: agtermctl events [--socket <socket>] [--json] [--kind <kind> ...] [--run <run>] [--after <after>] [--limit <limit>]

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --kind <kind>           Event kind; repeat or comma-separate values.
  --run <run>             App-run UUID paired with --after.
  --after <after>         Sequence cursor paired with --run.
  --limit <limit>         Events per read (1...1000).
  -h, --help              Show help information.

";

#[test]
fn help_goes_to_stdout_in_the_argument_parser_layout() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str); 14] = [
        (&["events", "--help"], EVENTS_HELP),
        (&["help", "events"], EVENTS_HELP),
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

// MARK: - session and window commands

/// A server that answers `expected` (compared as JSON) with `result` and refuses anything else, naming it, so a
/// test sees the request the CLI built.
fn scripted(socket: &Path, expected: serde_json::Value, result: serde_json::Value) -> TestResult<Server<()>> {
    serve(socket, (), move |(), request| {
        let sent = serde_json::to_value(&request).unwrap_or_default();
        if sent == expected {
            Response::with_result(result.clone()).into()
        } else {
            Response::failure(format!("unexpected request: {sent}")).into()
        }
    })
}

/// Runs `agtermctl args --socket <socket>`.
fn run_on(root: &Path, socket: &Path, args: &[&str]) -> TestResult<Output> {
    let command_line = args
        .iter()
        .map(OsStr::new)
        .chain([OsStr::new("--socket"), socket.as_os_str()]);
    run(root, &[], command_line)
}

const NEW_ID: &str = "0F2C7A4E-9B1D-4C3A-8E5F-1A2B3C4D5E6F";

#[test]
fn session_new_and_duplicate_echo_the_new_id() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let expected = serde_json::json!({"cmd": "session.new", "args": {"name": "build", "noSelect": true}});
    let _server = scripted(&socket, expected, serde_json::json!({"id": NEW_ID}))?;
    let args = ["session", "new", "--name", "build", "--no-select"];
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &args)?),
        (Some(0), format!("{NEW_ID}\n"), String::new())
    );
    let json = run_on(dir.path(), &socket, &[&args[..], &["--json"]].concat())?;
    let line = format!("{{\"ok\":true,\"result\":{{\"id\":\"{NEW_ID}\"}}}}\n");
    assert_eq!(outcome(&json), (Some(0), line, String::new()));

    let duplicated = dir.path().join("dup.sock");
    let expected = serde_json::json!({"cmd": "session.duplicate", "target": "9f3c"});
    let _server = scripted(&duplicated, expected, serde_json::json!({"id": NEW_ID}))?;
    assert_eq!(
        outcome(&run_on(
            dir.path(),
            &duplicated,
            &["session", "duplicate", "--target", "9f3c"]
        )?),
        (Some(0), format!("{NEW_ID}\n"), String::new())
    );
    Ok(())
}

#[test]
fn commands_naming_their_target_print_ok_and_a_batch_its_count() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], serde_json::Value, serde_json::Value, &str); 6] = [
        (
            &["session", "select", "--target", "s1"],
            serde_json::json!({"cmd": "session.select", "target": "s1"}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n",
        ),
        (
            &["session", "go", "--to", "next", "--window", "w1"],
            serde_json::json!({"cmd": "session.go", "args": {"to": "next", "window": "w1"}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n",
        ),
        (
            &["session", "rename", "build", "--target", "s1"],
            serde_json::json!({"cmd": "session.rename", "target": "s1", "args": {"name": "build"}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n",
        ),
        (
            &["session", "reveal"],
            serde_json::json!({"cmd": "session.reveal", "target": "active"}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["session", "close", "--target", "b", "--target", "a"],
            serde_json::json!({"cmd": "session.close", "target": "b", "args": {"targets": ["b", "a"]}}),
            serde_json::json!({"affected": 2}),
            "2 sessions\n",
        ),
        (
            &["session", "move", "ws2", "--target", "s1", "--target", "s2"],
            serde_json::json!({"cmd": "session.move", "target": "s1",
                               "args": {"targets": ["s1", "s2"], "workspace": "ws2"}}),
            serde_json::json!({"affected": 1}),
            "1 session\n",
        ),
    ];
    for (at, (args, expected, result, stdout)) in cases.into_iter().enumerate() {
        let socket = dir.path().join(format!("{at}.sock"));
        let _server = scripted(&socket, expected, result)?;
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(0), stdout.to_owned(), String::new()),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn window_list_renders_one_line_per_window() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let windows = serde_json::json!({"windows": [
        {"id": "AAAA", "name": "work", "open": true, "active": true, "sidebarVisible": true},
        {"id": "BBBB", "name": "archive", "open": false, "active": false},
    ]});
    let _server = scripted(&socket, serde_json::json!({"cmd": "window.list"}), windows.clone())?;
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["window", "list"])?),
        (
            Some(0),
            "AAAA  work [open] [active]\nBBBB  archive\n".to_owned(),
            String::new()
        )
    );
    let json = run_on(dir.path(), &socket, &["window", "list", "--json"])?;
    let (code, stdout, stderr) = outcome(&json);
    assert_eq!((code, stderr.as_str()), (Some(0), ""));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout)?,
        serde_json::json!({"ok": true, "result": windows})
    );
    Ok(())
}

#[test]
fn a_window_list_that_does_not_decode_fails_the_reply() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let malformed = serde_json::json!({"windows": [{"id": "AAAA"}]});
    let _server = scripted(&socket, serde_json::json!({"cmd": "window.list"}), malformed)?;
    for json in [false, true] {
        let args: &[&str] = if json {
            &["window", "list", "--json"]
        } else {
            &["window", "list"]
        };
        let (code, stdout, stderr) = outcome(&run_on(dir.path(), &socket, args)?);
        assert_eq!((code, stdout.as_str()), (Some(1), ""), "{args:?}");
        assert!(stderr.starts_with("Error: could not decode response: "), "{stderr}");
    }
    Ok(())
}

#[test]
fn a_session_refusal_exits_1_with_the_server_error() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let _server = serve(&socket, (), |(), _| Response::failure("no such session: zzz").into())?;
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["session", "close", "--target", "zzz"])?),
        (Some(1), String::new(), "error: no such session: zzz\n".to_owned())
    );
    let json = run_on(dir.path(), &socket, &["session", "close", "--target", "zzz", "--json"])?;
    let line = "{\"ok\":false,\"error\":\"no such session: zzz\"}\n".to_owned();
    assert_eq!(outcome(&json), (Some(1), line, String::new()));
    Ok(())
}

const MOVE_USAGE: &str = "Usage: agtermctl session move [<workspace>] [--to <to>] [--after <after>] [--before \
                          <before>] [--target <target> ...] [--socket <socket>] [--json] [--window <window>]\n  See \
                          'agtermctl session move --help' for more information.\n";
const NEW_USAGE: &str = "Usage: agtermctl session new <options>\n  See 'agtermctl session new --help' for more \
                         information.\n";

#[test]
fn validation_refusals_are_usage_errors_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let cases: [(&[&str], &str, &str); 4] = [
        (
            &["session", "move", "--to", "top", "--after", "s2", "--target", "s1"],
            "session.move takes --after/--before or --to, not both",
            MOVE_USAGE,
        ),
        (
            &["session", "move"],
            "provide a destination workspace, --to, or --after/--before",
            MOVE_USAGE,
        ),
        (
            &["session", "new", "--workspace", "active", "--workspace-name", "x"],
            "use either --workspace or --workspace-name, not both",
            NEW_USAGE,
        ),
        (&["session", "new", "--wait"], "--wait requires --command", NEW_USAGE),
    ];
    for (args, message, usage) in cases {
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(64), String::new(), format!("Error: {message}\n{usage}")),
            "{args:?}"
        );
    }
    // help wins over a refusal, as the library looks for it first
    let help = run_on(dir.path(), &socket, &["session", "new", "--wait", "--help"])?;
    assert_eq!(help.status.code(), Some(0));
    Ok(())
}

#[test]
fn parse_errors_of_the_new_commands_name_their_usage() -> TestResult {
    let dir = short_dir()?;
    let go_usage = "Usage: agtermctl session go --to <to> [--socket <socket>] [--json] [--window <window>]\n  See \
                    'agtermctl session go --help' for more information.\n";
    let list_usage = "Usage: agtermctl window list [--socket <socket>] [--json]\n  See 'agtermctl window list --help' for more \
         information.\n";
    let cases: [(&[&str], String); 3] = [
        (
            &["session", "go"],
            format!(
                "Error: Missing expected argument '--to <to>'\nHelp:  --to <to>  Direction: next, prev, first, last, \
                 next-attention, or prev-attention (attention = blocked/completed).\n{go_usage}"
            ),
        ),
        (
            &["window", "list", "--window", "w1"],
            format!("Error: Unknown option '--window'\n{list_usage}"),
        ),
        (
            &["session", "bogus"],
            "Error: Unexpected argument 'bogus'\nUsage: agtermctl session <subcommand>\n  See 'agtermctl session \
             --help' for more information.\n"
                .to_owned(),
        ),
    ];
    for (args, expected) in cases {
        assert_eq!(
            outcome(&run(dir.path(), &[], args)?),
            (Some(64), String::new(), expected),
            "{args:?}"
        );
    }
    Ok(())
}

const SESSION_HELP: &str = "\
OVERVIEW: Session commands.

USAGE: agtermctl session <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  new                     Create a session.
  duplicate               Duplicate a session: a fresh shell in its directory,
                          placed right after it.
  close                   Close a session.
  select                  Select a session.
  go                      Navigate sessions:
                          next|prev|first|last|next-attention|prev-attention.
  rename                  Rename a session.
  reveal                  Reveal a session's focused working directory in
                          Finder.
  move                    Move a session: to another workspace, reorder with
                          --to, or place relative to an anchor with
                          --after/--before.

  See 'agtermctl help session <subcommand>' for detailed help.
";

const SESSION_NEW_HELP: &str = "\
OVERVIEW: Create a session.

USAGE: agtermctl session new <options>

OPTIONS:
  --cwd <cwd>             Working directory (defaults to $HOME).
  --workspace <workspace> Target workspace by id/prefix/active (defaults to the
                          current one). Mutually exclusive with
                          --workspace-name.
  --workspace-name <workspace-name>
                          Target workspace by name; errors if not found unless
                          --create-workspace. Mutually exclusive with
                          --workspace.
  --create-workspace      With --workspace-name, create the workspace when it
                          does not exist (reuse it otherwise).
  --command <command>     Run this command as the session's process instead of
                          the login shell (no echoed command line; the session
                          closes when it exits).
  --wait                  With --command, hold the session open after the
                          command exits (press any key to close) instead of
                          closing immediately.
  --name <name>           Initial session name (defaults to the auto basename).
  --after <after>         Place the new session right AFTER this anchor session
                          (id/prefix/active); the anchor carries its own
                          workspace, replacing --workspace.
  --before <before>       Place the new session right BEFORE this anchor
                          session (id/prefix/active); mirror of --after.
  --no-select             Create the session in the background without
                          selecting or focusing it (leaves the current
                          selection untouched).
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const SESSION_DUPLICATE_HELP: &str = "\
OVERVIEW: Duplicate a session: a fresh shell in its directory, placed right
after it.

USAGE: agtermctl session duplicate [--target <target>] [--socket <socket>] [--json] [--window <window>]

OPTIONS:
  --target <target>       Target session/workspace id, unique prefix, or
                          'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const SESSION_CLOSE_HELP: &str = "\
OVERVIEW: Close a session.

USAGE: agtermctl session close [--target <target> ...] [--socket <socket>] [--json] [--window <window>]

OPTIONS:
  --target <target>       Target session id, prefix, or 'active'. Repeat for a
                          batch.
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const SESSION_SELECT_HELP: &str = "\
OVERVIEW: Select a session.

USAGE: agtermctl session select [--target <target>] [--socket <socket>] [--json] [--window <window>]

OPTIONS:
  --target <target>       Target session/workspace id, unique prefix, or
                          'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const SESSION_GO_HELP: &str = "\
OVERVIEW: Navigate sessions: next|prev|first|last|next-attention|prev-attention.

USAGE: agtermctl session go --to <to> [--socket <socket>] [--json] [--window <window>]

OPTIONS:
  --to <to>               Direction: next, prev, first, last, next-attention,
                          or prev-attention (attention = blocked/completed).
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const SESSION_RENAME_HELP: &str = "\
OVERVIEW: Rename a session.

USAGE: agtermctl session rename <name> [--target <target>] [--socket <socket>] [--json] [--window <window>]

ARGUMENTS:
  <name>                  New session name.

OPTIONS:
  --target <target>       Target session/workspace id, unique prefix, or
                          'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const SESSION_REVEAL_HELP: &str = "\
OVERVIEW: Reveal a session's focused working directory in Finder.

USAGE: agtermctl session reveal [--target <target>] [--socket <socket>] [--json] [--window <window>]

OPTIONS:
  --target <target>       Target session/workspace id, unique prefix, or
                          'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const SESSION_MOVE_HELP: &str = "\
OVERVIEW: Move a session: to another workspace, reorder with --to, or place
relative to an anchor with --after/--before.

USAGE: agtermctl session move [<workspace>] [--to <to>] [--after <after>] [--before <before>] [--target <target> ...] [--socket <socket>] [--json] [--window <window>]

ARGUMENTS:
  <workspace>             Destination workspace id/prefix (relocate). Omit with
                          --to or --after/--before.

OPTIONS:
  --to <to>               Reorder within the workspace: up, down, top, or
                          bottom.
  --after <after>         Place right AFTER this anchor session
                          (id/prefix/active); the anchor carries its own
                          workspace (relocates + positions in one shot).
  --before <before>       Place right BEFORE this anchor session
                          (id/prefix/active); mirror of --after.
  --target <target>       Target session id, prefix, or 'active'. Repeat for a
                          batch.
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const WINDOW_HELP: &str = "\
OVERVIEW: Window commands.

USAGE: agtermctl window <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  list                    List windows (id, name, open, active).

  See 'agtermctl help window <subcommand>' for detailed help.
";

const WINDOW_LIST_HELP: &str = "\
OVERVIEW: List windows (id, name, open, active).

USAGE: agtermctl window list [--socket <socket>] [--json]

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

#[test]
fn session_and_window_help_in_the_argument_parser_layout() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str); 14] = [
        (&["session"], SESSION_HELP),
        (&["session", "--help"], SESSION_HELP),
        (&["help", "session"], SESSION_HELP),
        (&["session", "new", "--help"], SESSION_NEW_HELP),
        (&["session", "duplicate", "--help"], SESSION_DUPLICATE_HELP),
        (&["session", "close", "-h"], SESSION_CLOSE_HELP),
        (&["session", "select", "--help"], SESSION_SELECT_HELP),
        (&["session", "go", "--help"], SESSION_GO_HELP),
        (&["help", "session", "rename"], SESSION_RENAME_HELP),
        (&["session", "reveal", "--help"], SESSION_REVEAL_HELP),
        (&["session", "move", "--help"], SESSION_MOVE_HELP),
        (&["window"], WINDOW_HELP),
        (&["window", "list", "--help"], WINDOW_LIST_HELP),
        (&["help", "window", "list"], WINDOW_LIST_HELP),
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

const WORKSPACE_HELP: &str = "\
OVERVIEW: Workspace commands.

USAGE: agtermctl workspace <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  new                     Create a workspace.
  rename                  Rename a workspace.
  delete                  Delete a workspace.
  select                  Select a workspace.
  go                      Navigate workspaces: next|prev.
  move                    Reorder a workspace among its siblings.
  focus                   Mark a workspace in the sidebar focus set
                          (on|off|toggle|add).
  filter                  Turn the sidebar workspace focus filter on or off
                          (on|off|toggle).
  collapse                Collapse a workspace in the sidebar tree.
  expand                  Expand a workspace in the sidebar tree.

  See 'agtermctl help workspace <subcommand>' for detailed help.
";

const WORKSPACE_NEW_HELP: &str = "\
OVERVIEW: Create a workspace.

USAGE: agtermctl workspace new [<name>] [--collapsed] [--socket <socket>] [--json] [--window <window>]

ARGUMENTS:
  <name>                  Workspace name (defaults to the auto-generated name).

OPTIONS:
  --collapsed             Create the workspace collapsed in the sidebar.
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const WORKSPACE_FOCUS_HELP: &str = "\
OVERVIEW: Mark a workspace in the sidebar focus set (on|off|toggle|add).

USAGE: agtermctl workspace focus [<mode>] [--target <target>] [--socket <socket>] [--json] [--window <window>]

ARGUMENTS:
  <mode>                  Mode: on (mark it alone and apply the filter), off
                          (unmark it; the filter switches off as the set
                          empties), toggle (replace-toggle and apply the
                          filter, the default; clears when it is the only
                          marked one AND the filter is applied), add (mark it
                          alongside the others, leaving the filter flag alone).
                          (default: toggle)

OPTIONS:
  --target <target>       Target session/workspace id, unique prefix, or
                          'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const WORKSPACE_FILTER_HELP: &str = "\
OVERVIEW: Turn the sidebar workspace focus filter on or off (on|off|toggle).

USAGE: agtermctl workspace filter [<mode>] [--socket <socket>] [--json] [--window <window>]

ARGUMENTS:
  <mode>                  Mode: on, off, or toggle (default). (default: toggle)

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

#[test]
fn workspace_help_in_the_argument_parser_layout() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str); 5] = [
        (&["workspace"], WORKSPACE_HELP),
        (&["help", "workspace"], WORKSPACE_HELP),
        (&["workspace", "new", "--help"], WORKSPACE_NEW_HELP),
        (&["workspace", "focus", "--help"], WORKSPACE_FOCUS_HELP),
        (&["help", "workspace", "filter"], WORKSPACE_FILTER_HELP),
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
fn workspace_modes_are_refused_before_connecting() -> TestResult {
    let dir = short_dir()?;
    for (args, refusal) in [
        (
            &["workspace", "focus", "pin"][..],
            "Error: mode must be one of: on, off, toggle, add\nUsage: agtermctl workspace focus [<mode>] [--target \
             <target>] [--socket <socket>] [--json] [--window <window>]\n  See 'agtermctl workspace focus --help' for \
             more information.\n",
        ),
        (
            &["workspace", "filter", "show"],
            "Error: mode must be on, off, or toggle\nUsage: agtermctl workspace filter [<mode>] [--socket <socket>] \
             [--json] [--window <window>]\n  See 'agtermctl workspace filter --help' for more information.\n",
        ),
    ] {
        assert_eq!(
            outcome(&run(dir.path(), &[], args)?),
            (Some(64), String::new(), refusal.to_owned()),
            "{args:?}"
        );
    }
    Ok(())
}

// MARK: - events

const EVENT_RUN: &str = "CBB5E3D0-7A9B-4C96-9EA2-18B14380DDB1";

/// A ring that answers `first` with one page, then refuses the read resuming from it with `event cursor expired`, and
/// anything else by naming it: the stream ends on that refusal.
fn event_ring(socket: &Path, first: serde_json::Value) -> TestResult<Server<usize>> {
    let page = serde_json::json!({"events": {"run": EVENT_RUN, "next": 7, "items": [
        {"seq": 6, "ts": 10.5, "kind": "status", "session": "S",
         "payload": {"name": "api", "status": "active", "blink": true}},
        {"seq": 7, "ts": 3661, "kind": "tree.changed", "window": "win", "payload": {}},
    ]}});
    serve(socket, 0, move |reads: &mut usize, request| {
        *reads += 1;
        let sent = serde_json::to_value(&request).unwrap_or_default();
        let resumed = sent.get("args").is_some_and(|args| {
            args.get("run").and_then(serde_json::Value::as_str) == Some(EVENT_RUN)
                && args.get("after").and_then(serde_json::Value::as_str) == Some("7")
        });
        match *reads {
            1 if sent == first => Response::with_result(page.clone()).into(),
            2 if resumed => Response::failure("event cursor expired").into(),
            _ => Response::failure(format!("unexpected request: {sent}")).into(),
        }
    })
}

#[test]
fn events_print_each_page_in_local_time_until_the_server_refuses() -> TestResult {
    let dir = short_dir()?;
    let refused = "Error: event cursor expired\n".to_owned();
    for (zone, printed) in [
        ("UTC", "00:00:10 status api active blink\n01:01:01 tree.changed win\n"),
        // POSIX spells three hours east of UTC with a minus
        ("XXX-3", "03:00:10 status api active blink\n04:01:01 tree.changed win\n"),
    ] {
        let socket = dir.path().join(format!("{zone}.sock"));
        let _server = event_ring(&socket, serde_json::json!({"cmd": "events.read"}))?;
        let command_line = [OsStr::new("events"), OsStr::new("--socket"), socket.as_os_str()];
        let output = run(dir.path(), &[("TZ", Path::new(zone))], command_line)?;
        assert_eq!(
            outcome(&output),
            (Some(1), printed.to_owned(), refused.clone()),
            "{zone}"
        );
    }

    let socket = dir.path().join("agterm.sock");
    let first = serde_json::json!({"cmd": "events.read", "args": {"kinds": ["status", "tree.changed"], "limit": 2}});
    let _server = event_ring(&socket, first)?;
    let args = ["events", "--json", "--kind", "tree.changed,status", "--limit", "2"];
    let (code, stdout, stderr) = outcome(&run_on(dir.path(), &socket, &args)?);
    assert_eq!((code, stderr), (Some(1), refused));
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            r#"{"seq":6,"ts":10.5,"kind":"status","session":"S","payload":{"name":"api","status":"active","blink":true}}"#,
            r#"{"seq":7,"ts":3661.0,"kind":"tree.changed","window":"win","payload":{}}"#,
        ]
    );
    Ok(())
}

#[test]
fn events_without_a_cursor_fail_at_once_when_nothing_listens() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let output = run_on(dir.path(), &socket, &["events"])?;
    let error = connect_failure(&socket, "No such file or directory", NOT_RUNNING);
    assert_eq!(outcome(&output), (Some(1), String::new(), error));
    Ok(())
}

const EVENTS_USAGE: &str = "Usage: agtermctl events [--socket <socket>] [--json] [--kind <kind> ...] [--run <run>] \
                            [--after <after>] [--limit <limit>]\n  See 'agtermctl events --help' for more \
                            information.\n";

#[test]
fn events_refuse_a_bad_cursor_limit_or_kind_before_reading() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a read would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let cases: [(&[&str], &str); 5] = [
        (&["--limit", "0"], "event limit must be between 1 and 1000"),
        (&["--run", EVENT_RUN], "events.read requires --run and --after together"),
        (&["--run", "not-a-run", "--after", "1"], "invalid event run id"),
        (&["--run", EVENT_RUN, "--after", "x"], "invalid event cursor"),
        (
            &["--kind", "status, session.renamed"],
            "invalid event kind: session.renamed",
        ),
    ];
    for (args, message) in cases {
        let command_line = [&["events"][..], args].concat();
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, &command_line)?),
            (Some(64), String::new(), format!("Error: {message}\n{EVENTS_USAGE}")),
            "{args:?}"
        );
    }
    Ok(())
}
