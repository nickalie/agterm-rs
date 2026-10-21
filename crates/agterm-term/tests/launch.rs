//! Launching panes as libghostty launched a surface's program, through a real PTY: the terminal identity reaching the
//! shell, a plain pane's command, typed input, exit hold, working directory, environment and shell integration, and
//! the foreground process group. Live panes, which attach in process, are `live.rs`'s.
//!
//! Unix only: the children are POSIX shell scripts whose output the tests pin byte for byte, which Windows'
//! pseudoconsole re-renders; `platform.rs` runs a pane's launch, echo, resize, exit, selection and search there.
#![cfg(unix)]

#[expect(dead_code, reason = "the file helpers serve the other test files")]
mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agterm_core::ghostty_config::GhosttyConfig;
use agterm_term::{ExitStatus, Host, Launch, Pane, PaneConfig, PaneEvent, TextRegion};
use support::{DEADLINE, SIZE, TestResult, wait_events, wait_text};

/// The names of the inherited `AGTERM_*`/`ZMX_*` variables, which no test pane may see.
fn isolation() -> Vec<String> {
    std::env::vars_os()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .filter(|name| name.starts_with("AGTERM_") || name.starts_with("ZMX_"))
        .collect()
}

/// A launch of `command` (the login shell when `None`), isolated from any agterm the tests run under.
fn launch(command: Option<&str>) -> Launch {
    Launch {
        command: command.map(str::to_owned),
        env_remove: isolation(),
        ..Launch::default()
    }
}

/// Ghostty's resources under a fresh root: `share/ghostty/shell-integration/bash/ghostty.bash` holding `bash`, and,
/// with `terminfo`, the compiled entry beside them (`share/terminfo/x/xterm-ghostty`).
fn resources(bash: &str, terminfo: bool) -> TestResult<(tempfile::TempDir, PathBuf)> {
    let root = tempfile::tempdir()?;
    let ghostty = root.path().join("share/ghostty");
    let integration = ghostty.join("shell-integration/bash");
    std::fs::create_dir_all(&integration)?;
    std::fs::write(integration.join("ghostty.bash"), bash)?;
    if terminfo {
        let entry = root.path().join("share/terminfo/x");
        std::fs::create_dir_all(&entry)?;
        std::fs::write(entry.join("xterm-ghostty"), "")?;
    }
    Ok((root, ghostty))
}

/// A host running `shell` with `resources`, the default config, agterm's executable in `/opt/agterm/bin`.
fn host(shell: &str, resources: Option<PathBuf>) -> Host {
    Host::new(
        &GhosttyConfig::default(),
        Some(shell.to_owned()),
        resources,
        Some(PathBuf::from("/opt/agterm/bin")),
    )
}

fn exited(events: &[PaneEvent]) -> bool {
    events.iter().any(|event| matches!(event, PaneEvent::Exited(_)))
}

/// The pane's exit, waited for.
fn exit_of(pane: &Pane) -> ExitStatus {
    let events = wait_events(pane, exited);
    let Some(PaneEvent::Exited(status)) = events.into_iter().rfind(|event| matches!(event, PaneEvent::Exited(_)))
    else {
        unreachable!("waited for an exit")
    };
    status
}

/// The whole screen's text, scrollback included.
fn screen(pane: &Pane) -> String {
    pane.read_text(TextRegion::All).unwrap_or_default()
}

#[test]
fn the_terminal_identity_reaches_the_shell() -> TestResult {
    let (_root, ghostty) = resources("", true)?;
    let pane = Pane::launch(
        &Launch {
            wait_after_command: true,
            ..launch(Some(
                "env | grep -E '^(TERM|TERMINFO|COLORTERM|TERM_PROGRAM|GHOSTTY_RESOURCES_DIR|GHOSTTY_SHELL_FEATURES)=' | sort",
            ))
        },
        &host("/bin/sh", Some(ghostty.clone())),
        &PaneConfig::new(SIZE),
    )?;
    assert!(exit_of(&pane).held);
    let text = screen(&pane);
    let terminfo = ghostty.parent().unwrap().join("terminfo");
    for line in [
        "COLORTERM=truecolor".to_owned(),
        format!("GHOSTTY_RESOURCES_DIR={}", ghostty.display()),
        "GHOSTTY_SHELL_FEATURES=cursor:blink,path,title".to_owned(),
        "TERM=xterm-ghostty".to_owned(),
        format!("TERMINFO={}", terminfo.display()),
        "TERM_PROGRAM=ghostty".to_owned(),
    ] {
        assert!(text.lines().any(|seen| seen == line), "{line} missing from:\n{text}");
    }
    Ok(())
}

#[test]
fn without_its_terminfo_the_terminal_claims_xterm_256color() -> TestResult {
    // shell integration alone is not enough: the terminfo must be beside it
    let (_root, ghostty) = resources("", false)?;
    let resolved = agterm_term::resolve_resources(std::slice::from_ref(&ghostty), Path::exists);
    assert_eq!(resolved, None);
    let pane = Pane::launch(
        &launch(Some(
            "echo \"term=$TERM terminfo=${TERMINFO-unset} colorterm=$COLORTERM resources=${GHOSTTY_RESOURCES_DIR-unset}\"; sleep 30",
        )),
        &host("/bin/sh", resolved),
        &PaneConfig::new(SIZE),
    )?;
    wait_text(
        &pane,
        "term=xterm-256color terminfo=unset colorterm=truecolor resources=unset",
    );
    Ok(())
}

#[test]
fn the_terminfo_counts_in_the_hashed_layout_macos_tic_writes() -> TestResult {
    // tic on macOS and Windows writes `78/xterm-ghostty` (the hex code of `x`) where ncurses on Linux writes `x/`
    let (root, ghostty) = resources("", false)?;
    let hashed = root.path().join("share/terminfo/78");
    std::fs::create_dir_all(&hashed)?;
    std::fs::write(hashed.join("xterm-ghostty"), "")?;
    let resolved = agterm_term::resolve_resources(std::slice::from_ref(&ghostty), Path::exists);
    assert_eq!(resolved, Some(ghostty));
    Ok(())
}

#[test]
fn a_plain_pane_runs_its_command_in_its_directory_with_its_environment() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut launch = launch(Some("pwd; echo \"var=$AGTERM_TEST_VAR pwd=$PWD\""));
    launch.working_directory = Some(dir.path().to_path_buf());
    launch
        .env
        .push(("AGTERM_TEST_VAR".to_owned(), "from-the-pane".to_owned()));
    launch.wait_after_command = true;
    let pane = Pane::launch(&launch, &host("/bin/sh", None), &PaneConfig::new(SIZE))?;
    let status = exit_of(&pane);
    assert_eq!((status.code, status.held), (0, true));
    let text = screen(&pane);
    let path = dir.path().display().to_string();
    assert!(text.lines().any(|line| line == path), "{text}");
    assert!(text.contains(&format!("var=from-the-pane pwd={path}")), "{text}");
    // held: the prompt libghostty wrote, the cursor hidden for it
    assert!(
        text.contains("Process exited. Press any key to close the terminal."),
        "{text}"
    );
    assert!(!pane.frame().frame.cursor.visible);
    Ok(())
}

#[test]
fn the_exit_message_shows_after_a_program_cut_off_mid_sequence() -> TestResult {
    // libghostty wrote the message on the terminal directly; through the parser it must first end the sequence the
    // program left unfinished (a CSI would take the message's `P` as its final byte, a string would swallow it all)
    for (cut, rest) in [
        ("printf 'before \\033[3'", "before"),
        ("printf 'before \\033]2;unfinished'", "before"),
        ("printf 'before \\033P$q'", "before"),
        ("printf 'before \\342'", "before"),
    ] {
        let mut waited = launch(Some(&format!("{cut}; sleep 0.3")));
        waited.wait_after_command = true;
        let pane = Pane::launch(&waited, &host("/bin/sh", None), &PaneConfig::new(SIZE))?;
        let status = exit_of(&pane);
        assert_eq!((status.code, status.held), (0, true), "{cut}");
        let text = screen(&pane);
        assert!(text.contains(rest), "{cut}: {text}");
        assert!(
            text.contains("Process exited. Press any key to close the terminal."),
            "{cut}: {text}"
        );
    }
    Ok(())
}

#[test]
fn a_command_not_waited_for_ends_its_pane() -> TestResult {
    let pane = Pane::launch(
        &launch(Some("sleep 0.3; exit 4")),
        &host("/bin/sh", None),
        &PaneConfig::new(SIZE),
    )?;
    let status = exit_of(&pane);
    assert_eq!((status.code, status.held), (4, false));
    assert!(!screen(&pane).contains("Process exited"));
    Ok(())
}

#[test]
fn initial_input_is_typed_into_the_login_shell() -> TestResult {
    let pane = Pane::launch(
        &Launch {
            initial_input: Some("echo typed-$((40+2))\n".to_owned()),
            ..launch(None)
        },
        &host("/bin/sh", None),
        &PaneConfig::new(SIZE),
    )?;
    wait_text(&pane, "typed-42");
    pane.write(b"exit 0\n")?;
    let status = exit_of(&pane);
    assert_eq!((status.code, status.held), (0, false));
    Ok(())
}

#[test]
fn a_command_that_is_not_found_holds_on_the_failure_report() -> TestResult {
    let mut waited = launch(Some("agterm-no-such-command-xyz"));
    waited.wait_after_command = true;
    let pane = Pane::launch(&waited, &host("/bin/sh", None), &PaneConfig::new(SIZE))?;
    let status = exit_of(&pane);
    assert_eq!((status.code, status.held), (127, true));
    let text = screen(&pane);
    for line in [
        "Ghostty failed to launch the requested command:",
        "/bin/sh -c agterm-no-such-command-xyz",
        "Exit Code: 127",
        "Press any key to close the window.",
    ] {
        assert!(text.contains(line), "{line} missing from:\n{text}");
    }
    // the shell's own complaint stays above a rule
    assert!(text.contains('\u{2501}'), "{text}");
    // not waited for, it just ends
    let pane = Pane::launch(
        &launch(Some("agterm-no-such-command-xyz")),
        &host("/bin/sh", None),
        &PaneConfig::new(SIZE),
    )?;
    let status = exit_of(&pane);
    assert_eq!((status.code, status.held), (127, false));
    assert!(!screen(&pane).contains("Ghostty failed"));
    // a login shell that fails right away holds on the report too
    let pane = Pane::launch(&launch(None), &host("false", None), &PaneConfig::new(SIZE))?;
    let status = exit_of(&pane);
    assert_eq!((status.code, status.held), (1, true));
    wait_text(&pane, "/bin/sh -c false");
    Ok(())
}

#[test]
fn the_login_shell_starts_with_its_integration() -> TestResult {
    if !Path::new("/bin/bash").exists() {
        eprintln!("no /bin/bash: skipping the integration launch");
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        // Apple's /bin/bash skips the ENV startup, so it gets no integration, as in Ghostty
        eprintln!("macOS's /bin/bash takes no integration: skipping the integration launch");
        return Ok(());
    }
    // the integration bash reads through ENV in POSIX mode, standing in for Ghostty's
    let (_root, ghostty) = resources(
        "case :$SHELLOPTS: in *:posix:*) mode=posix ;; *) mode=bash ;; esac\n\
         echo \"integrated inject=$GHOSTTY_BASH_INJECT mode=$mode\"; PS1='integrated$ '\n",
        true,
    )?;
    let home = tempfile::tempdir()?;
    let mut launch = launch(None);
    launch.env.push(("HOME".to_owned(), home.path().display().to_string()));
    let pane = Pane::launch(&launch, &host("/bin/bash", Some(ghostty)), &PaneConfig::new(SIZE))?;
    wait_text(&pane, "integrated inject=1 mode=posix");
    wait_text(&pane, "integrated$");
    Ok(())
}

/// The command line of process `pid`, empty once it is gone.
#[cfg(target_os = "linux")]
fn cmdline(pid: u32) -> Vec<u8> {
    std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default()
}

/// The parent of process `pid`, `None` once it is gone (the fourth field of its `stat`, past the parenthesized name).
#[cfg(target_os = "linux")]
fn parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
}

/// The interactive `/bin/sh` a pane of the login shell `/bin/sh` runs through its `/bin/sh -c /bin/sh` wrapper: the
/// pane's own process once the wrapper execs it (bash), else the wrapper's child (dash, the `/bin/sh` of Debian and
/// Ubuntu, stays as the wrapper); `None` until it runs.
#[cfg(target_os = "linux")]
fn interactive_sh(pane: &Pane) -> Option<u32> {
    let pid = pane.pid()?;
    if cmdline(pid) == b"/bin/sh\0" {
        return Some(pid);
    }
    std::fs::read_dir("/proc")
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .find(|child| parent(*child) == Some(pid) && cmdline(*child) == b"/bin/sh\0")
}

/// The interactive `/bin/sh` of a pane of the login shell `/bin/sh`: the pane's own process, macOS's `/bin/sh`
/// (bash in POSIX mode) execing its wrapper's command; `None` until it runs.
#[cfg(not(target_os = "linux"))]
fn interactive_sh(pane: &Pane) -> Option<u32> {
    pane.pid()
}

#[test]
fn the_foreground_pid_is_the_terminals_foreground_group() -> TestResult {
    let pane = Pane::launch(&launch(None), &host("/bin/sh", None), &PaneConfig::new(SIZE))?;
    // the shell holds the foreground while it waits at its prompt (not the wrapper, which holds it until the shell
    // takes it)
    let deadline = Instant::now() + DEADLINE;
    let shell = loop {
        if let Some(shell) = interactive_sh(&pane).filter(|shell| pane.foreground_pid() == Some(*shell)) {
            break shell;
        }
        assert!(Instant::now() < deadline, "the shell never held the foreground");
        std::thread::sleep(Duration::from_millis(10));
    };
    // a job it runs takes it over
    pane.write(b"sleep 30\n")?;
    let job = loop {
        if let Some(pid) = pane.foreground_pid().filter(|pid| *pid != shell) {
            break pid;
        }
        assert!(Instant::now() < deadline, "the job never took the foreground");
        std::thread::sleep(Duration::from_millis(10));
    };
    // the job's process group leads before its `exec`: until then its command line is the forked shell's (or
    // empty), so wait for the `exec` to land (read from `/proc`, Linux's)
    #[cfg(target_os = "linux")]
    loop {
        let cmdline = std::fs::read(format!("/proc/{job}/cmdline"))?;
        if cmdline == b"sleep\x0030\x00" {
            break;
        }
        assert!(Instant::now() < deadline, "the job never ran `sleep 30`: {cmdline:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = job;
    Ok(())
}
