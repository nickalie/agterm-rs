//! PowerShell live sessions end to end, at the session and the core level: a session started the way a live pane's
//! wrap starts its PowerShell (`agterm-core`'s login arguments and integration variable, this repository's
//! integration script) reports its working directory and its prompt and command marks to the server's terminal; a
//! program typed into it is read the way the restore capture reads it (`agterm-core`'s `ForegroundReader` over the
//! process table): on Windows as its foreground command, on Linux and macOS, where PowerShell has no job control and
//! nothing tells its foreground program from a background one, not at all (the shell holds the foreground). Then
//! `agterm-core`'s replay of the program's argv (the one read on Windows, the one typed elsewhere) into a recreated
//! session runs it again with the same arguments, then leaves the integrated prompt.
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

#[cfg(unix)]
use agterm_core::command_restore::PaneForeground;
use agterm_core::live::replay::{self, FinalShell, powershell_quoted};
use agterm_core::live::shell_integration::{self, Inherited, LoginShell};
use agterm_core::restore::capture::ForegroundReader;
use agterm_host::ProcessTable;
use agterm_session::process_table::ProcTable;
use agterm_session::protocol::SessionSpec;
use agterm_session::server::pane::Pane;
use support::{Client, PATIENCE, TestResult, config, wait_for};

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

/// The integration's mark where a prompt's input starts.
const INPUT_MARK: &[u8] = b"\x1b]133;B\x07";
/// The cursor query `PSReadLine` sends on Unix once it reads a line, its terminal out of the cooked mode.
const CURSOR_QUERY: &[u8] = b"\x1b[6n";

/// Where the first `needle` in `haystack` at or after `from` ends.
fn find_end(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at + needle.len())
}

/// Read `client` until the shell's `count`th prompt is about to read input: its input mark, and on Unix the cursor
/// query after it, which .NET sends once it took the terminal out of the cooked mode (no echo, a Return kept a
/// carriage return). A line typed sooner meets that cooked mode, which echoes it and turns its Return into a line
/// feed `PSReadLine` takes for Ctrl+J, so the line never runs. The output offset past that point.
fn wait_for_prompt(client: &mut Client, count: usize) -> usize {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let output = &client.output;
        let mark = (0..count).try_fold(0, |from, _| find_end(output, INPUT_MARK, from));
        let reading = if cfg!(unix) {
            mark.and_then(|mark| find_end(output, CURSOR_QUERY, mark))
        } else {
            mark
        };
        if let Some(reading) = reading {
            return reading;
        }
        assert!(
            Instant::now() < deadline && client.exit.is_none(),
            "no prompt {count} reading input: {:?}",
            String::from_utf8_lossy(output)
        );
        client.receive(Duration::from_millis(100));
    }
}

/// Type `line` at the shell's `count`th prompt and Return, once the line editor reads keys: its first character goes
/// first, and the rest only once `PSReadLine` echoed that one back (the terminal echoes nothing by then, and Windows'
/// console nothing still unread), so the line meets the line editor however the cursor report and the keys are
/// ordered. The Return is a carriage return, which `PSReadLine`'s raw input takes for the Enter key on every OS (a
/// line feed is Ctrl+J there).
fn type_at_prompt(pane: &Pane, client: &mut Client, count: usize, line: &str) -> TestResult {
    let reading = wait_for_prompt(client, count);
    let (probe, rest) = line.split_at(line.chars().next().map_or(0, char::len_utf8));
    pane.write(probe.as_bytes())?;
    let deadline = Instant::now() + PATIENCE;
    while find_end(&client.output, probe.as_bytes(), reading).is_none() {
        assert!(
            Instant::now() < deadline && client.exit.is_none(),
            "the line editor never echoed {probe:?}: {:?}",
            String::from_utf8_lossy(&client.output)
        );
        client.receive(Duration::from_millis(100));
    }
    pane.write(format!("{rest}\r").as_bytes())?;
    Ok(())
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

/// The program in the foreground of the shell `shell_pid` runs, read as a live pane's restore capture reads it: the
/// terminal's foreground group through the process table (Windows: the console's foreground), then
/// `ForegroundReader::command`. `None` while the shell holds its prompt, and on Unix while it runs a program in its
/// own group.
fn foreground_argv(table: &dyn ProcessTable, shell_pid: i32) -> Option<Vec<String>> {
    let group = table.terminal_foreground(shell_pid)?;
    ForegroundReader::new(table, None).command(group)
}

/// The argv to replay for `helper`, running at the prompt of the shell `shell_pid`, as the capture reads it: on
/// Windows the console's foreground, which must be the helper.
#[cfg(windows)]
fn captured_helper(table: &dyn ProcessTable, shell_pid: i32, helper: &[String]) -> Vec<String> {
    wait_for(
        "the helper in the foreground",
        || foreground_argv(table, shell_pid),
        |argv| argv.as_ref().is_some_and(|argv| argv.ends_with(&helper[1..])),
    )
    .unwrap_or_default()
}

/// The argv to replay for `helper`, running at the prompt of the shell `shell_pid`: on Unix the capture reads
/// nothing (the helper shares the shell's group, as a background program would), the `tree` and a restart see the
/// shell, so the replay is of the argv typed.
#[cfg(unix)]
fn captured_helper(table: &dyn ProcessTable, shell_pid: i32, helper: &[String]) -> Vec<String> {
    let group = table.terminal_foreground(shell_pid);
    assert_eq!(group, Some(shell_pid), "the helper runs in the shell's own group");
    let reader = ForegroundReader::new(table, None);
    assert_eq!(
        foreground_argv(table, shell_pid),
        None,
        "nothing is captured under PowerShell"
    );
    assert!(
        matches!(reader.running(shell_pid), Some(PaneForeground::ForegroundShell(_))),
        "the tree shows the shell"
    );
    assert!(
        matches!(reader.observed(shell_pid), Some((pid, PaneForeground::ForegroundShell(_))) if pid == shell_pid),
        "a restart sees the shell"
    );
    helper.to_vec()
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
fn a_live_powershell_reports_its_directory_reads_its_foreground_and_replays_a_program() -> TestResult {
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
    wait_for(
        "the title",
        || pane.with_terminal(|terminal| terminal.title()),
        |title| title.ends_with("work dir"),
    );
    // a command line is marked when it runs, and its status when the prompt is back
    type_at_prompt(&pane, &mut client, 1, &exit_three())?;
    expect_raw(&mut client, b"\x1b]133;C\x07");
    expect_raw(&mut client, b"\x1b]133;D;3\x07");

    // a program in the foreground, read through the process table once it runs
    let helper = helper_argv()?;
    type_at_prompt(&pane, &mut client, 2, &typed(&helper))?;
    wait_for("the helper's record", || record.exists(), |exists| *exists);
    let table = ProcTable::new();
    let foreground = captured_helper(&table, i32::try_from(pane.pid())?, &helper);
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

/// Not a test of its own: the program
/// [`a_live_powershell_reports_its_directory_reads_its_foreground_and_replays_a_program`] runs in its sessions, which
/// records its arguments after `--` as JSON in the file [`RECORD`] names (written beside it and renamed into place, so
/// it never shows up partly written), then holds the foreground until the `.stop` file beside it exists.
#[test]
#[ignore = "the PowerShell session test's child"]
fn foreground_helper() {
    let Some(record) = std::env::var_os(RECORD).map(PathBuf::from) else {
        return;
    };
    let argv: Vec<String> = std::env::args().skip_while(|arg| arg != "--").skip(1).collect();
    let partial = record.with_extension("partial");
    fs::write(&partial, serde_json::to_vec(&argv).unwrap()).unwrap();
    fs::rename(&partial, &record).unwrap();
    let stop = record.with_extension("stop");
    let deadline = Instant::now() + PATIENCE * 10;
    while !stop.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}
