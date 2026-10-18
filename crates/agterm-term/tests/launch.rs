//! Launching panes as libghostty launched a surface's program, through a real PTY: the terminal identity reaching the
//! shell, a plain pane's command, typed input, exit hold, working directory, environment and shell integration, the
//! foreground process group, and live panes through the real patched zmx when `AGTERM_TEST_ZMX` names it
//! (`$(scripts/build-zmx.sh --print-prefix)/bin/zmx`), each test under its own state directory, so its own `ZMX_DIR`,
//! reaped by that directory alone.
//!
//! `AGTERM_TEST_LIVE_LAUNCHES` sets how many fresh live launches must all paint their prompt (default 50, then as
//! many again in parallel).

#[expect(dead_code, reason = "the file helpers serve the other test files")]
mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agterm_core::ghostty_config::GhosttyConfig;
use agterm_core::live::support::{Configuration, attach_command, settled};
use agterm_core::paths::MapEnv;
use agterm_core::restore::mode::LiveRejection;
use agterm_core::restore::spawn_plan::LaunchSeed;
use agterm_host::{KillOutcome, SessionHost};
use agterm_term::{ExitStatus, Frame, Host, Launch, Pane, PaneConfig, PaneEvent, TextRegion};
use support::{DEADLINE, SIZE, TestResult, wait_events, wait_frame, wait_text};
use uuid::Uuid;

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

#[test]
fn the_foreground_pid_is_the_terminals_foreground_group() -> TestResult {
    let pane = Pane::launch(&launch(None), &host("/bin/sh", None), &PaneConfig::new(SIZE))?;
    let shell = pane.pid().expect("a child pid");
    // the shell holds the foreground while it waits at its prompt
    let deadline = Instant::now() + DEADLINE;
    while pane.foreground_pid() != Some(shell) {
        assert!(Instant::now() < deadline, "the shell never held the foreground");
        std::thread::sleep(Duration::from_millis(10));
    }
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
    // empty), so wait for the `exec` to land
    loop {
        let cmdline = std::fs::read(format!("/proc/{job}/cmdline"))?;
        if cmdline == b"sleep\x0030\x00" {
            break;
        }
        assert!(Instant::now() < deadline, "the job never ran `sleep 30`: {cmdline:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

// -- live panes through the real zmx

/// A live launch's inputs under a private state directory (so a private `ZMX_DIR`): bash as the login shell, whose
/// integration sets the prompt `live-ready$ `.
struct Live {
    runtime: agterm_zmx::Runtime,
    host: Host,
    home: tempfile::TempDir,
    _state: tempfile::TempDir,
    _resources: tempfile::TempDir,
    socket_directory: String,
}

impl Live {
    fn new(zmx: &Path) -> TestResult<Self> {
        let state = tempfile::tempdir()?;
        let home = tempfile::tempdir()?;
        // no terminfo beside them: the shell sees xterm-256color, which every system describes
        let (resources, ghostty) = resources("PS1='live-ready$ '\n", false)?;
        let env = MapEnv::new()
            .with("AGTERM_STATE_DIR", state.path())
            .with(agterm_zmx::launch::ZMX_PATH_OVERRIDE, zmx)
            .with("GHOSTTY_RESOURCES_DIR", &ghostty)
            .with("HOME", home.path());
        let runtime = agterm_zmx::Runtime::resolve(&env, None, Some("/bin/bash".to_owned()), true);
        let host = Host::from_runtime(&GhosttyConfig::default(), &runtime, None);
        let socket_directory = agterm_core::live::socket_directory(&runtime.state_dir);
        Ok(Self {
            runtime,
            host,
            home,
            _state: state,
            _resources: resources,
            socket_directory,
        })
    }

    /// A fresh pane's wrap configuration.
    fn configuration(&self) -> TestResult<Configuration> {
        let configuration = self.runtime.configuration(Uuid::new_v4(), BTreeMap::new(), None);
        Ok(configuration.map_err(|rejection| format!("the host cannot wrap a pane: {rejection:?}"))?)
    }

    /// The launch of `configuration`'s attach, as `resolve_live_seed` builds it.
    fn launch(&self, configuration: &Configuration) -> Launch {
        let seed = LaunchSeed {
            command: Some(settled(&attach_command(configuration, None, None, &[] as &[&str]))),
            env: configuration.environment.clone().into_iter().collect(),
            live: true,
            ..LaunchSeed::default()
        };
        let home = self.home.path().display().to_string();
        let mut launch = Launch::from_seed(
            &seed,
            Some(self.home.path().to_path_buf()),
            &[("HOME".to_owned(), home)],
        );
        launch.env_remove = isolation();
        launch
    }

    fn pane(&self, configuration: &Configuration) -> TestResult<Pane> {
        Ok(Pane::launch(
            &self.launch(configuration),
            &self.host,
            &PaneConfig::new(SIZE),
        )?)
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        reap(&self.socket_directory);
        let _ = std::fs::remove_dir_all(&self.socket_directory);
    }
}

/// The pids of every process whose environment carries `ZMX_DIR=<dir>`.
fn processes_in(dir: &str) -> Vec<i32> {
    let marker = format!("ZMX_DIR={dir}");
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|environ| environ.split(|byte| *byte == 0).any(|var| var == marker.as_bytes()))
        })
        .collect()
}

/// Kill every process of this test's `ZMX_DIR`, and only those, until none is left.
fn reap(dir: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let left = processes_in(dir);
        if left.is_empty() {
            return;
        }
        for pid in left {
            // SAFETY: plain kill(2) of a process that carries this test's own ZMX_DIR.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(
            Instant::now() <= deadline,
            "processes left under ZMX_DIR={dir}: {:?}",
            processes_in(dir)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The zmx under test, or `None` (logged) when `AGTERM_TEST_ZMX` does not name one.
fn test_zmx() -> Option<PathBuf> {
    let zmx = std::env::var_os("AGTERM_TEST_ZMX").map(PathBuf::from);
    if zmx.is_none() {
        eprintln!("AGTERM_TEST_ZMX is unset: skipping the real zmx test");
    }
    zmx
}

fn prompt_painted(frame: &Frame) -> bool {
    frame.lines().iter().any(|line| line.contains("live-ready$"))
}

/// Launch a fresh live pane and wait for its prompt; the time it took.
fn paint_once(live: &Live) -> TestResult<Duration> {
    let started = Instant::now();
    let pane = live.pane(&live.configuration()?)?;
    wait_frame(&pane, prompt_painted);
    Ok(started.elapsed())
}

#[test]
fn every_fresh_live_launch_paints_its_prompt() -> TestResult {
    let Some(zmx) = test_zmx() else {
        return Ok(());
    };
    let launches: usize = std::env::var("AGTERM_TEST_LIVE_LAUNCHES")
        .ok()
        .and_then(|count| count.parse().ok())
        .unwrap_or(50);
    let live = Arc::new(Live::new(&zmx)?);
    // one after another: each a new daemon, its prompt printed before its first client connected
    for launch in 0..launches {
        let took = paint_once(&live).map_err(|err| format!("launch {launch}: {err}"))?;
        eprintln!("live launch {launch}: prompt in {} ms", took.as_millis());
    }
    // as many again, six at a time
    let workers: Vec<_> = (0..6)
        .map(|worker| {
            let live = Arc::clone(&live);
            std::thread::spawn(move || -> Result<(), String> {
                for launch in 0..launches.div_ceil(6) {
                    paint_once(&live).map_err(|err| format!("worker {worker} launch {launch}: {err}"))?;
                }
                Ok(())
            })
        })
        .collect();
    for worker in workers {
        worker.join().map_err(|_| "a worker panicked")??;
    }
    Ok(())
}

#[test]
fn a_live_pane_ends_when_its_session_is_gone() -> TestResult {
    let Some(zmx) = test_zmx() else {
        return Ok(());
    };
    let live = Live::new(&zmx)?;
    let configuration = live.configuration()?;
    let pane = live.pane(&configuration)?;
    wait_frame(&pane, prompt_painted);
    // the daemon's shell sees the pane's environment and the wrap's
    pane.write(b"echo \"pane=$AGTERM_PANE_ID term=$TERM\"\r")?;
    wait_text(&pane, &format!("pane={} term=xterm-256color", configuration.pane_id));
    assert_eq!(
        live.runtime.client().kill(&configuration.daemon_name, true),
        KillOutcome::Killed
    );
    // the attach's end closes the pane: a live pane's attach is never waited for
    let status = exit_of(&pane);
    assert!(!status.held, "{status:?}");
    Ok(())
}

#[test]
fn a_live_attach_that_cannot_reach_its_socket_directory_ends_its_pane() -> TestResult {
    let Some(zmx) = test_zmx() else {
        return Ok(());
    };
    let live = Live::new(&zmx)?;
    let blocked = tempfile::NamedTempFile::new()?;
    let mut launch = live.launch(&live.configuration()?);
    // a regular file where the socket directory should be
    launch
        .env
        .push(("ZMX_DIR".to_owned(), blocked.path().display().to_string()));
    let pane = Pane::launch(&launch, &live.host, &PaneConfig::new(SIZE))?;
    let status = exit_of(&pane);
    assert!(status.code != 0 && !status.held, "{status:?}");
    assert!(processes_in(&blocked.path().display().to_string()).is_empty());
    Ok(())
}

#[test]
fn a_missing_zmx_cannot_wrap_and_its_attach_ends_the_pane() -> TestResult {
    let state = tempfile::tempdir()?;
    let (_resources, ghostty) = resources("", false)?;
    let env = MapEnv::new()
        .with("AGTERM_STATE_DIR", state.path())
        .with(agterm_zmx::launch::ZMX_PATH_OVERRIDE, "/nonexistent/agterm-test/zmx")
        .with("GHOSTTY_RESOURCES_DIR", &ghostty);
    let runtime = agterm_zmx::Runtime::resolve(&env, None, Some("/bin/bash".to_owned()), true);
    // the launch probe rejects the host, so the pane falls back to a plain launch
    assert_eq!(
        runtime.configuration(Uuid::new_v4(), BTreeMap::new(), None),
        Err(LiveRejection::ExecutableUnavailable)
    );
    // a seed made while zmx was there, run once it is gone: the attach fails and the pane ends
    let pane = Pane::launch(
        &launch(Some("exec '/nonexistent/agterm-test/zmx' attach agterm-gone")),
        &Host::from_runtime(&GhosttyConfig::default(), &runtime, None),
        &PaneConfig::new(SIZE),
    )?;
    let status = exit_of(&pane);
    assert_eq!((status.code, status.held), (127, false));
    Ok(())
}
