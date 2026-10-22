//! A pane on a real PTY on every OS (openpty on Unix, the pseudoconsole on Windows), launched as a pane's command
//! line is (`/bin/sh -c`, or `cmd.exe /D /S /C "<line>"`): its output on screen, an echo round trip, a resize reaching
//! the child, exit codes, selection and search over what the child printed, and the password-input detection.
//!
//! The other test files drive the child with POSIX shell scripts and check the VT byte for byte, so they run on Unix
//! only: Windows' pseudoconsole renders the child's output itself and re-emits it, which they cannot pin.

#[expect(dead_code, reason = "the file helpers serve the other test files")]
mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ngterm_core::ghostty_config::GhosttyConfig;
use ngterm_term::{ExitStatus, Frame, GridPoint, GridSize, Host, Launch, Pane, PaneConfig, PaneEvent, SelectionText};
use support::{SIZE, TestResult};

/// How long a wait may take: Windows PowerShell can take seconds to start on a CI runner.
const PATIENCE: Duration = Duration::from_secs(60);

/// The command line for this OS: `unix` for `/bin/sh -c`, `windows` for `cmd.exe`.
fn line(unix: &str, windows: &str) -> String {
    if cfg!(windows) { windows } else { unix }.to_owned()
}

/// `command` launched as a pane's command line, isolated from any ngterm the tests run under.
fn launch(command: &str) -> TestResult<Pane> {
    let isolation = std::env::vars_os()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .filter(|name| name.starts_with("NGTERM_") || name.starts_with("AGTERM_") || name.starts_with("ZMX_"))
        .collect();
    let launch = Launch {
        command: Some(command.to_owned()),
        working_directory: Some(std::env::temp_dir()),
        env_remove: isolation,
        ..Launch::default()
    };
    let host = Host::new(&GhosttyConfig::default(), None, None, None);
    Ok(Pane::launch(&launch, &host, &PaneConfig::new(SIZE))?)
}

/// Poll the pane's frame until `done` holds; the last frame.
fn wait_frame(pane: &Pane, mut done: impl FnMut(&Frame) -> bool) -> Arc<Frame> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let frame = pane.frame().frame;
        if done(&frame) {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "timed out; the screen:\n{}",
            frame.lines().join("\n")
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The row and column where `needle` shows on screen, waited for.
fn wait_text(pane: &Pane, needle: &str) -> (u32, u16) {
    let frame = wait_frame(pane, |frame| frame.lines().iter().any(|line| line.contains(needle)));
    let lines = frame.lines();
    let (row, text) = lines
        .iter()
        .enumerate()
        .find(|(_, line)| line.contains(needle))
        .unwrap_or_else(|| unreachable!("waited for it"));
    let column = text[..text.find(needle).unwrap_or(0)].chars().count();
    (
        u32::try_from(row).unwrap_or(u32::MAX),
        u16::try_from(column).unwrap_or(u16::MAX),
    )
}

/// Collect the pane's events until `done` holds for them; all of them.
fn wait_events(pane: &Pane, mut done: impl FnMut(&[PaneEvent]) -> bool) -> Vec<PaneEvent> {
    let deadline = Instant::now() + PATIENCE;
    let mut events = Vec::new();
    loop {
        events.extend(pane.take_events());
        if done(&events) {
            return events;
        }
        assert!(Instant::now() < deadline, "timed out; events so far: {events:#?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The pane's exit, waited for.
fn exit_of(pane: &Pane) -> ExitStatus {
    let events = wait_events(pane, |events| {
        events.iter().any(|event| matches!(event, PaneEvent::Exited(_)))
    });
    events
        .into_iter()
        .find_map(|event| match event {
            PaneEvent::Exited(status) => Some(status),
            _ => None,
        })
        .unwrap_or_else(|| unreachable!("waited for it"))
}

/// A command that prints `text` and then keeps its pane for 30 s.
fn print_and_stay(text: &str) -> String {
    line(
        &format!("printf '%s' '{text}'; exec sleep 30"),
        &format!("echo {text}& ping -n 30 127.0.0.1 >NUL"),
    )
}

#[test]
fn a_command_line_runs_with_its_quotes_as_written() -> TestResult {
    // cmd.exe's echo prints the quotes it was given: only a command text passed as written keeps them
    let pane = launch(&line(
        "echo '\"quoted words\"'; exec sleep 30",
        "echo \"quoted words\"& ping -n 30 127.0.0.1 >NUL",
    ))?;
    wait_text(&pane, "\"quoted words\"");
    Ok(())
}

#[test]
fn typed_input_round_trips_through_the_child() -> TestResult {
    // the typed line reaches the child, which prints it back tagged (`call` expands the variable after `set /p`)
    let pane = launch(&line(
        "read line; echo \"got:$line\"; exec sleep 30",
        "set /p line=& call echo got:%line%& ping -n 30 127.0.0.1 >NUL",
    ))?;
    pane.write(b"ping\r")?;
    wait_text(&pane, "got:ping");
    Ok(())
}

#[test]
fn a_resize_reaches_the_child() -> TestResult {
    let pane = launch(&line(
        "read _; echo \"SIZE=$(stty size | tr ' ' x)\"; exec sleep 30",
        "set /p _=& powershell.exe -NoLogo -NoProfile -NonInteractive -Command \"'SIZE=' + [Console]::WindowHeight + \
         'x' + [Console]::WindowWidth\"& ping -n 30 127.0.0.1 >NUL",
    ))?;
    pane.resize(GridSize {
        cols: 100,
        rows: 30,
        ..SIZE
    })?;
    pane.write(b"\r")?;
    wait_text(&pane, "SIZE=30x100");
    Ok(())
}

#[test]
fn the_exit_code_is_the_programs() -> TestResult {
    assert_eq!(exit_of(&launch("exit 7")?).code, 7);
    assert_eq!(exit_of(&launch("exit 0")?).code, 0);
    // a command that is not found: the shell's own status for it (127; `cmd /C` ends with 1)
    let missing = exit_of(&launch("ngterm-no-such-command-xyz")?);
    assert_eq!(missing.code, if cfg!(windows) { 1 } else { 127 });
    Ok(())
}

#[test]
fn what_the_child_printed_selects_and_searches() -> TestResult {
    let pane = launch(&print_and_stay("alpha needle omega"))?;
    let (row, column) = wait_text(&pane, "needle");
    assert!(pane.select_word(GridPoint::viewport(column + 1, row))?);
    assert_eq!(pane.selection_text(SelectionText::Raw).as_deref(), Some("needle"));
    assert!(pane.select_line(GridPoint::viewport(column, row))?);
    assert_eq!(
        pane.selection_text(SelectionText::Copy).as_deref(),
        Some("alpha needle omega")
    );
    assert!(pane.search("needle"));
    wait_events(&pane, |events| {
        events
            .iter()
            .any(|event| matches!(event, PaneEvent::SearchTotal(Some(1))))
    });
    assert_eq!(pane.search_state().total, Some(1));
    Ok(())
}

/// Unix: a canonical read with echo off is a password prompt, and the shell's process group is the terminal's
/// foreground. Windows says neither: the pseudoconsole gives its parent no handle on the console's input mode, and has
/// no foreground process group.
#[test]
fn a_read_without_echo_is_password_input_where_the_pty_can_say() -> TestResult {
    let pane = launch(&line(
        "printf ready; stty -echo; read secret; exec sleep 30",
        "echo ready& set /p secret=& ping -n 30 127.0.0.1 >NUL",
    ))?;
    wait_text(&pane, "ready");
    if cfg!(windows) {
        std::thread::sleep(Duration::from_millis(500));
        assert!(!pane.password_input());
        assert_eq!(pane.foreground_pid(), None);
    } else {
        let deadline = Instant::now() + PATIENCE;
        while !pane.password_input() {
            assert!(Instant::now() < deadline, "the read without echo was never seen");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(pane.foreground_pid().is_some());
    }
    Ok(())
}
