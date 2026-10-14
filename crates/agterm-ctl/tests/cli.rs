//! The built `agtermctl` against an in-process control server over the fake main loop: socket resolution, `version`,
//! exit statuses and what lands on stdout and stderr.

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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
  surface                 Terminal surface commands.
  dashboard               Open a view-only grid of live sessions, or --close
                          the open one.
  window                  Window commands.
  quick                   Quick terminal: visibility, type into it, read its
                          text.
  sidebar                 Sidebar visibility and view mode.
  notify                  Post a desktop notification (default: the active
                          session of the frontmost window).
  font                    Font size commands.
  keymap                  Keymap commands.
  hooks                   Event hook commands.
  browser                 Saved browser storage of --persistent URL overlays,
                          and where clicked links open.
  config                  Config commands.
  theme                   Theme commands.
  pick                    Open, poll, or cancel a native fuzzy picker.
  ask                     Open, poll, or cancel a themed question dialog.
  restore                 Restore-running-command commands.
  zmx                     Inspect and manage the daemons behind live sessions.
  terminfo                The terminal's terminfo entry on other hosts.
  clipboard               The clipboard of the terminals showing this pane.
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
fn window_commands_print_the_new_id_the_applied_size_or_ok() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], serde_json::Value, serde_json::Value, String); 4] = [
        (
            &["window", "new", "Work", "--minimized"],
            serde_json::json!({"cmd": "window.new", "args": {"name": "Work", "minimized": true}}),
            serde_json::json!({"id": NEW_ID}),
            format!("{NEW_ID}\n"),
        ),
        (
            &["window", "resize", "--width", "1000", "--height", "700"],
            serde_json::json!({"cmd": "window.resize", "target": "active",
                               "args": {"width": 1000, "height": 700}}),
            serde_json::json!({"id": NEW_ID, "width": 900, "height": 640}),
            "900 640\n".to_owned(),
        ),
        (
            &["window", "minimize", "on"],
            serde_json::json!({"cmd": "window.minimize", "target": "active", "args": {"mode": "on"}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n".to_owned(),
        ),
        (
            &["window", "move", "9f3c", "--x", "100", "--y", "50", "--display", "1"],
            serde_json::json!({"cmd": "window.move", "target": "9f3c",
                               "args": {"x": 100, "y": 50, "display": 1}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n".to_owned(),
        ),
    ];
    for (at, (args, expected, result, stdout)) in cases.into_iter().enumerate() {
        let socket = dir.path().join(format!("{at}.sock"));
        let _server = scripted(&socket, expected, result)?;
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(0), stdout, String::new()),
            "{args:?}"
        );
    }
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

#[test]
fn restore_prints_its_note_and_background_and_restart_send_their_args() -> TestResult {
    let dir = short_dir()?;
    let note = "saved for rerun mode; active restore mode is none";
    let cases: [(&[&str], serde_json::Value, serde_json::Value, String); 4] = [
        (
            &["session", "restore", "echo pinned", "--target", "s1"],
            serde_json::json!({"cmd": "session.restore", "target": "s1",
                               "args": {"mode": "set", "command": "echo pinned"}}),
            serde_json::json!({"id": NEW_ID, "text": note, "pane": "left"}),
            format!("{note}\n"),
        ),
        (
            &["session", "restore", "--clear", "--pane", "right"],
            serde_json::json!({"cmd": "session.restore", "target": "active",
                               "args": {"mode": "clear", "pane": "right"}}),
            serde_json::json!({"id": NEW_ID, "pane": "right"}),
            "ok\n".to_owned(),
        ),
        (
            &[
                "session",
                "background",
                "text",
                "WATERMARK",
                "--color",
                "#336699",
                "--pane",
                "scratch",
            ],
            serde_json::json!({"cmd": "session.background", "target": "active",
                               "args": {"text": "WATERMARK", "mode": "text", "color": "#336699", "pane": "scratch"}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n".to_owned(),
        ),
        (
            &["session", "restart", "--pane-id", "T", "--command", "cat"],
            serde_json::json!({"cmd": "session.restart", "target": "active",
                               "args": {"paneID": "T", "command": "cat"}}),
            serde_json::json!({"id": NEW_ID, "text": "restarted left pane: shell 1 -> 2", "pane": "left",
                               "restart": {"paneID": "T", "oldPid": 1, "newPid": 2}}),
            "restarted left pane: shell 1 -> 2\n".to_owned(),
        ),
    ];
    for (index, (args, expected, result, printed)) in cases.into_iter().enumerate() {
        let socket = dir.path().join(format!("{index}.sock"));
        let _server = scripted(&socket, expected, result)?;
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(0), printed, String::new()),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn zmx_tree_and_present_send_their_args_and_print_the_sessions() -> TestResult {
    let dir = short_dir()?;
    let remote = serde_json::json!({"host": "buildbox", "endpoint": {"executable": "/z", "socketDirectory": "/t"},
        "presentation": 1, "sessions": [{"id": "S1", "name": "build", "windowID": "W", "windowName": "main",
        "workspaceID": "WS", "workspaceName": "dev", "context": "release prep", "cwd": "/repo",
        "splitAxis": "vertical", "panes": [
            {"pane": "left", "daemon": "agterm-a", "foreground": ["/usr/bin/tail", "-f"]},
            {"pane": "right", "daemon": "agterm-b"}]}]});
    let cases: [(&[&str], serde_json::Value, serde_json::Value, String); 3] = [
        (
            &["zmx", "tree"],
            serde_json::json!({"cmd": "zmx.tree"}),
            serde_json::json!({"remote": {"endpoint": {"executable": "/z", "socketDirectory": "/t"},
                                          "sessions": []}}),
            "no attachable sessions\n".to_owned(),
        ),
        (
            &["zmx", "tree", "buildbox"],
            serde_json::json!({"cmd": "zmx.tree", "args": {"host": "buildbox"}}),
            serde_json::json!({"remote": remote}),
            "  main/dev/build (split vertical)  [S1]  /repo  release prep  tail\n".to_owned(),
        ),
        (
            // the app answers and hangs up: nothing but frames would reach stdout
            &["zmx", "present", "S1"],
            serde_json::json!({"cmd": "zmx.present", "target": "S1"}),
            serde_json::json!({"id": "S1"}),
            String::new(),
        ),
    ];
    for (index, (args, expected, result, printed)) in cases.into_iter().enumerate() {
        let socket = dir.path().join(format!("{index}.sock"));
        let _server = scripted(&socket, expected, result)?;
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(0), printed, String::new()),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn zmx_commands_send_their_args_and_print_the_inventory() -> TestResult {
    let dir = short_dir()?;
    let restore = serde_json::json!({"configured": "live", "requestedAtLaunch": "live", "active": "live",
                                     "restartRequired": false});
    let cases: [(&[&str], serde_json::Value, serde_json::Value, String); 4] = [
        (
            &["zmx", "list"],
            serde_json::json!({"cmd": "zmx.list"}),
            serde_json::json!({"zmx": {"restore": restore, "inventoryComplete": true, "entries": []}}),
            "configured: live (next launch)\nthis launch: requested live, active live\nno daemons\n".to_owned(),
        ),
        (
            &["zmx", "screen", "agterm-a", "--lines", "3"],
            serde_json::json!({"cmd": "zmx.screen", "args": {"name": "agterm-a", "lines": 3}}),
            serde_json::json!({"text": "a\nb\nc"}),
            "a\nb\nc\n".to_owned(),
        ),
        (
            &[
                "zmx", "kill", "--pane", "right", "--force", "--target", "s1", "--window", "w1",
            ],
            serde_json::json!({"cmd": "zmx.kill", "target": "s1",
                               "args": {"force": true, "window": "w1", "pane": "right"}}),
            serde_json::json!({"id": NEW_ID, "text": "killed agterm-a", "pane": "right"}),
            "killed agterm-a\n".to_owned(),
        ),
        (
            &["zmx", "reset", "--force"],
            serde_json::json!({"cmd": "zmx.reset", "args": {"force": true}}),
            serde_json::json!({"text": "1 live session will be reset.",
                               "liveReset": {"sessions": 1, "panes": 1, "pending": true}}),
            "1 live session will be reset.\n".to_owned(),
        ),
    ];
    for (index, (args, expected, result, printed)) in cases.into_iter().enumerate() {
        let socket = dir.path().join(format!("{index}.sock"));
        let _server = scripted(&socket, expected, result)?;
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(0), printed, String::new()),
            "{args:?}"
        );
    }
    let socket = dir.path().join("absent.sock");
    let refusals: [(&[&str], &str); 5] = [
        (
            &["zmx", "kill", "--pane", "left", "--target", "s1"],
            "--force is required to destroy a running process",
        ),
        (
            &["zmx", "kill", "--pane", "middle", "--force", "--target", "s1"],
            "--pane must be left/primary/top or right/split/bottom",
        ),
        (
            &["zmx", "kill", "--pane", "left", "--force", "--target", "active"],
            "--target must name a session; 'active' is not accepted here",
        ),
        (
            &["zmx", "reset"],
            "--force is required to end the processes in the affected live sessions",
        ),
        (
            &["zmx", "screen", "agterm-a", "--all", "--lines", "2"],
            "use either --all or --lines, not both",
        ),
    ];
    for (args, message) in refusals {
        let (code, stdout, stderr) = outcome(&run_on(dir.path(), &socket, args)?);
        assert_eq!((code, stdout.as_str()), (Some(64), ""), "{args:?}");
        assert!(stderr.starts_with(&format!("Error: {message}\nUsage: ")), "{stderr}");
    }
    Ok(())
}

#[test]
fn zmx_present_reports_a_refusal_on_stderr_and_writes_no_frame() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let message = "session is not live-backed, so nothing can be attached to it";
    let _server = serve(&socket, (), move |(), _| Response::failure(message).into())?;
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["zmx", "present", "S1"])?),
        (Some(1), String::new(), format!("Error: {message}\n"))
    );
    Ok(())
}

#[test]
fn restore_background_and_restart_refuse_bad_forms_before_connecting() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("absent.sock");
    let cases: [(&[&str], &str); 4] = [
        (
            &["session", "restore", "--none", "--clear"],
            "provide exactly one of a COMMAND, --none, or --clear",
        ),
        (&["session", "restart"], "provide --pane-id or --pane"),
        (
            &["session", "background", "color", "blue"],
            "color must be a #rrggbb hex value",
        ),
        (
            &["session", "background", "image", "/a.png", "--opacity", "2"],
            "opacity must be between 0.0 and 1.0",
        ),
    ];
    for (args, message) in cases {
        let (code, stdout, stderr) = outcome(&run_on(dir.path(), &socket, args)?);
        assert_eq!((code, stdout.as_str()), (Some(64), ""), "{args:?}");
        assert!(stderr.starts_with(&format!("Error: {message}\nUsage: ")), "{stderr}");
    }
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
    let cases: [(&[&str], String); 4] = [
        (
            &["window", "resize", "--width", "1000"],
            "Error: Missing expected argument '--height <height>'\nHelp:  --height <height>  New height in points.\n\
             Usage: agtermctl window resize [<id>] --width <width> --height <height> [--socket <socket>] [--json]\n  \
             See 'agtermctl window resize --help' for more information.\n"
                .to_owned(),
        ),
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
  type                    Inject text into a session.
  split                   Show, hide, or close a session split.
  swap                    Exchange a split session's two panes, including their
                          live roles and state.
  lead                    Take the lead of a pane's terminal for this Mac.
  scratch                 Show or hide a session scratch terminal
                          (on|off|toggle).
  focus                   Focus a split session's pane by position or role.
  resize                  Resize a split session's divider (set or nudge the
                          primary-pane fraction).
  copy                    Print a session's selected text (does not touch the
                          system clipboard).
  paste                   Paste the system clipboard into a session (like ⌘V).
  select-all              Select a session's entire terminal buffer (like ⌘A).
  text                    Print a session's terminal buffer as plain text (does
                          not touch the system clipboard).
  status                  Set a session's agent status indicator.
  restore                 Pin the command a session's pane re-runs on the next
                          launch.
  restart                 Replace the shell of one live pane and run a shell
                          line, or the same program again, in the new one.
  flag                    Flag a session for the flagged working-set view
                          (on|off|toggle|clear).
  context                 Set or clear what a session is about, shown in the
                          title bar.
  seen                    Clear a session's unseen-notification badge without
                          changing the selection or focus (idempotent).
  search                  Search a session's terminal output (open the bar, set
                          a needle, or step matches).
  background              Set or clear a session's background (image,
                          rasterized text, or solid color).
  overlay                 Open, read, resize, or close an ephemeral overlay
                          terminal on a session.
  hud                     Post, update, or close a passive message panel over a
                          session.

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
  new                     Create and open a window.
  list                    List windows (id, name, open, active).
  select                  Select (raise or open) a window.
  go                      Navigate open windows: next|prev.
  close                   Close a window (its bundle is kept).
  rename                  Rename a window.
  delete                  Delete a window (keeps at least one).
  resize                  Resize a window (frame size in points).
  move                    Move a window (top-left x,y in points, relative to a
                          display).
  zoom                    Zoom a window (maximize-to-screen toggle).
  fullscreen              Toggle native macOS full screen for a window.
  minimize                Minimize a window to the Dock, or restore it.

  See 'agtermctl help window <subcommand>' for detailed help.
";

const WINDOW_RESIZE_HELP: &str = "\
OVERVIEW: Resize a window (frame size in points).

Prints the applied width and height as W H after clamping to the window minimum
and display bounds.

USAGE: agtermctl window resize [<id>] --width <width> --height <height> [--socket <socket>] [--json]

ARGUMENTS:
  <id>                    Window id, unique prefix, or 'active'. (default:
                          active)

OPTIONS:
  --width <width>         New width in points.
  --height <height>       New height in points.
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

const WINDOW_MINIMIZE_HELP: &str = "\
OVERVIEW: Minimize a window to the Dock, or restore it.

USAGE: agtermctl window minimize [<id>] [<mode>] [--socket <socket>] [--json]

ARGUMENTS:
  <id>                    Window id, unique prefix, or 'active'. (default:
                          active)
  <mode>                  on, off, or toggle (default: toggle).

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

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
    let cases: [(&[&str], &str); 16] = [
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
        (&["window", "resize", "--help"], WINDOW_RESIZE_HELP),
        (&["help", "window", "minimize"], WINDOW_MINIMIZE_HELP),
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

// MARK: - terminal I/O, session metadata and surface commands

/// Runs `agtermctl args --socket <socket>` with `input` on its standard input.
fn run_with_input(root: &Path, socket: &Path, args: &[&str], input: &[u8]) -> TestResult<Output> {
    let mut command = Command::new(AGTERMCTL);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("AGTERM_") || name == "COLUMNS" {
            command.env_remove(name);
        }
    }
    command
        .envs(isolated(root))
        .args(args)
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    child.stdin.take().ok_or("no stdin")?.write_all(input)?;
    Ok(child.wait_with_output()?)
}

#[test]
fn reads_print_their_text_a_search_its_counter_and_a_cursor_its_column() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], serde_json::Value, serde_json::Value, &str); 8] = [
        (
            &["session", "text", "--lines", "2", "--target", "s1"],
            serde_json::json!({"cmd": "session.text", "target": "s1", "args": {"lines": 2}}),
            serde_json::json!({"id": NEW_ID, "text": "$ echo hi\nhi\n", "pane": "left"}),
            "$ echo hi\nhi\n\n",
        ),
        (
            &["session", "copy"],
            serde_json::json!({"cmd": "session.copy", "target": "active"}),
            serde_json::json!({"id": NEW_ID, "text": "selected"}),
            "selected\n",
        ),
        (
            &["session", "search", "needle", "--target", "s1"],
            serde_json::json!({"cmd": "session.search", "target": "s1", "args": {"text": "needle"}}),
            serde_json::json!({"id": NEW_ID, "text": "1/3", "count": 3}),
            "1/3\n",
        ),
        (
            &["session", "search", "--close", "--target", "s1"],
            serde_json::json!({"cmd": "session.search", "target": "s1", "args": {"to": "close"}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n",
        ),
        (
            &["surface", "cursor", "--target", "surface:S:left"],
            serde_json::json!({"cmd": "surface.cursor", "target": "surface:S:left", "args": {}}),
            serde_json::json!({"id": "surface:S:left", "cursor": {"column": 7}}),
            "7\n",
        ),
        (
            &["surface", "zoom", "show", "--target", "surface:S:left"],
            serde_json::json!({"cmd": "surface.zoom", "target": "surface:S:left", "args": {"mode": "show"}}),
            serde_json::json!({"id": "surface:S:left"}),
            "ok\n",
        ),
        (
            &[
                "session", "status", "active", "--blink", "--color", "#ff8800", "--pane", "left", "--target", "s1",
            ],
            serde_json::json!({"cmd": "session.status", "target": "s1",
                               "args": {"pane": "left", "status": "active", "blink": true, "color": "#ff8800"}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n",
        ),
        (
            &["session", "context", "--clear", "--target", "s1"],
            serde_json::json!({"cmd": "session.context", "target": "s1", "args": {"mode": "clear"}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n",
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
fn type_takes_text_after_a_separator_or_from_stdin() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let expected = serde_json::json!({"cmd": "session.type", "target": "s1",
                                      "args": {"text": "printf 'a%s\\n' b\n", "select": false}});
    let _server = scripted(&socket, expected, serde_json::json!({"id": NEW_ID}))?;
    let output = run_with_input(
        dir.path(),
        &socket,
        &["session", "type", "--stdin", "--target", "s1"],
        b"printf 'a%s\\n' b\n",
    )?;
    assert_eq!(outcome(&output), (Some(0), "ok\n".to_owned(), String::new()));

    let dashed = dir.path().join("dash.sock");
    let expected = serde_json::json!({"cmd": "session.type", "target": "s1",
                                      "args": {"text": "--dash", "select": false}});
    let _server = scripted(&dashed, expected, serde_json::json!({"id": NEW_ID}))?;
    let args = ["session", "type", "--target", "s1", "--socket"]
        .map(OsStr::new)
        .into_iter()
        .chain([dashed.as_os_str(), OsStr::new("--"), OsStr::new("--dash")]);
    assert_eq!(
        outcome(&run(dir.path(), &[], args)?),
        (Some(0), "ok\n".to_owned(), String::new())
    );
    Ok(())
}

#[test]
fn type_without_a_payload_or_with_invalid_utf8_is_a_usage_error_at_run_time() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let refusal = |message: &str| format!("Error: {message}\n{ROOT_USAGE}");
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["session", "type"])?),
        (Some(64), String::new(), refusal("provide TEXT or --stdin"))
    );
    let invalid = run_with_input(dir.path(), &socket, &["session", "type", "--stdin"], &[b'a', 0xFF])?;
    assert_eq!(
        outcome(&invalid),
        (Some(64), String::new(), refusal("stdin must be valid UTF-8"))
    );
    Ok(())
}

#[test]
fn io_and_metadata_refusals_are_usage_errors_before_connecting() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let common = "[--target <target>] [--socket <socket>] [--json] [--window <window>]";
    let usage = |command: &str, synopsis: &str| {
        format!(
            "Usage: agtermctl session {command} {synopsis}{common}\n  See 'agtermctl session {command} --help' for \
             more information.\n"
        )
    };
    let text = usage(
        "text",
        "[--all] [--lines <lines>] [--pane <pane>] [--pane-id <pane-id>] ",
    );
    let status = usage(
        "status",
        "<state> [--blink] [--auto-reset] [--sound <sound>] [--color <color>] [--shape <shape>] [--pane <pane>] \
         [--pane-id <pane-id>] ",
    );
    let cases: [(&[&str], &str, String); 7] = [
        (
            &["session", "text", "--all", "--lines", "2"],
            "use either --all or --lines, not both",
            text.clone(),
        ),
        (
            &["session", "text", "--lines", "0"],
            "--lines must be greater than 0",
            text,
        ),
        (
            &["session", "paste", "--pane", "middle"],
            "--pane must be left, right, or scratch",
            usage("paste", "[--pane <pane>] "),
        ),
        (
            &["session", "status", "blocked", "--color", "nope"],
            "color must be a #rrggbb hex value",
            status,
        ),
        (
            &["session", "flag", "bogus"],
            "mode must be on, off, toggle, or clear",
            usage("flag", "[<mode>] "),
        ),
        (
            &["session", "context"],
            "provide exactly one of a TEXT or --clear",
            usage("context", "[<text>] [--clear] "),
        ),
        (
            &["session", "search", "--next", "--prev"],
            "--next, --prev, and --close are mutually exclusive",
            usage("search", "[<needle>] [--next] [--prev] [--close] "),
        ),
    ];
    for (args, message, usage) in cases {
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(64), String::new(), format!("Error: {message}\n{usage}")),
            "{args:?}"
        );
    }
    Ok(())
}

const SURFACE_HELP: &str = "\
OVERVIEW: Terminal surface commands.

USAGE: agtermctl surface <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  zoom                    Zoom a terminal surface (show|hide|toggle).
  cursor                  Report a terminal surface's zero-based cursor column.

  See 'agtermctl help surface <subcommand>' for detailed help.
";

const SURFACE_CURSOR_HELP: &str = "\
OVERVIEW: Report a terminal surface's zero-based cursor column.

Prints the column alone, so it drops straight into a command substitution. Row
is not reported: the pinned libghostty exposes no cursor accessor and the
vertical metrics it does export cannot recover a row that survives a custom
`adjust-font-baseline`.

A column is a signal, not proof about the line's content. Past the prompt it
establishes the line is not empty; AT the prompt it establishes nothing, since
the caret may have been moved back over text that is still there.

USAGE: agtermctl surface cursor [--pane-id <pane-id>] [--target <target>] [--socket <socket>] [--json] [--window <window>]

OPTIONS:
  --pane-id <pane-id>     Stable pane token ($AGTERM_PANE_ID). With it --target
                          names the session ('active' or a session id), not a
                          surface.
  --target <target>       Target surface id from tree, 'quick' (the quick
                          terminal), or 'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const SESSION_TYPE_HELP: &str = "\
OVERVIEW: Inject text into a session.

USAGE: agtermctl session type [<text>] [--stdin] [--select] [--pane <pane>] [--target <target>] [--socket <socket>] [--json] [--window <window>] [--pane-id <pane-id>]

ARGUMENTS:
  <text>                  Text to inject (omit with --stdin).

OPTIONS:
  --stdin                 Read the text from stdin instead of an argument.
  --select                Select the session first if its surface is not ready
                          (main pane only; a split pane must already exist).
  --pane <pane>           Which pane to type into: primary/left/top,
                          split/right/bottom, or scratch (even when hidden).
                          Defaults to primary.
  --target <target>       Target session/workspace id, unique prefix, or
                          'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  --pane-id <pane-id>     Stable pane token ($AGTERM_PANE_ID); overrides
                          --pane, and unknown without --pane is an error.
  -h, --help              Show help information.

";

const SESSION_FLAG_HELP: &str = "\
OVERVIEW: Flag a session for the flagged working-set view (on|off|toggle|clear).

USAGE: agtermctl session flag [<mode>] [--target <target>] [--socket <socket>] [--json] [--window <window>]

ARGUMENTS:
  <mode>                  Mode: on, off, toggle (default), or clear (unflag
                          all; ignores --target). (default: toggle)

OPTIONS:
  --target <target>       Target session/workspace id, unique prefix, or
                          'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

#[test]
fn io_metadata_and_surface_help_in_the_argument_parser_layout() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str); 5] = [
        (&["surface"], SURFACE_HELP),
        (&["help", "surface"], SURFACE_HELP),
        (&["surface", "cursor", "--help"], SURFACE_CURSOR_HELP),
        (&["help", "session", "type"], SESSION_TYPE_HELP),
        (&["session", "flag", "--help"], SESSION_FLAG_HELP),
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

// MARK: - split, scratch and pane commands

#[test]
fn pane_commands_send_their_request_and_print_ok() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], serde_json::Value); 8] = [
        (
            &["session", "split"],
            serde_json::json!({"cmd": "session.split", "target": "active", "args": {"mode": "toggle"}}),
        ),
        (
            &[
                "session",
                "split",
                "on",
                "--axis",
                "horizontal",
                "--target",
                "s1",
                "--window",
                "w1",
            ],
            serde_json::json!({"cmd": "session.split", "target": "s1",
                               "args": {"mode": "on", "axis": "horizontal", "window": "w1"}}),
        ),
        (
            &["session", "split", "close", "--target", "s1"],
            serde_json::json!({"cmd": "session.split.close", "target": "s1"}),
        ),
        (
            &["session", "swap", "--window", "w1"],
            serde_json::json!({"cmd": "session.swap", "target": "active", "args": {"window": "w1"}}),
        ),
        (
            &["session", "lead", "--pane", "right"],
            serde_json::json!({"cmd": "session.lead", "target": "active", "args": {"pane": "right"}}),
        ),
        (
            &["session", "scratch", "on", "--command", "htop"],
            serde_json::json!({"cmd": "session.scratch", "target": "active",
                               "args": {"mode": "on", "command": "htop"}}),
        ),
        (
            &["session", "focus"],
            serde_json::json!({"cmd": "session.focus", "target": "active", "args": {"pane": "other"}}),
        ),
        (
            &["session", "focus", "bottom", "--target", "s1"],
            serde_json::json!({"cmd": "session.focus", "target": "s1", "args": {"pane": "bottom"}}),
        ),
    ];
    for (at, (args, expected)) in cases.into_iter().enumerate() {
        let socket = dir.path().join(format!("{at}.sock"));
        let _server = scripted(&socket, expected, serde_json::json!({"id": NEW_ID}))?;
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(0), "ok\n".to_owned(), String::new()),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn resize_prints_the_applied_ratio_and_json_the_server_line() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], serde_json::Value); 2] = [
        (
            &["session", "resize", "--split-ratio", "0.01", "--target", "s1"],
            serde_json::json!({"cmd": "session.resize", "target": "s1", "args": {"ratio": 0.01}}),
        ),
        (
            &["session", "resize", "--grow-bottom", "0.2", "--window", "w1"],
            serde_json::json!({"cmd": "session.resize", "target": "active",
                               "args": {"ratioDelta": -0.2, "window": "w1"}}),
        ),
    ];
    for (at, (args, expected)) in cases.into_iter().enumerate() {
        let socket = dir.path().join(format!("{at}.sock"));
        let _server = scripted(&socket, expected, serde_json::json!({"id": NEW_ID, "ratio": 0.3}))?;
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(0), "0.300\n".to_owned(), String::new()),
            "{args:?}"
        );
        let json = run_on(dir.path(), &socket, &[args, &["--json"]].concat())?;
        let line = format!("{{\"ok\":true,\"result\":{{\"id\":\"{NEW_ID}\",\"ratio\":0.3}}}}\n");
        assert_eq!(outcome(&json), (Some(0), line, String::new()), "{args:?}");
    }
    Ok(())
}

const SPLIT_VISIBILITY_USAGE: &str = "Usage: agtermctl session split visibility [<mode>] [--axis <axis>] [--target \
                                      <target>] [--socket <socket>] [--json] [--window <window>]\n  See 'agtermctl \
                                      session split visibility --help' for more information.\n";
const RESIZE_USAGE: &str = "Usage: agtermctl session resize [--split-ratio <split-ratio>] [--grow-left <grow-left>] \
                            [--grow-right <grow-right>] [--grow-primary <grow-primary>] [--grow-split <grow-split>] \
                            [--grow-top <grow-top>] [--grow-bottom <grow-bottom>] [--target <target>] [--socket \
                            <socket>] [--json] [--window <window>]\n  See 'agtermctl session resize --help' for more \
                            information.\n";
const LEAD_USAGE: &str = "Usage: agtermctl session lead [--pane <pane>] [--target <target>] [--socket <socket>] \
                          [--json] [--window <window>]\n  See 'agtermctl session lead --help' for more information.\n";

#[test]
fn pane_command_refusals_are_usage_errors_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let cases: [(&[&str], &str, &str); 7] = [
        (
            &["session", "split", "--axis", "diagonal"],
            "--axis must be vertical or horizontal",
            SPLIT_VISIBILITY_USAGE,
        ),
        (
            &["session", "split", "visibility", "off", "--axis", "Vertical"],
            "--axis must be vertical or horizontal",
            SPLIT_VISIBILITY_USAGE,
        ),
        (
            &["session", "resize"],
            "provide exactly one split ratio or grow option",
            RESIZE_USAGE,
        ),
        (
            &["session", "resize", "--grow-left", "0.1", "--grow-right", "0.1"],
            "provide exactly one split ratio or grow option",
            RESIZE_USAGE,
        ),
        (
            &["session", "resize", "--split-ratio", "nan"],
            "the resize value must be a finite number",
            RESIZE_USAGE,
        ),
        (
            &["session", "resize", "--grow-top", "1e999"],
            "the resize value must be a finite number",
            RESIZE_USAGE,
        ),
        (
            &["session", "lead", "--pane", "other"],
            "--pane must be left, right, or scratch",
            LEAD_USAGE,
        ),
    ];
    for (args, message, usage) in cases {
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(64), String::new(), format!("Error: {message}\n{usage}")),
            "{args:?}"
        );
    }
    let close_usage = "Usage: agtermctl session split close [--target <target>] [--socket <socket>] [--json] \
                       [--window <window>]\n  See 'agtermctl session split close --help' for more information.\n";
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["session", "split", "close", "extra"])?),
        (
            Some(64),
            String::new(),
            format!("Error: Unexpected argument 'extra'\n{close_usage}")
        )
    );
    Ok(())
}

const SESSION_SPLIT_HELP: &str = "\
OVERVIEW: Show, hide, or close a session split.

USAGE: agtermctl session split <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  visibility (default)    Show or hide a session split (on|off|toggle).
  close                   Close the split pane (destroys it, killing whatever
                          it runs); ok when there is none.

  See 'agtermctl help session split <subcommand>' for detailed help.
";

const SESSION_SPLIT_VISIBILITY_HELP: &str = "\
OVERVIEW: Show or hide a session split (on|off|toggle).

USAGE: agtermctl session split visibility [<mode>] [--axis <axis>] [--target <target>] [--socket <socket>] [--json] [--window <window>]

ARGUMENTS:
  <mode>                  Mode: on (show), off (hide), or toggle (default).
                          Hidden panes stay alive. (default: toggle)

OPTIONS:
  --axis <axis>           Divider direction: vertical (left/right) or
                          horizontal (top/bottom).
  --target <target>       Target session/workspace id, unique prefix, or
                          'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const SESSION_LEAD_HELP: &str = "\
OVERVIEW: Take the lead of a pane's terminal for this Mac.

A session attached from another Mac has one leading client per pane, whose
window size the program sees. A pane that does not lead is covered; this
uncovers it, as pressing a key on the cover does, and covers it on the other
Mac. Read each pane's `lead` from `tree`.

USAGE: agtermctl session lead [--pane <pane>] [--target <target>] [--socket <socket>] [--json] [--window <window>]

OPTIONS:
  --pane <pane>           Which pane: primary/left/top or split/right/bottom.
                          Defaults to primary.
  --target <target>       Target session/workspace id, unique prefix, or
                          'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

#[test]
fn split_help_marks_its_default_subcommand() -> TestResult {
    let dir = short_dir()?;
    // a lone help flag shows the group; beside anything else it falls through to the default subcommand
    let cases: [(&[&str], &str); 6] = [
        (&["session", "split", "--help"], SESSION_SPLIT_HELP),
        (&["session", "split", "-h"], SESSION_SPLIT_HELP),
        (&["help", "session", "split"], SESSION_SPLIT_HELP),
        (&["session", "split", "on", "--help"], SESSION_SPLIT_VISIBILITY_HELP),
        (
            &["help", "session", "split", "visibility"],
            SESSION_SPLIT_VISIBILITY_HELP,
        ),
        (&["session", "lead", "--help"], SESSION_LEAD_HELP),
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

// MARK: - quick

const QUICK_HELP: &str = "\
OVERVIEW: Quick terminal: visibility, type into it, read its text.

USAGE: agtermctl quick <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  visibility (default)    Quick terminal visibility (show|hide|toggle).
  type                    Inject text into the quick terminal.
  text                    Print the quick terminal's buffer as plain text.

  See 'agtermctl help quick <subcommand>' for detailed help.
";

const QUICK_VISIBILITY_HELP: &str = "\
OVERVIEW: Quick terminal visibility (show|hide|toggle).

USAGE: agtermctl quick visibility [<mode>] [--socket <socket>] [--json]

ARGUMENTS:
  <mode>                  Mode: show, hide, or toggle (default). (default:
                          toggle)

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

const QUICK_TYPE_HELP: &str = "\
OVERVIEW: Inject text into the quick terminal.

USAGE: agtermctl quick type [<text>] [--stdin] [--socket <socket>] [--json]

ARGUMENTS:
  <text>                  Text to inject (omit with --stdin).

OPTIONS:
  --stdin                 Read the text from stdin instead of an argument.
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

const QUICK_TEXT_HELP: &str = "\
OVERVIEW: Print the quick terminal's buffer as plain text.

USAGE: agtermctl quick text [--all] [--lines <lines>] [--socket <socket>] [--json]

OPTIONS:
  --all                   Read the full screen + scrollback instead of just the
                          visible screen.
  --lines <lines>         Keep only the last N lines of the full buffer.
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

#[test]
fn quick_help_marks_its_default_subcommand() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str); 6] = [
        (&["quick", "--help"], QUICK_HELP),
        (&["help", "quick"], QUICK_HELP),
        (&["quick", "show", "--help"], QUICK_VISIBILITY_HELP),
        (&["help", "quick", "visibility"], QUICK_VISIBILITY_HELP),
        (&["quick", "type", "--help"], QUICK_TYPE_HELP),
        (&["quick", "text", "--help"], QUICK_TEXT_HELP),
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
fn quick_commands_send_their_request_and_print_the_reply() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], serde_json::Value, serde_json::Value, &str); 4] = [
        (
            &["quick"],
            serde_json::json!({"cmd": "quick", "args": {"mode": "toggle"}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["quick", "show"],
            serde_json::json!({"cmd": "quick", "args": {"mode": "show"}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["quick", "text", "--lines", "2"],
            serde_json::json!({"cmd": "quick.text", "args": {"lines": 2}}),
            serde_json::json!({"text": "$ echo hi\nhi"}),
            "$ echo hi\nhi\n",
        ),
        (
            &["quick", "type", "ls"],
            serde_json::json!({"cmd": "quick.type", "args": {"text": "ls"}}),
            serde_json::json!({}),
            "ok\n",
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
    let socket = dir.path().join("stdin.sock");
    let expected = serde_json::json!({"cmd": "quick.type", "args": {"text": "printf 'a%s\\n' b\n"}});
    let _server = scripted(&socket, expected, serde_json::json!({}))?;
    let output = run_with_input(
        dir.path(),
        &socket,
        &["quick", "type", "--stdin"],
        b"printf 'a%s\\n' b\n",
    )?;
    assert_eq!(outcome(&output), (Some(0), "ok\n".to_owned(), String::new()));
    Ok(())
}

#[test]
fn quick_refusals_are_usage_errors_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let text = "Usage: agtermctl quick text [--all] [--lines <lines>] [--socket <socket>] [--json]\n  See 'agtermctl \
                quick text --help' for more information.\n";
    let cases: [(&[&str], &str); 2] = [
        (
            &["quick", "text", "--all", "--lines", "1"],
            "use either --all or --lines, not both",
        ),
        (&["quick", "text", "--lines", "0"], "--lines must be greater than 0"),
    ];
    for (args, message) in cases {
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(64), String::new(), format!("Error: {message}\n{text}")),
            "{args:?}"
        );
    }
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["quick", "type"])?),
        (
            Some(64),
            String::new(),
            format!("Error: provide TEXT or --stdin\n{ROOT_USAGE}")
        )
    );
    Ok(())
}

// MARK: - dashboard

const DASHBOARD_USAGE: &str = "Usage: agtermctl dashboard [<ids> ...] [--font-size <font-size>] [--auto-size] [--mru] \
                               [--close] [--socket <socket>] [--json] [--window <window>]\n  See 'agtermctl \
                               dashboard --help' for more information.\n";

#[test]
fn dashboard_sends_its_ids_verbatim_and_prints_ok_or_the_note() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], serde_json::Value, serde_json::Value, &str); 7] = [
        (
            &["dashboard", "s1", "s2", "--font-size", "12"],
            serde_json::json!({"cmd": "dashboard", "args": {"targets": ["s1", "s2"], "fontSize": 12.0}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["dashboard", "s1", "s2", "--auto-size"],
            serde_json::json!({"cmd": "dashboard", "args": {"targets": ["s1", "s2"], "autoSize": true}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["dashboard", "s1", "--window", "win"],
            serde_json::json!({"cmd": "dashboard", "args": {"targets": ["s1"], "window": "win"}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["dashboard", "--close"],
            serde_json::json!({"cmd": "dashboard", "args": {"close": true}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            // the pane grammar is the server's to check
            &["dashboard", "s1:left", "s2:right", "s3", "active:left", "s4:nope"],
            serde_json::json!({"cmd": "dashboard",
                               "args": {"targets": ["s1:left", "s2:right", "s3", "active:left", "s4:nope"]}}),
            serde_json::json!({"text": "unresolved: s4:nope"}),
            "unresolved: s4:nope\n",
        ),
        (
            &["dashboard", "--mru"],
            serde_json::json!({"cmd": "dashboard", "args": {"mru": true}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["dashboard", "--mru", "--auto-size", "--window", "win"],
            serde_json::json!({"cmd": "dashboard", "args": {"window": "win", "autoSize": true, "mru": true}}),
            serde_json::json!({}),
            "ok\n",
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
fn dashboard_refusals_are_usage_errors_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let cases: [(&[&str], &str); 8] = [
        (
            &["dashboard"],
            "dashboard requires at least one session id (or --mru, or --close)",
        ),
        (
            &["dashboard", "s1:left", "--mru"],
            "--mru cannot be combined with session ids",
        ),
        (
            &["dashboard", "s1", "--font-size", "12", "--auto-size"],
            "--font-size is mutually exclusive with --auto-size",
        ),
        (
            &["dashboard", "s1", "--font-size", "0"],
            "--font-size must be a positive number",
        ),
        (
            &["dashboard", "s1", "--font-size", "nan"],
            "--font-size must be a positive number",
        ),
        (
            &["dashboard", "s1", "--close"],
            "--close takes no ids, --mru, or font options",
        ),
        (
            &["dashboard", "--close", "--auto-size"],
            "--close takes no ids, --mru, or font options",
        ),
        (
            &["dashboard", "--close", "--mru"],
            "--close takes no ids, --mru, or font options",
        ),
    ];
    for (args, message) in cases {
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(64), String::new(), format!("Error: {message}\n{DASHBOARD_USAGE}")),
            "{args:?}"
        );
    }
    Ok(())
}

// MARK: - hud and notify

const SESSION_HUD_HELP: &str = "\
OVERVIEW: Post, update, or close a passive message panel over a session.

USAGE: agtermctl session hud <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  open (default)          Post a message panel over the session; the session
                          keeps focus and stays typable.
  update                  Replace the panel's text in place (no re-spawn, no
                          blink).
  close                   Take the message panel down (a program overlay in the
                          same slot is left alone).

  See 'agtermctl help session hud <subcommand>' for detailed help.
";

const SESSION_HUD_OPEN_HELP: &str = "\
OVERVIEW: Post a message panel over the session; the session keeps focus and
stays typable.

USAGE: agtermctl session hud open [<options>] [<message>]

ARGUMENTS:
  <message>               Message shown in the panel (omit with --file).

OPTIONS:
  --file <file>           Read the message from FILE instead of the argument.
  --markdown              Render the message as markdown, up to 4096
                          characters. A single newline inside a paragraph is a
                          soft break; end a line with two spaces or a backslash
                          to break it. A link is underlined and opens on a
                          command-click.
  --font-size <font-size> The panel's own font size in points, 6-72; omit to
                          use the session's. Fixed for the panel's life.
  --detail <detail>       Dim second line under the message (e.g. what the
                          caller is waiting on).
  --spinner               Animate a spinner glyph in the panel, in the default
                          style.
  --spinner-style <spinner-style>
                          Spinner style: bar, braille, circle, blocks, dot,
                          none (default: bar). Implies --spinner; none leaves
                          the panel static.
  --position <position>   Placement in the pane: top-left, top-center,
                          top-right, center-left, center, center-right,
                          bottom-left, bottom-center, bottom-right (default:
                          center), the same anchors session background takes.
                          Every anchor off center holds a fixed margin at that
                          edge. Here top and bottom are also accepted, for
                          top-center and bottom-center.
  --background-color <background-color>
                          Solid background color (#rrggbb) for the panel,
                          independent of the session's own.
  --text-color <text-color>
                          Color (#rrggbb) for the panel's text; omit to keep
                          the terminal foreground.
  --size-percent <size-percent>
                          Set the panel's WIDTH to PERCENT (1-100) of the pane
                          instead of measuring the message; bounded to 10-80,
                          or up to 100 with --sticky off center. Height always
                          follows the message.
  --sticky                Put the panel flush against the edge or corner
                          --position names, with no margin; center has no edge
                          and ignores it. Add --size-percent 100 for a strip
                          across the pane.
  --no-frame              Draw no border, no rounded corners and no blank row
                          above and below the text.
  --pane <pane>           Anchor inside primary/left/top or split/right/bottom;
                          omit for the whole session.
  --pane-id <pane-id>     Stable pane token ($AGTERM_PANE_ID); overrides --pane
                          when it resolves.
  --hide-after <hide-after>
                          Take the panel down by itself after SECONDS,
                          0...86400; omit or 0 to leave it up until something
                          closes it. The clock runs whether or not the session
                          is on screen.
  --target <target>       Target session/workspace id, unique prefix, or
                          'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

const NOTIFY_USAGE: &str = "Usage: agtermctl notify <body> [--title <title>] [--target <target>] [--socket <socket>] \
                            [--json] [--window <window>]\n  See 'agtermctl notify --help' for more information.\n";

const NOTIFY_HELP: &str = "\
OVERVIEW: Post a desktop notification (default: the active session of the
frontmost window).

USAGE: agtermctl notify <body> [--title <title>] [--target <target>] [--socket <socket>] [--json] [--window <window>]

ARGUMENTS:
  <body>                  Notification body.

OPTIONS:
  --title <title>         Notification title (defaults to the session name).
  --target <target>       Target session/workspace id, unique prefix, or
                          'active'. (default: active)
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

#[test]
fn hud_and_notify_help_in_the_argument_parser_layout() -> TestResult {
    let dir = short_dir()?;
    // a lone help flag shows the group; beside anything else it falls through to `open`
    let cases: [(&[&str], &str); 6] = [
        (&["session", "hud", "--help"], SESSION_HUD_HELP),
        (&["help", "session", "hud"], SESSION_HUD_HELP),
        (&["session", "hud", "open", "--help"], SESSION_HUD_OPEN_HELP),
        (&["session", "hud", "a message", "--help"], SESSION_HUD_OPEN_HELP),
        (&["notify", "--help"], NOTIFY_HELP),
        (&["help", "notify"], NOTIFY_HELP),
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
fn hud_and_notify_send_their_request_and_print_ok_or_the_note() -> TestResult {
    let dir = short_dir()?;
    let banners_off = "badge updated, but \"Show notification banners\" is off, so no banner was posted";
    let cases: [(&[&str], serde_json::Value, serde_json::Value, String); 6] = [
        (
            &[
                "session",
                "hud",
                "first",
                "--detail",
                "waiting",
                "--position",
                "top",
                "--target",
                "s1",
            ],
            serde_json::json!({"cmd": "session.hud.open", "target": "s1",
                               "args": {"message": "first", "detail": "waiting", "position": "top"}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n".to_owned(),
        ),
        (
            // a message named like a subcommand takes the explicit verb
            &[
                "session",
                "hud",
                "open",
                "update",
                "--spinner-style",
                "dot",
                "--no-frame",
            ],
            serde_json::json!({"cmd": "session.hud.open", "target": "active",
                               "args": {"message": "update", "spinner": "dot", "frame": false}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n".to_owned(),
        ),
        (
            &["session", "hud", "update", "second", "--target", "s1", "--window", "w1"],
            serde_json::json!({"cmd": "session.hud.update", "target": "s1",
                               "args": {"message": "second", "window": "w1"}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n".to_owned(),
        ),
        (
            &["session", "hud", "close", "--target", "s1"],
            serde_json::json!({"cmd": "session.hud.close", "target": "s1"}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n".to_owned(),
        ),
        (
            &["notify", "cli body", "--title", "cli title", "--target", "s1"],
            serde_json::json!({"cmd": "notify", "target": "s1",
                               "args": {"title": "cli title", "body": "cli body"}}),
            serde_json::json!({"id": NEW_ID}),
            "ok\n".to_owned(),
        ),
        (
            // banners off: the badge still rose, and the note says no banner was posted
            &["notify", "done"],
            serde_json::json!({"cmd": "notify", "target": "active", "args": {"body": "done"}}),
            serde_json::json!({"id": NEW_ID, "text": banners_off}),
            format!("{banners_off}\n"),
        ),
    ];
    for (at, (args, expected, result, stdout)) in cases.into_iter().enumerate() {
        let socket = dir.path().join(format!("{at}.sock"));
        let _server = scripted(&socket, expected, result)?;
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(0), stdout, String::new()),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn a_hud_message_read_from_a_file_is_sent_without_its_terminator() -> TestResult {
    let dir = short_dir()?;
    let file = dir.path().join("hud.md");
    fs::write(&file, "# Tasks\r\n\r\n- build\r\n")?;
    let socket = dir.path().join("agterm.sock");
    let expected = serde_json::json!({"cmd": "session.hud.update", "target": "active",
                                      "args": {"message": "# Tasks\n\n- build", "markdown": true}});
    let _server = scripted(&socket, expected, serde_json::json!({"id": NEW_ID}))?;
    let file = file.to_str().ok_or("temp path is not UTF-8")?;
    let args = ["session", "hud", "update", "--file", file, "--markdown"];
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &args)?),
        (Some(0), "ok\n".to_owned(), String::new())
    );
    Ok(())
}

#[test]
fn hud_close_json_prints_the_server_line() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("json.sock");
    let expected = serde_json::json!({"cmd": "session.hud.close", "target": "s1"});
    let _server = scripted(&socket, expected, serde_json::json!({"id": NEW_ID}))?;
    let json = run_on(
        dir.path(),
        &socket,
        &["session", "hud", "close", "--target", "s1", "--json"],
    )?;
    let line = format!("{{\"ok\":true,\"result\":{{\"id\":\"{NEW_ID}\"}}}}\n");
    assert_eq!(outcome(&json), (Some(0), line, String::new()));
    Ok(())
}

#[test]
fn a_hud_update_without_a_panel_exits_1_with_the_server_error() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let _server = serve(&socket, (), |(), _| Response::failure("no hud").into())?;
    assert_eq!(
        outcome(&run_on(
            dir.path(),
            &socket,
            &["session", "hud", "update", "third", "--target", "s1"]
        )?),
        (Some(1), String::new(), "error: no hud\n".to_owned())
    );
    Ok(())
}

#[test]
fn hud_and_notify_refusals_are_usage_errors_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let open = "Usage: agtermctl session hud open [<options>] [<message>]\n  See 'agtermctl session hud open --help' for \
                more information.\n";
    let update = "Usage: agtermctl session hud update [<options>] [<message>]\n  See 'agtermctl session hud update \
                  --help' for more information.\n";
    let cases: [(&[&str], &str, &str); 7] = [
        (
            &[
                "session",
                "hud",
                "open",
                "--spinner-style",
                "wheel",
                "--target",
                "s1",
                "x",
            ],
            "spinner style must be one of: bar, braille, circle, blocks, dot, none",
            open,
        ),
        (&["session", "hud"], "provide MESSAGE or --file", open),
        (
            &["session", "hud", "x", "--pane", "scratch"],
            "--pane must be left or right",
            open,
        ),
        (
            &["session", "hud", "x", "--font-size", "80"],
            "font-size must be 6...72 points",
            open,
        ),
        (&["session", "hud", "update"], "provide MESSAGE or --file", update),
        (
            &["session", "hud", "update", "x", "--position", "middle"],
            "position must be one of: top-left, top-center, top-right, center-left, center, center-right, \
             bottom-left, bottom-center, bottom-right, top, bottom",
            update,
        ),
        (
            &["session", "hud", "update", "x", "--hide-after", "90000"],
            "hide-after must be 0...86400 seconds",
            update,
        ),
    ];
    for (args, message, usage) in cases {
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(64), String::new(), format!("Error: {message}\n{usage}")),
            "{args:?}"
        );
    }
    // update has no creation-only option
    let (code, _, stderr) = outcome(&run_on(
        dir.path(),
        &socket,
        &["session", "hud", "update", "x", "--background-color", "#112233"],
    )?);
    assert_eq!(code, Some(64));
    assert!(
        stderr.starts_with("Error: Unknown option '--background-color'\n"),
        "{stderr}"
    );
    // an unreadable file is refused when the run reads it, so the usage line is the root's
    let missing = dir.path().join("missing.md");
    let missing = missing.to_str().ok_or("temp path is not UTF-8")?;
    let (code, stdout, stderr) = outcome(&run_on(dir.path(), &socket, &["session", "hud", "--file", missing])?);
    assert_eq!((code, stdout.as_str()), (Some(64), ""));
    assert!(
        stderr.starts_with(&format!("Error: cannot read --file {missing}: ")) && stderr.ends_with(ROOT_USAGE),
        "{stderr}"
    );
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["notify", "--target", "s1"])?),
        (
            Some(64),
            String::new(),
            format!("Error: Missing expected argument '<body>'\nHelp:  <body>  Notification body.\n{NOTIFY_USAGE}")
        )
    );
    Ok(())
}

// MARK: - pick and ask

const MODAL_ID: &str = "7D3C0A51-2B6E-4F0A-9C1D-3E4F5A6B7C8D";

/// A modal host for `family` (`pick` or `ask`): `open` must equal `expected_open`; `result` reports the outcomes in
/// `results` in turn (the last one repeating); `cancel` of the open id answers ok; an unknown id is refused.
fn modal_host(
    socket: &Path,
    family: &'static str,
    expected_open: serde_json::Value,
    results: Vec<serde_json::Value>,
) -> TestResult<Server<usize>> {
    serve(socket, 0, move |polls: &mut usize, request| {
        let sent = serde_json::to_value(&request).unwrap_or_default();
        let cmd = sent["cmd"].as_str().unwrap_or_default().to_owned();
        let target = sent["target"].as_str().unwrap_or_default().to_owned();
        if cmd == format!("{family}.open") {
            return if sent == expected_open {
                Response::with_result(serde_json::json!({"id": MODAL_ID})).into()
            } else {
                Response::failure(format!("unexpected request: {sent}")).into()
            };
        }
        if target != MODAL_ID {
            return Response::failure(format!("unknown {family}: {target}")).into();
        }
        if cmd == format!("{family}.cancel") {
            return Response::success().into();
        }
        let outcome = results[(*polls).min(results.len() - 1)].clone();
        *polls += 1;
        Response::with_result(serde_json::json!({ family: outcome })).into()
    })
}

#[test]
fn pick_without_blocking_prints_the_id_and_result_reads_each_outcome() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let open = serde_json::json!({"cmd": "pick.open", "args": {
        "items": [{"id": "a=Alpha", "label": "a=Alpha"}, {"id": "b=Beta", "label": "b=Beta"}], "prompt": "choose"}});
    let results = vec![
        serde_json::json!({"result": "pending"}),
        serde_json::json!({"result": "cancelled"}),
    ];
    let _server = modal_host(&socket, "pick", open, results)?;
    let opened = run_with_input(
        dir.path(),
        &socket,
        &["pick", "--no-block", "--prompt", "choose"],
        b"a=Alpha\nb=Beta\n",
    )?;
    let id_line = format!("{{\"id\":\"{MODAL_ID}\"}}\n");
    assert_eq!(outcome(&opened), (Some(0), id_line, String::new()));
    let pending = run_on(dir.path(), &socket, &["pick", "result", MODAL_ID])?;
    assert_eq!(
        outcome(&pending),
        (Some(1), "{\"result\":\"pending\"}\n".to_owned(), String::new())
    );
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["pick", "cancel", MODAL_ID])?),
        (Some(0), "ok\n".to_owned(), String::new())
    );
    let cancelled = run_on(dir.path(), &socket, &["pick", "result", MODAL_ID])?;
    assert_eq!(
        outcome(&cancelled),
        (Some(2), "{\"result\":\"cancelled\"}\n".to_owned(), String::new())
    );
    for verb in ["result", "cancel"] {
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, &["pick", verb, "zzz"])?),
            (Some(1), String::new(), "error: unknown pick: zzz\n".to_owned()),
            "{verb}"
        );
    }
    let refused_json = run_on(dir.path(), &socket, &["pick", "result", "zzz", "--json"])?;
    assert_eq!(
        outcome(&refused_json),
        (
            Some(1),
            "{\"ok\":false,\"error\":\"unknown pick: zzz\"}\n".to_owned(),
            String::new()
        )
    );
    Ok(())
}

#[test]
fn a_blocking_pick_polls_until_it_resolves() -> TestResult {
    let dir = short_dir()?;
    let cases = [
        (serde_json::json!({"result": "cancelled"}), 2),
        (
            serde_json::json!({"result": "picked", "id": "b", "label": "Beta", "index": 1}),
            0,
        ),
    ];
    for (index, (result, code)) in cases.into_iter().enumerate() {
        let socket = dir.path().join(format!("pick{index}.sock"));
        let open = serde_json::json!({"cmd": "pick.open", "args": {
            "items": [{"id": "a", "label": "Alpha"}, {"id": "b", "label": "Beta", "subtitle": "second"}]}});
        let results = vec![serde_json::json!({"result": "pending"}), result.clone()];
        let _server = modal_host(&socket, "pick", open, results)?;
        let input = br#"[{"id":"a","label":"Alpha"},{"id":"b","label":"Beta","subtitle":"second"}]"#;
        let (status, stdout, stderr) = outcome(&run_with_input(dir.path(), &socket, &["pick"], input)?);
        assert_eq!((status, stderr), (Some(code), String::new()), "{result}");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&stdout)?, result);
        assert_eq!(stdout.matches('\n').count(), 1, "{stdout}");
    }
    Ok(())
}

#[test]
fn pick_refuses_stdin_that_is_neither_text_nor_an_item_array() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a sent request would fail to connect instead
    let socket = dir.path().join("agterm.sock");
    let (code, stdout, stderr) = outcome(&run_with_input(dir.path(), &socket, &["pick"], &[0xFF, b'\n'])?);
    assert_eq!((code, stdout.as_str()), (Some(64), ""));
    assert!(
        stderr.starts_with("Error: stdin must be UTF-8 text or a JSON item array\n") && stderr.ends_with(ROOT_USAGE),
        "{stderr}"
    );
    let (code, stdout, stderr) = outcome(&run_with_input(dir.path(), &socket, &["pick"], b"[{\"id\":")?);
    assert_eq!((code, stdout.as_str()), (Some(1), ""));
    assert!(
        stderr.starts_with("Error: stdin is not a valid JSON item array: "),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn ask_without_blocking_prints_the_id_and_result_reads_each_outcome() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let open = serde_json::json!({"cmd": "ask.open", "target": "s1", "args": {
        "buttons": [{"id": "yes", "label": "Yes"}, {"id": "no", "label": "no"}], "style": "terminal",
        "align": "right", "title": "Proceed?"}});
    let results = vec![
        serde_json::json!({"result": "pending"}),
        serde_json::json!({"result": "escaped"}),
    ];
    let _server = modal_host(&socket, "ask", open, results)?;
    let args = [
        "ask",
        "--no-block",
        "--button",
        "yes=Yes",
        "--button",
        "no",
        "--target",
        "s1",
        "Proceed?",
    ];
    let id_line = format!("{{\"id\":\"{MODAL_ID}\"}}\n");
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &args)?),
        (Some(0), id_line, String::new())
    );
    let pending = run_on(dir.path(), &socket, &["ask", "result", MODAL_ID])?;
    assert_eq!(
        outcome(&pending),
        (Some(1), "{\"result\":\"pending\"}\n".to_owned(), String::new())
    );
    let escaped = run_on(dir.path(), &socket, &["ask", "result", MODAL_ID])?;
    assert_eq!(
        outcome(&escaped),
        (Some(3), "{\"result\":\"escaped\"}\n".to_owned(), String::new())
    );
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["ask", "cancel", MODAL_ID])?),
        (Some(0), "ok\n".to_owned(), String::new())
    );
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["ask", "result", "zzz"])?),
        (Some(1), String::new(), "error: unknown ask: zzz\n".to_owned())
    );
    Ok(())
}

#[test]
fn a_blocking_ask_polls_until_it_is_cancelled() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let open = serde_json::json!({"cmd": "ask.open", "target": "s1", "args": {
        "buttons": [{"id": "ok", "label": "ok"}], "style": "terminal", "align": "right", "title": "Wait here"}});
    let results = vec![
        serde_json::json!({"result": "pending"}),
        serde_json::json!({"result": "cancelled"}),
    ];
    let _server = modal_host(&socket, "ask", open, results)?;
    let args = ["ask", "--button", "ok", "--target", "s1", "Wait here"];
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &args)?),
        (Some(2), "{\"result\":\"cancelled\"}\n".to_owned(), String::new())
    );
    Ok(())
}

const ASK_OPEN_USAGE: &str = "Usage: agtermctl ask open [<options>] <title>\n  See 'agtermctl ask open --help' for more \
                              information.\n";

#[test]
fn ask_refusals_are_usage_errors_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a sent request would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let cases: [(&[&str], &str); 4] = [
        (&["ask", "--no-block", "Proceed?"], "ask requires at least one --button"),
        (
            &["ask", "--no-block", "--button", "y", "--width", "5", "Proceed?"],
            "width must be 10 to 100",
        ),
        (
            &["ask", "--button", "y", "--width", "-1", "Proceed?"],
            "width must be 10 to 100",
        ),
        (
            &[
                "ask", "open", "cancel", "--button", "ok", "--style", "gui", "--pane", "right",
            ],
            "--pane requires a session",
        ),
    ];
    for (args, message) in cases {
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(64), String::new(), format!("Error: {message}\n{ASK_OPEN_USAGE}")),
            "{args:?}"
        );
    }
    Ok(())
}

const ASK_HELP: &str = "\
OVERVIEW: Open, poll, or cancel a themed question dialog.

Use ask open TITLE when the title is 'open', 'result', or 'cancel'.

USAGE: agtermctl ask <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  open (default)          Ask a question and wait for its button answer.
  result                  Print a dialog's current or terminal result as JSON.
  cancel                  Cancel a dialog without answering a button.

  See 'agtermctl help ask <subcommand>' for detailed help.
";

const PICK_HELP: &str = "\
OVERVIEW: Open, poll, or cancel a native fuzzy picker.

USAGE: agtermctl pick <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  open (default)          Read choices from stdin and open a native fuzzy
                          picker.
  result                  Print a picker's current or terminal result as JSON.
  cancel                  Cancel a pending picker.

  See 'agtermctl help pick <subcommand>' for detailed help.
";

#[test]
fn ask_and_pick_help_mark_their_default_subcommand() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str); 4] = [
        (&["ask", "--help"], ASK_HELP),
        (&["help", "ask"], ASK_HELP),
        (&["pick", "--help"], PICK_HELP),
        (&["help", "pick"], PICK_HELP),
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

// MARK: - session overlay

const OVERLAY_SESSION: &str = "5B1E2C3D-4F5A-4B6C-8D7E-9F0A1B2C3D4E";
const OVERLAY_PAGE: &str = "9A8B7C6D-5E4F-4A3B-8C2D-1E0F9A8B7C6D";

/// An overlay host: `session.overlay.open` must equal `expected_open` and answers `opened`; each
/// `session.overlay.result` answers the next of `results` (the last one repeating), a refusal written as
/// `{"error": ...}`.
fn overlay_host(
    socket: &Path,
    expected_open: serde_json::Value,
    opened: serde_json::Value,
    results: Vec<serde_json::Value>,
) -> TestResult<Server<usize>> {
    serve(socket, 0, move |polls: &mut usize, request| {
        let sent = serde_json::to_value(&request).unwrap_or_default();
        match sent["cmd"].as_str().unwrap_or_default() {
            "session.overlay.open" if sent == expected_open => Response::with_result(opened.clone()).into(),
            "session.overlay.result" => {
                let result = results[(*polls).min(results.len() - 1)].clone();
                *polls += 1;
                match result["error"].as_str() {
                    Some(error) => Response::failure(error).into(),
                    None => Response::with_result(result).into(),
                }
            }
            _ => Response::failure(format!("unexpected request: {sent}")).into(),
        }
    })
}

#[test]
fn a_blocked_overlay_program_exits_with_its_status_and_prints_nothing() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let open = serde_json::json!({"cmd": "session.overlay.open", "target": OVERLAY_SESSION,
                                  "args": {"command": "exit 5"}});
    let results = vec![
        serde_json::json!({"error": "overlay still running"}),
        serde_json::json!({"id": OVERLAY_SESSION, "exitCode": 5}),
    ];
    let _server = overlay_host(&socket, open, serde_json::json!({"id": OVERLAY_SESSION}), results)?;
    let args = [
        "session",
        "overlay",
        "open",
        "--block",
        "--target",
        OVERLAY_SESSION,
        "exit 5",
    ];
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &args)?),
        (Some(5), String::new(), String::new())
    );
    let line = format!("{{\"ok\":true,\"result\":{{\"id\":\"{OVERLAY_SESSION}\",\"exitCode\":5}}}}\n");
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &[&args[..], &["--json"]].concat())?),
        (Some(5), line, String::new())
    );
    Ok(())
}

#[test]
fn a_blocked_overlay_program_without_a_status_or_refused_exits_1() -> TestResult {
    let dir = short_dir()?;
    let open = serde_json::json!({"cmd": "session.overlay.open", "target": "active", "args": {"command": "echo"}});
    let cases = [
        (
            serde_json::json!({"id": OVERLAY_SESSION}),
            "error: result missing exit code\n",
        ),
        (serde_json::json!({"error": "no overlay"}), "error: no overlay\n"),
    ];
    for (at, (result, stderr)) in cases.into_iter().enumerate() {
        let socket = dir.path().join(format!("{at}.sock"));
        let _server = overlay_host(
            &socket,
            open.clone(),
            serde_json::json!({"id": OVERLAY_SESSION}),
            vec![result],
        )?;
        assert_eq!(
            outcome(&run_on(
                dir.path(),
                &socket,
                &["session", "overlay", "open", "echo", "--block"]
            )?),
            (Some(1), String::new(), stderr.to_owned())
        );
    }
    Ok(())
}

#[test]
fn an_overlay_program_result_prints_its_exit_status() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let expected = serde_json::json!({"cmd": "session.overlay.result", "target": OVERLAY_SESSION});
    let _server = scripted(
        &socket,
        expected,
        serde_json::json!({"id": OVERLAY_SESSION, "exitCode": 5}),
    )?;
    let args = ["session", "overlay", "result", "--target", OVERLAY_SESSION];
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &args)?),
        (Some(0), "exit 5\n".to_owned(), String::new())
    );
    let line = format!("{{\"ok\":true,\"result\":{{\"id\":\"{OVERLAY_SESSION}\",\"exitCode\":5}}}}\n");
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &[&args[..], &["--json"]].concat())?),
        (Some(0), line, String::new())
    );
    Ok(())
}

#[test]
fn a_blocked_page_prints_its_outcome_and_a_page_result_reads_it_again() -> TestResult {
    let dir = short_dir()?;
    let page = dir.path().join("p.html");
    let page = page.to_str().ok_or("not UTF-8")?;
    let cases = [
        (
            "submitted",
            serde_json::json!({"pageID": OVERLAY_PAGE, "outcome": "submitted", "value": "picked"}),
            0,
        ),
        (
            "dismissed",
            serde_json::json!({"pageID": OVERLAY_PAGE, "outcome": "dismissed"}),
            2,
        ),
    ];
    for (name, outcome_object, code) in cases {
        let socket = dir.path().join(format!("{name}.sock"));
        let open = serde_json::json!({"cmd": "session.overlay.open", "target": OVERLAY_SESSION,
                                      "args": {"html": page}});
        let results = vec![
            serde_json::json!({"pageOutcome": {"pageID": OVERLAY_PAGE, "outcome": "pending"}}),
            serde_json::json!({ "pageOutcome": outcome_object }),
        ];
        let opened = serde_json::json!({"id": OVERLAY_SESSION, "pageID": OVERLAY_PAGE});
        let _server = overlay_host(&socket, open, opened, results)?;
        let args = [
            "session",
            "overlay",
            "open",
            "--html",
            page,
            "--block",
            "--target",
            OVERLAY_SESSION,
        ];
        let (status, stdout, stderr) = outcome(&run_on(dir.path(), &socket, &args)?);
        assert_eq!((status, stderr), (Some(code), String::new()), "{name}");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&stdout)?, outcome_object);
        let read = run_on(
            dir.path(),
            &socket,
            &["session", "overlay", "result", "--page", OVERLAY_PAGE],
        )?;
        let (status, stdout, _) = outcome(&read);
        assert_eq!(status, Some(code), "{name}");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&stdout)?, outcome_object);
    }
    Ok(())
}

#[test]
fn an_unclaimable_job_exits_1_having_launched_nothing() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let _server = serve(&socket, (), |(), request| {
        let sent = serde_json::to_value(&request).unwrap_or_default();
        if sent == serde_json::json!({"cmd": "session.overlay.job.run", "target": "J1"}) {
            Response::failure("job not claimable").into()
        } else {
            Response::failure(format!("unexpected request: {sent}")).into()
        }
    })?;
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["session", "overlay", "run-job", "J1"])?),
        (Some(1), String::new(), "error: job not claimable\n".to_owned())
    );
    Ok(())
}

const OVERLAY_OPEN_USAGE: &str = "Usage: agtermctl session overlay open [<options>] [<command>]\n  See 'agtermctl session \
                                  overlay open --help' for more information.\n";
const OVERLAY_RESIZE_USAGE: &str = "Usage: agtermctl session overlay resize [--size-percent <size-percent>] [--full] \
                                    [--target <target>] [--socket <socket>] [--json] [--window <window>]\n  See \
                                    'agtermctl session overlay resize --help' for more information.\n";

#[test]
fn overlay_refusals_are_usage_errors_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let cases: [(&[&str], &str, &str); 4] = [
        (
            &[
                "session", "overlay", "open", "--block", "--wait", "--target", "s", "true",
            ],
            "--block cannot be combined with --wait",
            OVERLAY_OPEN_USAGE,
        ),
        (
            &[
                "session",
                "overlay",
                "open",
                "--url",
                "http://localhost:5173/",
                "--block",
            ],
            "a page takes no --wait, and a --url page no --block",
            OVERLAY_OPEN_USAGE,
        ),
        (
            &["session", "overlay", "resize", "--target", "s"],
            "provide --size-percent PERCENT or --full",
            OVERLAY_RESIZE_USAGE,
        ),
        (
            &["session", "overlay", "resize", "--full", "--size-percent", "50"],
            "--full cannot be combined with --size-percent",
            OVERLAY_RESIZE_USAGE,
        ),
    ];
    for (args, message, usage) in cases {
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(64), String::new(), format!("Error: {message}\n{usage}")),
            "{args:?}"
        );
    }
    let navigate = run_on(dir.path(), &socket, &["session", "overlay", "navigate", "sideways"])?;
    assert_eq!(navigate.status.code(), Some(64));
    assert!(
        text(&navigate.stderr).starts_with("Error: step must be back, forward, browser, or finder\n"),
        "{}",
        text(&navigate.stderr)
    );
    Ok(())
}

const OVERLAY_HELP: &str = "\
OVERVIEW: Open, read, resize, or close an ephemeral overlay terminal on a
session.

USAGE: agtermctl session overlay <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  open                    Open an overlay running COMMAND (it closes when
                          COMMAND exits), or showing a page with --html or
                          --url.
  close                   Close the overlay terminal (destroys it; for one
                          shown on another Mac, requests its cancel).
  resize                  Resize an open overlay: floating at a percent, or
                          back to full-pane.
  reload                  Reload an HTML overlay: the file or URL it was opened
                          with, or with --current the page shown now.
  navigate                Step an HTML overlay's history, open it in the
                          browser, or reveal its current file in Finder.
  result                  Print the overlay program's exit status, or with
                          --page an HTML page's outcome.
  submit                  Answer an HTML overlay with a value and close it, as
                          its page would; a waiting --block prints the value.
  copy                    Print the selection made INSIDE the overlay (session
                          copy reads the pane underneath).
  text                    Print the overlay's terminal buffer as plain text (a
                          TUI's drawn screen, wrapped as rendered).
  run-job                 Run a remote overlay job this app handed to another
                          Mac.

  See 'agtermctl help session overlay <subcommand>' for detailed help.
";

const OVERLAY_RUN_JOB_HELP: &str = "\
OVERVIEW: Run a remote overlay job this app handed to another Mac.

Run on this Mac by the agterm on another Mac, over ssh, when it shows one of
this app's overlays: it claims the job, runs its program under that ssh
terminal, and reports how it ended. It is not meant to be run by hand. A job
that cannot be claimed exits 1 having launched nothing, one whose program
cannot be launched exits 127, and otherwise it exits with the program's status.

USAGE: agtermctl session overlay run-job <job> [--socket <socket>] [--json]

ARGUMENTS:
  <job>                   The job id the other Mac was handed.

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

#[test]
fn overlay_help_in_the_argument_parser_layout() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str); 4] = [
        (&["session", "overlay"], OVERLAY_HELP),
        (&["help", "session", "overlay"], OVERLAY_HELP),
        (&["session", "overlay", "run-job", "--help"], OVERLAY_RUN_JOB_HELP),
        (&["help", "session", "overlay", "run-job"], OVERLAY_RUN_JOB_HELP),
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

// MARK: - sidebar

const SIDEBAR_HELP: &str = "\
OVERVIEW: Sidebar visibility and view mode.

USAGE: agtermctl sidebar <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  visibility (default)    Sidebar visibility (show|hide|toggle).
  mode                    Sidebar view mode (tree|flagged|toggle).
  flagged-layout          Flagged view layout (flat|plain|tree|toggle).
  expand                  Expand every workspace in the sidebar.
  collapse                Collapse all workspaces except the active one.
  width                   Set the sidebar width in points.

  See 'agtermctl help sidebar <subcommand>' for detailed help.
";

const SIDEBAR_WIDTH_HELP: &str = "\
OVERVIEW: Set the sidebar width in points.

USAGE: agtermctl sidebar width <points> [--socket <socket>] [--json] [--window <window>]

ARGUMENTS:
  <points>                Sidebar width in points, clamped to the drag range.

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  --window <window>       Target window id, unique prefix, or 'active'
                          (defaults to the frontmost).
  -h, --help              Show help information.

";

#[test]
fn sidebar_help_marks_its_default_subcommand() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str); 4] = [
        (&["sidebar", "--help"], SIDEBAR_HELP),
        (&["help", "sidebar"], SIDEBAR_HELP),
        (&["sidebar", "width", "--help"], SIDEBAR_WIDTH_HELP),
        (&["help", "sidebar", "width"], SIDEBAR_WIDTH_HELP),
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
fn sidebar_commands_send_their_request_and_print_the_reply() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], serde_json::Value, serde_json::Value, &str); 7] = [
        (
            &["sidebar"],
            serde_json::json!({"cmd": "sidebar", "args": {"mode": "toggle"}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["sidebar", "hide"],
            serde_json::json!({"cmd": "sidebar", "args": {"mode": "hide"}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["sidebar", "mode", "flagged"],
            serde_json::json!({"cmd": "sidebar.mode", "args": {"mode": "flagged"}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["sidebar", "flagged-layout", "toggle"],
            serde_json::json!({"cmd": "sidebar.flagged-layout", "args": {"mode": "toggle"}}),
            serde_json::json!({"text": "flat"}),
            "flat\n",
        ),
        (
            &["sidebar", "expand", "--window", "w1"],
            serde_json::json!({"cmd": "sidebar.expand", "args": {"window": "w1"}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["sidebar", "collapse"],
            serde_json::json!({"cmd": "sidebar.collapse"}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["sidebar", "width", "300"],
            serde_json::json!({"cmd": "sidebar.width", "args": {"sidebarWidth": 300.0}}),
            serde_json::json!({"sidebarWidth": 300.0}),
            "300.0\n",
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
fn font_and_theme_commands_send_their_request_and_print_the_reply() -> TestResult {
    let dir = short_dir()?;
    let listing = serde_json::json!({"themes": ["Dracula", "Nord"], "theme": "Nord"});
    let cases: [(&[&str], serde_json::Value, serde_json::Value, &str); 5] = [
        (
            &["font", "inc", "--target", "s1"],
            serde_json::json!({"cmd": "font.inc", "target": "s1"}),
            serde_json::json!({"id": "s1"}),
            "ok\n",
        ),
        (
            &["font", "reset", "--pane", "right", "--window", "w1"],
            serde_json::json!({"cmd": "font.reset", "target": "active", "args": {"pane": "right", "window": "w1"}}),
            serde_json::json!({"id": "s1"}),
            "ok\n",
        ),
        (
            &["theme", "list"],
            serde_json::json!({"cmd": "theme.list"}),
            listing.clone(),
            "  default ghostty\n  Dracula\n* Nord\n",
        ),
        (
            &["theme", "set", "Nord"],
            serde_json::json!({"cmd": "theme.set", "args": {"name": "Nord"}}),
            listing,
            "  default ghostty\n  Dracula\n* Nord\n",
        ),
        (
            &["theme", "set", "--light", "Builtin Light", "--dark", "Nord"],
            serde_json::json!({"cmd": "theme.set", "args": {"light": "Builtin Light", "dark": "Nord"}}),
            serde_json::json!({"themes": ["Builtin Light", "Nord"], "sync": true, "light": "Builtin Light", "dark": "Nord"}),
            "syncing with macOS appearance — light: Builtin Light, dark: Nord\n  default ghostty\n* Builtin Light\n* \
             Nord\n",
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
fn font_and_theme_refusals_are_usage_errors_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let cases: [(&[&str], &str); 2] = [
        (
            &["font", "inc", "--pane", "middle", "--target", "s1"],
            "--pane must be left, right, or scratch",
        ),
        (
            &["theme", "set", "Dracula", "--light", "Builtin Light"],
            "Pass either a NAME or --light, not both.",
        ),
    ];
    for (args, message) in cases {
        let (code, stdout, stderr) = outcome(&run_on(dir.path(), &socket, args)?);
        assert_eq!((code, stdout.as_str()), (Some(64), ""), "{args:?}");
        assert!(stderr.starts_with(&format!("Error: {message}\n")), "{stderr}");
        assert!(stderr.contains("\nUsage: agtermctl "), "{stderr}");
    }
    Ok(())
}

#[test]
fn keymap_and_hooks_commands_send_their_request_and_print_the_reply() -> TestResult {
    let dir = short_dir()?;
    let keymap = serde_json::json!({"keymap": {
        "path": "/c/keymap.conf",
        "actions": [{"action": "new_session", "chord": "ctrl+shift+f12", "overridden": true}, {"action": "dashboard"}],
        "commands": [
            {"name": "mark", "shortcut": "ctrl+shift+f11", "errorHud": true, "errorPosition": "center", "repeats": false},
        ],
        "diagnostics": [{"line": 3, "message": "unknown verb 'bogus'"}],
    }});
    let hooks = serde_json::json!({"hooks": {
        "path": "/c/hooks.conf",
        "diagnostics": [{"line": 2, "message": "unknown event 'session.renamed'"}],
        "hooks": [{"kind": "session.created", "command": "true", "line": 1, "pending": 0, "dropped": 0}],
    }});
    let cases: [(&[&str], serde_json::Value, serde_json::Value, &str); 6] = [
        (
            &["keymap", "reload"],
            serde_json::json!({"cmd": "keymap.reload"}),
            serde_json::json!({"count": 1}),
            "1 diagnostic(s)\n",
        ),
        (
            &["hooks", "reload"],
            serde_json::json!({"cmd": "hooks.reload"}),
            serde_json::json!({"count": 0}),
            "ok\n",
        ),
        (
            &["keymap", "list"],
            serde_json::json!({"cmd": "keymap.list"}),
            keymap,
            "keymap: /c/keymap.conf\n\nactions:\n  * new_session  ctrl+shift+f12\n    dashboard    -\n\ncommands:\n    \
             mark  ctrl+shift+f11  --error-hud --error-position center\n\ndiagnostics:\n    line 3: unknown verb \
             'bogus'\n",
        ),
        (
            &["hooks", "list"],
            serde_json::json!({"cmd": "hooks.list"}),
            hooks,
            "hooks: /c/hooks.conf\n  line 1: on session.created true\n\ndiagnostics:\n    line 2: unknown event \
             'session.renamed'\n",
        ),
        (
            &["keymap", "run", "mark", "--target", "s1"],
            serde_json::json!({"cmd": "keymap.run", "target": "s1", "args": {"name": "mark"}}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["keymap", "reload", "--json"],
            serde_json::json!({"cmd": "keymap.reload"}),
            serde_json::json!({"count": 1}),
            "{\"ok\":true,\"result\":{\"count\":1}}\n",
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
    let socket = dir.path().join("refuse.sock");
    let _server = serve(&socket, (), |(), _| {
        Response::failure("no custom command named nope").into()
    })?;
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["keymap", "run", "nope"])?),
        (
            Some(1),
            String::new(),
            "error: no custom command named nope\n".to_owned()
        )
    );
    Ok(())
}

#[test]
fn keymap_and_hooks_usage_errors_come_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let cases: [&[&str]; 4] = [
        &["keymap", "run"],
        &["keymap", "reload", "--window", "w1"],
        &["hooks", "list", "--window", "active"],
        &["hooks", "reload", "--target", "s1"],
    ];
    for args in cases {
        let (code, stdout, stderr) = outcome(&run_on(dir.path(), &socket, args)?);
        assert_eq!((code, stdout.as_str()), (Some(64), ""), "{args:?}");
        assert!(stderr.starts_with("Error: "), "{stderr}");
        assert!(stderr.contains("\nUsage: agtermctl "), "{stderr}");
    }
    Ok(())
}

// MARK: - browser and config

const BROWSER_HELP: &str = "\
OVERVIEW: Saved browser storage of --persistent URL overlays, and where clicked
links open.

USAGE: agtermctl browser <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  clear                   Remove every cookie and all site data the saved
                          browser store holds.
  links                   Set or read where a clicked web link in a terminal
                          opens (browser|overlay).

  See 'agtermctl help browser <subcommand>' for detailed help.
";

const BROWSER_LINKS_HELP: &str = "\
OVERVIEW: Set or read where a clicked web link in a terminal opens
(browser|overlay).

overlay opens http and https links in a full session web overlay with
JavaScript, navigation buttons and the saved browser store; with no MODE the
current one is printed. A link in a HUD, a link clicked while a HUD is up or
the terminal is zoomed, and a link the overlay cannot show open in the browser.

USAGE: agtermctl browser links [<mode>] [--socket <socket>] [--json]

ARGUMENTS:
  <mode>                  Mode: browser or overlay. Omit to read the current
                          mode.

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

const BROWSER_CLEAR_HELP: &str = "\
OVERVIEW: Remove every cookie and all site data the saved browser store holds.

Refused while a --persistent page is open, one in a just-closed session that
can still be restored included: an open page writes its login back. Removing
the local data does not sign you out on the server.

USAGE: agtermctl browser clear [--socket <socket>] [--json]

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

const CONFIG_HELP: &str = "\
OVERVIEW: Config commands.

USAGE: agtermctl config <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  reload                  Re-read and apply the agterm-scoped ghostty.conf
                          (prints the diagnostic count).

  See 'agtermctl help config <subcommand>' for detailed help.
";

const CONFIG_RELOAD_HELP: &str = "\
OVERVIEW: Re-read and apply the agterm-scoped ghostty.conf (prints the
diagnostic count).

USAGE: agtermctl config reload [--socket <socket>] [--json]

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

#[test]
fn browser_and_config_help_in_the_argument_parser_layout() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str); 8] = [
        (&["browser", "--help"], BROWSER_HELP),
        (&["browser"], BROWSER_HELP),
        (&["help", "browser", "links"], BROWSER_LINKS_HELP),
        (&["browser", "links", "--help"], BROWSER_LINKS_HELP),
        (&["browser", "clear", "--help"], BROWSER_CLEAR_HELP),
        (&["config", "--help"], CONFIG_HELP),
        (&["help", "config"], CONFIG_HELP),
        (&["config", "reload", "--help"], CONFIG_RELOAD_HELP),
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
fn browser_and_config_commands_send_their_request_and_print_the_reply() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], serde_json::Value, serde_json::Value, &str); 7] = [
        (
            &["browser", "links"],
            serde_json::json!({"cmd": "browser.links"}),
            serde_json::json!({"text": "browser"}),
            "browser\n",
        ),
        (
            &["browser", "links", "overlay"],
            serde_json::json!({"cmd": "browser.links", "args": {"mode": "overlay"}}),
            serde_json::json!({"text": "overlay"}),
            "overlay\n",
        ),
        (
            &["browser", "links", "overlay", "--json"],
            serde_json::json!({"cmd": "browser.links", "args": {"mode": "overlay"}}),
            serde_json::json!({"text": "overlay"}),
            "{\"ok\":true,\"result\":{\"text\":\"overlay\"}}\n",
        ),
        (
            &["browser", "clear"],
            serde_json::json!({"cmd": "browser.clear"}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["config", "reload"],
            serde_json::json!({"cmd": "config.reload"}),
            serde_json::json!({"count": 2}),
            "2 diagnostic(s)\n",
        ),
        (
            &["config", "reload"],
            serde_json::json!({"cmd": "config.reload"}),
            serde_json::json!({"count": 0}),
            "ok\n",
        ),
        (
            &["config", "reload", "--json"],
            serde_json::json!({"cmd": "config.reload"}),
            serde_json::json!({"count": 0}),
            "{\"ok\":true,\"result\":{\"count\":0}}\n",
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
    let socket = dir.path().join("refuse.sock");
    let _server = serve(&socket, (), |(), _| {
        Response::failure("1 persistent page(s) still open").into()
    })?;
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["browser", "clear"])?),
        (
            Some(1),
            String::new(),
            "error: 1 persistent page(s) still open\n".to_owned()
        )
    );
    Ok(())
}

#[test]
fn browser_and_config_usage_errors_come_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let cases: [(&[&str], &str); 5] = [
        (&["browser", "links", "tab"], "Error: mode must be browser or overlay\n"),
        (&["browser", "links", "overlay", "--window", "w1"], "Error: "),
        (&["browser", "clear", "--window", "w1"], "Error: "),
        (&["browser", "clear", "--target", "s1"], "Error: "),
        (&["config", "reload", "--window", "w1"], "Error: "),
    ];
    for (args, prefix) in cases {
        let (code, stdout, stderr) = outcome(&run_on(dir.path(), &socket, args)?);
        assert_eq!((code, stdout.as_str()), (Some(64), ""), "{args:?}");
        assert!(stderr.starts_with(prefix), "{stderr}");
        assert!(stderr.contains("\nUsage: agtermctl "), "{stderr}");
    }
    Ok(())
}

// MARK: - restore

const RESTORE_HELP: &str = "\
OVERVIEW: Restore-running-command commands.

USAGE: agtermctl restore <subcommand>

OPTIONS:
  -h, --help              Show help information.

SUBCOMMANDS:
  capture                 Capture every pane's running command now, so a forced
                          exit restores it.
  clear                   Clear every session's saved foreground command so the
                          next restart restores plain shells.
  mode                    Read or set what a restart does with your sessions.

  See 'agtermctl help restore <subcommand>' for detailed help.
";

const RESTORE_CAPTURE_HELP: &str = "\
OVERVIEW: Capture every pane's running command now, so a forced exit restores
it.

agterm captures the running commands when it quits, so an exit that never gets
there leaves every pane restoring a plain shell: a force quit, a crash, a hard
reset, a power loss. This runs the same capture on demand, app-global across
every open window, and prints how many panes had a command to capture.
Consumption is unchanged: the next launch arms each captured command once and
clears it.

A capture is only as fresh as its last run, so a pager or a build that has
finished since still re-runs after a crash. \"restore clear\" drops every
captured command, and restore-denylist.conf in the config directory keeps a
named program from re-running at all.

Typed at a prompt it records ITSELF: while it runs it is that pane's foreground
process, so that pane comes back running the capture command. Bind it or run it
from a scheduled job rather than by hand; \"restore clear\" is app-global, so it
is no per-pane undo.

This command is available only when this launch is in rerun mode. In
fresh-shell or live mode it fails and names the active mode.

USAGE: agtermctl restore capture [--socket <socket>] [--json]

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

const RESTORE_CLEAR_HELP: &str = "\
OVERVIEW: Clear every session's saved foreground command so the next restart
restores plain shells.

This is app-global and CAPTURE-scoped: it drops the foreground commands agterm
captured at quit, across every open window, and leaves per-session
restore-command overrides alone.

Not to be confused with `agtermctl session restore --clear`, which is
per-session and OVERRIDE-scoped: it drops one pane's pinned command so that
pane goes back to auto-capture.

USAGE: agtermctl restore clear [--socket <socket>] [--json]

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

const RESTORE_MODE_HELP: &str = "\
OVERVIEW: Read or set what a restart does with your sessions.

With no argument this reports the policy: what settings hold for the next
launch, what THIS launch asked for, and what it actually got. The two requested
values differ once the mode has been changed since this instance started, which
is exactly when a caller is confused about why nothing happened.

Setting one writes it for the NEXT launch. This process keeps the mode it
started with, and that is not a shortcut: a pane is wrapped in a zmx daemon or
not at the moment it is created, so no setting can retrofit a shell that is
already running.

fresh shells (none) re-spawns each pane in its saved directory. re-run (rerun)
starts the command each pane had at the last clean quit. live keeps the actual
processes alive.

Switching away from live and restarting ends every detached live process in
this state directory. If live was requested but could not be used, the reason
is reported here.

USAGE: agtermctl restore mode [<mode>] [--socket <socket>] [--json]

ARGUMENTS:
  <mode>                  none|rerun|live. Omit to read the current policy.

OPTIONS:
  --socket <socket>       Override the control socket path.
  --json                  Print the raw JSON response.
  -h, --help              Show help information.

";

#[test]
fn restore_help_in_the_argument_parser_layout() -> TestResult {
    let dir = short_dir()?;
    let cases: [(&[&str], &str); 6] = [
        (&["restore", "--help"], RESTORE_HELP),
        (&["restore"], RESTORE_HELP),
        (&["help", "restore"], RESTORE_HELP),
        (&["restore", "capture", "--help"], RESTORE_CAPTURE_HELP),
        (&["restore", "clear", "--help"], RESTORE_CLEAR_HELP),
        (&["help", "restore", "mode"], RESTORE_MODE_HELP),
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
fn restore_commands_send_their_request_and_print_the_reply() -> TestResult {
    let dir = short_dir()?;
    let status = serde_json::json!({
        "configured": "live",
        "requestedAtLaunch": "rerun",
        "active": "rerun",
        "restartRequired": true,
    });
    let cases: [(&[&str], serde_json::Value, serde_json::Value, &str); 4] = [
        (
            &["restore", "capture"],
            serde_json::json!({"cmd": "restore.capture"}),
            serde_json::json!({"count": 2, "text": "captured 2 panes"}),
            "captured 2 panes\n",
        ),
        (
            &["restore", "clear"],
            serde_json::json!({"cmd": "restore.clear"}),
            serde_json::json!({}),
            "ok\n",
        ),
        (
            &["restore", "mode"],
            serde_json::json!({"cmd": "restore.mode"}),
            serde_json::json!({"restore": status}),
            "configured: live (next launch)\nthis launch: requested rerun, active rerun\nrestart agterm to apply the \
             configured mode\n",
        ),
        (
            &["restore", "mode", "live"],
            serde_json::json!({"cmd": "restore.mode", "args": {"mode": "live"}}),
            serde_json::json!({"restore": status}),
            "configured: live (next launch)\nthis launch: requested rerun, active rerun\nrestart agterm to apply the \
             configured mode\n",
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
    let socket = dir.path().join("json.sock");
    let _server = scripted(
        &socket,
        serde_json::json!({"cmd": "restore.mode"}),
        serde_json::json!({"restore": status}),
    )?;
    let (code, stdout, stderr) = outcome(&run_on(dir.path(), &socket, &["restore", "mode", "--json"])?);
    assert_eq!((code, stderr.as_str()), (Some(0), ""));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout)?,
        serde_json::json!({"ok": true, "result": {"restore": status}})
    );
    let socket = dir.path().join("refuse.sock");
    let _server = serve(&socket, (), |(), _| {
        Response::failure("restore capture requires rerun mode; the active mode is live").into()
    })?;
    assert_eq!(
        outcome(&run_on(dir.path(), &socket, &["restore", "capture"])?),
        (
            Some(1),
            String::new(),
            "error: restore capture requires rerun mode; the active mode is live\n".to_owned()
        )
    );
    Ok(())
}

#[test]
fn restore_usage_errors_come_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let cases: [(&[&str], &str); 5] = [
        (
            &["restore", "mode", "fresh"],
            "Error: mode must be none, rerun, or live\nUsage: agtermctl restore mode [<mode>] [--socket <socket>] \
             [--json]\n  See 'agtermctl restore mode --help' for more information.\n",
        ),
        (&["restore", "mode", "live", "--window", "w1"], "Error: "),
        (&["restore", "capture", "--window", "w1"], "Error: "),
        (&["restore", "clear", "--target", "s1"], "Error: "),
        (&["restore", "bogus"], "Error: "),
    ];
    for (args, prefix) in cases {
        let (code, stdout, stderr) = outcome(&run_on(dir.path(), &socket, args)?);
        assert_eq!((code, stdout.as_str()), (Some(64), ""), "{args:?}");
        assert!(stderr.starts_with(prefix), "{stderr}");
        assert!(stderr.contains("\nUsage: agtermctl "), "{stderr}");
    }
    Ok(())
}

#[test]
fn sidebar_width_json_prints_the_result_and_a_refusal_exits_1() -> TestResult {
    let dir = short_dir()?;
    let socket = dir.path().join("agterm.sock");
    let expected = serde_json::json!({"cmd": "sidebar.width", "args": {"window": "w1", "sidebarWidth": 301.5}});
    let _server = scripted(&socket, expected, serde_json::json!({"sidebarWidth": 301.5}))?;
    let json = run_on(
        dir.path(),
        &socket,
        &["sidebar", "width", "301.5", "--window", "w1", "--json"],
    )?;
    let (code, stdout, stderr) = outcome(&json);
    assert_eq!((code, stderr.as_str()), (Some(0), ""));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout)?,
        serde_json::json!({"ok": true, "result": {"sidebarWidth": 301.5}})
    );
    // the scripted server refuses any other request, naming it
    let (code, stdout, stderr) = outcome(&run_on(dir.path(), &socket, &["sidebar", "width", "1"])?);
    assert_eq!((code, stdout.as_str()), (Some(1), ""));
    assert!(stderr.starts_with("error: unexpected request: "), "{stderr}");
    Ok(())
}

#[test]
fn sidebar_refusals_are_usage_errors_before_connecting() -> TestResult {
    let dir = short_dir()?;
    // nothing listens here: a connection attempt would exit 1, not 64
    let socket = dir.path().join("agterm.sock");
    let width = "Usage: agtermctl sidebar width <points> [--socket <socket>] [--json] [--window <window>]\n  See \
                 'agtermctl sidebar width --help' for more information.\n";
    let mode = "Usage: agtermctl sidebar mode [<mode>] [--socket <socket>] [--json]\n  See 'agtermctl sidebar mode \
                --help' for more information.\n";
    let layout = "Usage: agtermctl sidebar flagged-layout [<layout>] [--socket <socket>] [--json]\n  See 'agtermctl \
                  sidebar flagged-layout --help' for more information.\n";
    let cases: [(&[&str], String); 4] = [
        (
            &["sidebar", "width", "wide"],
            format!(
                "Error: The value 'wide' is invalid for '<points>'\nHelp:  <points>  Sidebar width in points, clamped \
                 to the drag range.\n{width}"
            ),
        ),
        (
            &["sidebar", "width", "nan"],
            format!("Error: points must be a number\n{width}"),
        ),
        (
            &["sidebar", "mode", "grid"],
            format!("Error: mode must be tree, flagged, or toggle\n{mode}"),
        ),
        (
            &["sidebar", "flagged-layout", "grid"],
            format!("Error: layout must be flat, plain, tree, or toggle\n{layout}"),
        ),
    ];
    for (args, expected) in cases {
        assert_eq!(
            outcome(&run_on(dir.path(), &socket, args)?),
            (Some(64), String::new(), expected),
            "{args:?}"
        );
    }
    Ok(())
}
