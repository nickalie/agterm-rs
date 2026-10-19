//! PowerShell live sessions end to end, at the session and the core level: a session started the way a live pane's
//! wrap starts its PowerShell (`agterm-core`'s login arguments and integration variable, this repository's
//! integration script) reports its working directory and its prompt and command marks to the server's terminal; a
//! program typed into it is read back through the process table as its foreground command, and `agterm-core`'s
//! replay of that argv into a recreated session runs it again with the same arguments, then leaves the integrated
//! prompt.
//!
//! On every OS with a PowerShell 7 (`pwsh`; every CI runner carries one, and Windows requires it). On Unix the shell
//! runs in a throwaway home, so no profile, history or module cache of the user's is read or written; every session
//! gets the test's environment without the live agterm's `AGTERM_*` and `ZMX_*` variables, and a namespace directory
//! of its own (only named in its environment: no server runs).

#[expect(dead_code, reason = "the helpers serve the pane, the lead and these tests alike")]
mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agterm_core::command_restore::{ProcessGroupMember, group_descent_candidates, is_idle_shell};
use agterm_core::live::replay::{self, FinalShell, powershell_quoted};
use agterm_core::live::shell_integration::{self, Inherited, LoginShell};
use agterm_host::ProcessTable;
use agterm_session::process_table::ProcTable;
use agterm_session::protocol::SessionSpec;
use agterm_session::server::pane::Pane;
use support::{Client, PATIENCE, TestResult, config};

/// The file the foreground helper records its arguments in; it ends once a file of the same name and `.stop` exists.
const RECORD: &str = "PWSH_TEST_RECORD";

/// Arguments the helper is run with and replayed with: spaces, quotes of both kinds and PowerShell's typographic
/// ones, backslashes before a quote and at the end, an empty one, PowerShell's operators, and what it would expand
/// unquoted. No tab: typed at the prompt, `PSReadLine` takes one for completion.
const ODD_ARGS: [&str; 9] = [
    "a b",
    "it's \"quoted\"",
    r#"back\"slash"#,
    r"C:\dir with space\",
    "pipe|amp&",
    "",
    "$HOME",
    "semi;colon",
    "\u{2018}curly\u{2019}",
];

/// PowerShell 7 as a live pane runs it: on Windows the login shell `agterm-session` finds, which must be `pwsh.exe`
/// here; elsewhere `pwsh` on `PATH`, if any.
fn pwsh() -> TestResult<Option<String>> {
    if cfg!(windows) {
        let shell = agterm_session::launch::login_shell().ok_or("no PowerShell on this Windows host")?;
        let name = Path::new(&shell)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert!(
            name.eq_ignore_ascii_case("pwsh.exe"),
            "the replay of quotes needs PowerShell 7: {shell}"
        );
        return Ok(Some(shell));
    }
    let Some(path) = std::env::var_os("PATH") else {
        return Ok(None);
    };
    Ok(std::env::split_paths(&path)
        .map(|dir| dir.join("pwsh"))
        .find(|candidate| candidate.is_file())
        .map(|pwsh| pwsh.display().to_string()))
}

/// The repository's integration script, staged as `scripts/stage.sh` stages it under a resources directory in `dir`.
fn staged_resources(dir: &Path) -> TestResult<PathBuf> {
    let resources = dir.join("resources");
    let integration = resources.join("shell-integration").join("pwsh");
    fs::create_dir_all(&integration)?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("resources")
        .join("shell-integration")
        .join("pwsh")
        .join("ngterm.ps1");
    fs::copy(source, integration.join("ngterm.ps1"))?;
    assert!(LoginShell::Pwsh.is_bundled(&resources));
    Ok(resources)
}

/// A live pane's environment: the test's own with the integration injected, the title and cursor features on, a
/// terminal .NET knows, and on Unix a throwaway home.
fn session_environment(dir: &Path, resources: &Path) -> TestResult<BTreeMap<String, String>> {
    let mut environment: BTreeMap<String, String> = support::environment().into_iter().collect();
    shell_integration::inject(LoginShell::Pwsh, resources, &Inherited::default(), &mut environment);
    environment.insert("GHOSTTY_SHELL_FEATURES".to_owned(), "cursor,title".to_owned());
    environment.insert("TERM".to_owned(), "xterm-256color".to_owned());
    environment.remove("TERMINFO");
    // no update check or telemetry from a test's shells
    environment.insert("POWERSHELL_UPDATECHECK".to_owned(), "Off".to_owned());
    environment.insert("POWERSHELL_TELEMETRY_OPTOUT".to_owned(), "1".to_owned());
    environment.insert(RECORD.to_owned(), dir.join("record.json").display().to_string());
    if cfg!(unix) {
        let home = dir.join("home");
        fs::create_dir_all(&home)?;
        for (key, below) in [
            ("HOME", ""),
            ("XDG_CONFIG_HOME", ".config"),
            ("XDG_DATA_HOME", ".local/share"),
            ("XDG_CACHE_HOME", ".cache"),
        ] {
            environment.insert(key.to_owned(), home.join(below).display().to_string());
        }
    }
    Ok(environment)
}

fn session(argv: Vec<String>, environment: &BTreeMap<String, String>, cwd: &Path) -> SessionSpec {
    SessionSpec {
        argv,
        env: environment.clone().into_iter().collect(),
        cwd: Some(cwd.display().to_string()),
        tracked_env: Vec::new(),
    }
}

/// Poll `read` until `done` holds for what it reads; the last read otherwise, in the panic.
fn wait_for<T: std::fmt::Debug>(what: &str, mut read: impl FnMut() -> T, done: impl Fn(&T) -> bool) -> T {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let got = read();
        if done(&got) {
            return got;
        }
        assert!(Instant::now() < deadline, "{what}: still {got:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Read `client` until its raw output holds `needle` (a control sequence the shown text leaves out).
fn expect_raw(client: &mut Client, needle: &[u8]) {
    let deadline = Instant::now() + PATIENCE;
    while !client.output.windows(needle.len()).any(|window| window == needle) {
        assert!(
            Instant::now() < deadline && client.exit.is_none(),
            "no {:?} in the output: {:?}",
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(&client.output)
        );
        client.receive(Duration::from_millis(100));
    }
}

/// The directory an OSC 7 report names, when it is the integration's: `kitty-shell-cwd://` with this host's name,
/// then the path (a Windows one after a `/`, with `/` separators).
fn reported_directory(pwd: &str) -> Option<PathBuf> {
    let rest = pwd.strip_prefix("kitty-shell-cwd://")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    if host != hostname() {
        return None;
    }
    let path = if cfg!(windows) { path.strip_prefix('/')? } else { path };
    Some(PathBuf::from(path))
}

/// This machine's name as the integration reports it: Windows' computer name, `gethostname` elsewhere.
fn hostname() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
    #[cfg(unix)]
    {
        let mut buffer = [0_u8; 256];
        // SAFETY: the buffer is valid for its length, and `gethostname` writes at most that many bytes.
        let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
        assert_eq!(status, 0, "gethostname");
        let len = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..len]).into_owned()
    }
}

/// Whether `pane`'s terminal was told `cwd` as its working directory.
fn wait_for_directory(pane: &Pane, cwd: &Path) {
    let canonical = cwd.canonicalize().ok();
    wait_for(
        "the reported directory",
        || pane.with_terminal(|terminal| terminal.pwd()),
        |pwd| reported_directory(pwd).and_then(|path| path.canonicalize().ok()) == canonical,
    );
}

/// The program in the foreground of the shell `shell_pid` runs, read through the process table: the terminal's
/// foreground, or, where PowerShell leaves its programs in its own process group (Unix: it has no job control), the
/// group leader's child. `None` while the shell holds its prompt.
fn foreground_argv(table: &dyn ProcessTable, shell_pid: i32) -> Option<Vec<String>> {
    let group = table.terminal_foreground(shell_pid)?;
    let argv = table.argv(group)?;
    if !is_idle_shell(&argv, None) {
        return Some(argv);
    }
    let members: Vec<ProcessGroupMember> = table
        .group(group)
        .into_iter()
        .map(|stat| ProcessGroupMember {
            pid: stat.pid,
            ppid: stat.ppid,
        })
        .collect();
    group_descent_candidates(group, &members)
        .into_iter()
        .find_map(|pid| table.argv(pid))
}

/// Type `line` at the prompt and Return: a carriage return, which `PSReadLine`'s raw input takes for the Enter key on
/// every OS (a line feed is Ctrl+J there).
fn type_line(pane: &Pane, line: &str) -> TestResult {
    pane.write(format!("{line}\r").as_bytes())?;
    Ok(())
}

/// The line that runs `argv` at a PowerShell prompt.
fn typed(argv: &[String]) -> String {
    let words: Vec<String> = argv.iter().map(|word| powershell_quoted(word)).collect();
    format!("& {}", words.join(" "))
}

/// The helper's argv: this test binary, running [`foreground_helper`] with [`ODD_ARGS`] after `--`.
fn helper_argv() -> TestResult<Vec<String>> {
    let mut argv = vec![
        std::env::current_exe()?.display().to_string(),
        "--ignored".to_owned(),
        "--exact".to_owned(),
        "foreground_helper".to_owned(),
        "--".to_owned(),
    ];
    argv.extend(ODD_ARGS.iter().map(|arg| (*arg).to_owned()));
    Ok(argv)
}

/// A program that exits with 3, typed at the prompt.
fn exit_three() -> String {
    if cfg!(windows) {
        "& cmd.exe /d /c exit 3".to_owned()
    } else {
        "& /bin/sh -c 'exit 3'".to_owned()
    }
}

#[test]
fn a_live_powershell_reports_its_directory_and_replays_its_foreground_command() -> TestResult {
    let Some(shell) = pwsh()? else {
        eprintln!("SKIPPED: no pwsh on PATH");
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let resources = staged_resources(dir.path())?;
    let environment = session_environment(dir.path(), &resources)?;
    let cwd = dir.path().join("work dir");
    fs::create_dir_all(&cwd)?;
    let cwd = cwd.canonicalize()?;
    #[cfg(windows)]
    let cwd = PathBuf::from(cwd.display().to_string().trim_start_matches(r"\\?\"));
    let record = dir.path().join("record.json");

    // the shell a live pane starts: its prompt reports the directory, marks itself and titles the pane
    let first = session(LoginShell::Pwsh.login_arguments(&shell), &environment, &cwd);
    let (pane, creator) = Pane::spawn(config("pwsh-first", dir.path(), first))?;
    let mut client = Client::new(creator);
    wait_for_directory(&pane, &cwd);
    expect_raw(&mut client, b"\x1b]133;A\x07");
    expect_raw(&mut client, b"\x1b]133;B\x07");
    wait_for(
        "the title",
        || pane.with_terminal(|terminal| terminal.title()),
        |title| title.ends_with("work dir"),
    );
    // a command line is marked when it runs, and its status when the prompt is back
    type_line(&pane, &exit_three())?;
    expect_raw(&mut client, b"\x1b]133;C\x07");
    expect_raw(&mut client, b"\x1b]133;D;3\x07");

    // a program in the foreground, read through the process table
    let helper = helper_argv()?;
    type_line(&pane, &typed(&helper))?;
    let table = ProcTable::new();
    let shell_pid = i32::try_from(pane.pid())?;
    let foreground = wait_for(
        "the helper in the foreground",
        || foreground_argv(&table, shell_pid),
        |argv| argv.as_ref().is_some_and(|argv| argv.ends_with(&helper[1..])),
    )
    .unwrap_or_default();
    wait_for("the helper's record", || record.exists(), |exists| *exists);
    pane.kill(true)?;
    drop(client);
    fs::remove_file(&record)?;

    // the replay a recreated session runs: the same arguments, then the integrated prompt
    let rearm = shell_integration::rearm(LoginShell::Pwsh, &environment).ok_or("no integration to re-arm")?;
    let script = replay::render_argv(
        &foreground,
        &FinalShell {
            path: &shell,
            kind: LoginShell::Pwsh,
            rearm: &rearm,
        },
    );
    let mut argv = vec![shell.clone()];
    argv.extend(LoginShell::Pwsh.script_flags().iter().map(|flag| (*flag).to_owned()));
    argv.push(script);
    let (pane, creator) = Pane::spawn(config("pwsh-replayed", dir.path(), session(argv, &environment, &cwd)))?;
    let mut client = Client::new(creator);
    let recorded = wait_for(
        "the replayed helper's record",
        || fs::read(&record).ok(),
        Option::is_some,
    )
    .unwrap_or_default();
    let recorded: Vec<String> = serde_json::from_slice(&recorded)?;
    assert_eq!(recorded, ODD_ARGS);
    fs::write(record.with_extension("stop"), "")?;
    wait_for_directory(&pane, &cwd);
    expect_raw(&mut client, b"\x1b]133;A\x07");
    assert_eq!(pane.exit_status(), None, "the shell stays once the command ended");
    pane.kill(true)?;
    Ok(())
}

/// Not a test of its own: the program [`a_live_powershell_reports_its_directory_and_replays_its_foreground_command`]
/// runs in its sessions, which records its arguments after `--` as JSON in the file [`RECORD`] names, then holds the
/// foreground until the `.stop` file beside it exists.
#[test]
#[ignore = "the PowerShell session test's child"]
fn foreground_helper() {
    let Some(record) = std::env::var_os(RECORD).map(PathBuf::from) else {
        return;
    };
    let argv: Vec<String> = std::env::args().skip_while(|arg| arg != "--").skip(1).collect();
    fs::write(&record, serde_json::to_vec(&argv).unwrap()).unwrap();
    let stop = record.with_extension("stop");
    let deadline = Instant::now() + PATIENCE * 10;
    while !stop.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}
