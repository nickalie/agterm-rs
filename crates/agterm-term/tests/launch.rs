//! Launching panes as libghostty launched a surface's program, through a real PTY: the terminal identity reaching the
//! shell, a plain pane's command, typed input, exit hold, working directory, environment and shell integration, the
//! foreground process group, and live panes through the cargo-built session server (`agterm-session attach` in the
//! pane's PTY, as the frontend runs it), each test under its own state directory, so its own namespace and server,
//! reaped through that server alone.
//!
//! `AGTERM_TEST_LIVE_LAUNCHES` sets how many fresh live launches must all paint their prompt (default 50, then as
//! many again in parallel).
//!
//! Unix only: the children are POSIX shell scripts whose output the tests pin byte for byte, which Windows'
//! pseudoconsole re-renders; `platform.rs` runs a pane's launch, echo, resize, exit, selection and search there.
#![cfg(unix)]

#[expect(dead_code, reason = "the file helpers serve the other test files")]
mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agterm_core::ghostty_config::GhosttyConfig;
use agterm_core::paths::MapEnv;
use agterm_core::restore::mode::LiveRejection;
use agterm_term::{ExitStatus, Host, Launch, Pane, PaneConfig, PaneEvent, TextRegion};
use support::{DEADLINE, SIZE, TestResult, wait_events, wait_text};
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

// -- live panes through the cargo-built session server
//
// Linux only: the session's bash is a login shell, and macOS's startup files (`/etc/profile`, `/etc/bashrc`) set a
// prompt of their own over the integration's; Windows runs PowerShell. The in-process attach's tests (Task 26 of
// docs/plans/20261018-cross-platform-sessions.md) run on all three.

#[cfg(target_os = "linux")]
mod live {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use agterm_core::ghostty_config::GhosttyConfig;
    use agterm_core::live::support::{Configuration, attach_command, settled};
    use agterm_core::paths::MapEnv;
    use agterm_core::restore::spawn_plan::LaunchSeed;
    use agterm_host::{KillOutcome, SessionHost};
    use agterm_session::host::ServerHost;
    use agterm_term::{ClipboardAnswer, Frame, Host, Launch, Pane, PaneConfig, PaneEvent, Rgb};
    use uuid::Uuid;

    use super::support::{DEADLINE, SIZE, TestResult, wait_events, wait_frame, wait_text};
    use super::{exit_of, isolation, resources};

    /// The cargo-built `agterm-session` of this test's profile (`target/<profile>/`, above the test binary's `deps/`),
    /// which `cargo test` builds beside this one with the workspace, or with `-p agterm-session`.
    fn session_executable() -> TestResult<PathBuf> {
        let test = std::env::current_exe()?;
        let executable = test
            .parent()
            .and_then(Path::parent)
            .ok_or("no target profile directory above the test binary")?
            .join(agterm_session::launch::EXECUTABLE_FILE);
        if !executable.is_file() {
            return Err(format!(
                "no cargo-built {} (build it with `cargo build -p agterm-session`, or test with the workspace)",
                executable.display()
            )
            .into());
        }
        Ok(executable)
    }

    /// A live launch's inputs under a private state directory (so a private namespace, its own server): bash as the
    /// login shell, whose integration sets the prompt `live-ready$ `.
    struct Live {
        runtime: agterm_session::launch::Runtime,
        host: Host,
        home: tempfile::TempDir,
        _state: tempfile::TempDir,
        _resources: tempfile::TempDir,
        socket_directory: String,
    }

    impl Live {
        fn new() -> TestResult<Self> {
            let executable = session_executable()?;
            let state = tempfile::tempdir()?;
            let home = tempfile::tempdir()?;
            // no terminfo beside them: the shell sees xterm-256color, which every system describes
            let (resources, ghostty) = resources("PS1='live-ready$ '\n", false)?;
            let env = MapEnv::new()
                .with("AGTERM_STATE_DIR", state.path())
                .with(agterm_session::launch::EXECUTABLE_OVERRIDE, &executable)
                .with("GHOSTTY_RESOURCES_DIR", &ghostty)
                .with("HOME", home.path());
            let runtime = agterm_session::launch::Runtime::resolve(&env, None, Some("/bin/bash".to_owned()), true);
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

        /// The `SessionHost` over this test's namespace.
        fn session_host(&self) -> ServerHost {
            ServerHost::new(self.runtime.executable.clone(), &self.socket_directory)
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
            self.pane_with(configuration, &PaneConfig::new(SIZE))
        }

        fn pane_with(&self, configuration: &Configuration, config: &PaneConfig) -> TestResult<Pane> {
            Ok(Pane::launch(&self.launch(configuration), &self.host, config)?)
        }
    }

    impl Drop for Live {
        fn drop(&mut self) {
            reap(&self.session_host(), &self.socket_directory);
            let _ = std::fs::remove_dir_all(&self.socket_directory);
        }
    }

    /// The pids of this namespace's processes this machine shows: its server (`agterm-session server <dir>`) and every
    /// process whose environment carries `AGTERM_LIVE_DIR=<dir>` (the sessions' programs and their attach clients). Linux
    /// reads them from `/proc`; elsewhere none are seen, and the server's own listing stands alone.
    fn processes_in(dir: &str) -> Vec<i32> {
        #[cfg(target_os = "linux")]
        {
            let marker = format!("{}={dir}", agterm_core::live::DIRECTORY_VARIABLE);
            let Ok(entries) = std::fs::read_dir("/proc") else {
                return Vec::new();
            };
            let words = |pid: i32, file: &str| -> Vec<Vec<u8>> {
                std::fs::read(format!("/proc/{pid}/{file}"))
                    .map(|bytes| bytes.split(|byte| *byte == 0).map(<[u8]>::to_vec).collect())
                    .unwrap_or_default()
            };
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
                .filter(|pid| {
                    let argv = words(*pid, "cmdline");
                    let server = argv.len() >= 3
                        && argv[0].ends_with(b"/agterm-session")
                        && argv[1] == b"server"
                        && argv[2] == dir.as_bytes();
                    server || words(*pid, "environ").iter().any(|var| var == marker.as_bytes())
                })
                .collect()
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = dir;
            Vec::new()
        }
    }

    /// End this test's namespace through its own server, and only it: every session it lists killed, then the server's
    /// own exit once idle (`agterm_session::server::lifecycle::IDLE_GRACE` after its last client, so nothing here asks it
    /// anything meanwhile) and every process of the namespace gone (Linux); one that outlives the deadline is killed by
    /// its pid, which names this namespace alone, and fails the test.
    fn reap(host: &ServerHost, dir: &str) {
        let listed = host.list();
        if let Ok(records) = &listed {
            let names: Vec<String> = records.iter().map(|record| record.name.clone()).collect();
            if !names.is_empty() {
                host.kill_batch(&names);
            }
        }
        let deadline = Instant::now() + DEADLINE;
        loop {
            let left = processes_in(dir);
            if left.is_empty() {
                return;
            }
            if Instant::now() > deadline {
                let described: Vec<String> = left
                    .iter()
                    .map(|pid| {
                        let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                        format!("{pid} {}", String::from_utf8_lossy(&argv).replace('\0', " "))
                    })
                    .collect();
                for pid in &left {
                    // SAFETY: plain kill(2) of a process of this test's own namespace.
                    unsafe { libc::kill(*pid, libc::SIGKILL) };
                }
                let message = format!(
                    "left in the namespace {dir}: listing {:?}, processes {described:?}",
                    listed.map_err(|error| error.to_string())
                );
                // a test that already failed keeps its own report
                assert!(std::thread::panicking(), "{message}");
                eprintln!("{message}");
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
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
        let launches: usize = std::env::var("AGTERM_TEST_LIVE_LAUNCHES")
            .ok()
            .and_then(|count| count.parse().ok())
            .unwrap_or(50);
        let live = Arc::new(Live::new()?);
        // one after another: each a new session, its prompt printed before its first client was sent its snapshot
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
        let live = Live::new()?;
        let configuration = live.configuration()?;
        let pane = live.pane(&configuration)?;
        wait_frame(&pane, prompt_painted);
        // the session's shell sees the pane's environment, the wrap's and the server's
        pane.write(b"echo \"pane=$AGTERM_PANE_ID term=$TERM\"; echo \"session=$AGTERM_LIVE_SESSION\"\r")?;
        wait_text(&pane, &format!("pane={} term=xterm-256color", configuration.pane_id));
        wait_text(&pane, &format!("session={}", configuration.daemon_name));
        assert_eq!(
            live.session_host().kill(&configuration.daemon_name, true),
            KillOutcome::Killed
        );
        // the attach's end closes the pane: a live pane's attach is never waited for
        let status = exit_of(&pane);
        assert!(!status.held, "{status:?}");
        Ok(())
    }

    #[test]
    fn a_live_attach_that_cannot_reach_its_socket_directory_ends_its_pane() -> TestResult {
        let live = Live::new()?;
        let blocked = tempfile::NamedTempFile::new()?;
        let mut launch = live.launch(&live.configuration()?);
        // a regular file where the socket directory should be
        launch.env.push((
            agterm_core::live::DIRECTORY_VARIABLE.to_owned(),
            blocked.path().display().to_string(),
        ));
        let pane = Pane::launch(&launch, &live.host, &PaneConfig::new(SIZE))?;
        let status = exit_of(&pane);
        assert!(status.code != 0 && !status.held, "{status:?}");
        assert!(processes_in(&blocked.path().display().to_string()).is_empty());
        Ok(())
    }

    /// Type `script` into the live pane's prompt, then the bytes the shell then reads raw (`count` of them, read after
    /// `request` is printed) as `cat -v` shows them, after `reply:`, then `NODUP` unless a byte more came within a second
    /// (a second answer to the one request).
    fn ask(pane: &Pane, request: &str, count: usize) -> TestResult {
        let script = format!(
            "stty raw -echo; printf '{request}'; r=$(dd bs=1 count={count} 2>/dev/null | cat -v); stty sane; \
             printf 'reply:%s\\n' \"$r\"; if read -r -t 1 -n 1 _; then echo DUP; else echo NODUP; fi\r"
        );
        pane.write(script.as_bytes())?;
        Ok(())
    }

    #[test]
    fn a_live_panes_programs_are_answered_with_the_panes_theme_through_its_attach() -> TestResult {
        let live = Live::new()?;
        let configuration = live.configuration()?;
        let mut config = PaneConfig::new(SIZE);
        config.colors.background = Rgb::new(0x12, 0x34, 0x56);
        let pane = live.pane_with(&configuration, &config)?;
        wait_frame(&pane, prompt_painted);
        // the server's terminal answers, with the colours the attach (the reply owner) read of the pane's terminal; the
        // attach keeps the query from the pane's terminal, so it is answered once
        ask(&pane, "\\033]11;?\\033\\\\", 25)?;
        wait_text(&pane, "reply:^[]11;rgb:1212/3434/5656^[\\");
        wait_text(&pane, "NODUP");
        Ok(())
    }

    #[test]
    fn a_live_panes_clipboard_read_is_answered_by_the_pane_through_its_attach() -> TestResult {
        let live = Live::new()?;
        let configuration = live.configuration()?;
        let pane = live.pane(&configuration)?;
        wait_frame(&pane, prompt_painted);
        pane.take_events();
        // the server delegates the read to the attach, which asks its terminal, the pane's
        ask(&pane, "\\033]52;c;?\\007", 12)?;
        let events = wait_events(&pane, |events| {
            events.iter().any(|event| matches!(event, PaneEvent::ClipboardRead(_)))
        });
        let read = events
            .iter()
            .find_map(|event| match event {
                PaneEvent::ClipboardRead(read) => Some(*read),
                _ => None,
            })
            .ok_or("no clipboard read")?;
        pane.answer_clipboard_read(read.id, ClipboardAnswer::Text(Some("abc".to_owned())));
        // "abc" is YWJj
        wait_text(&pane, "reply:^[]52;c;YWJj^G");
        wait_text(&pane, "NODUP");
        Ok(())
    }
}

#[test]
fn a_missing_session_server_cannot_wrap_and_its_attach_ends_the_pane() -> TestResult {
    let state = tempfile::tempdir()?;
    let (_resources, ghostty) = resources("", false)?;
    let env = MapEnv::new()
        .with("AGTERM_STATE_DIR", state.path())
        .with(
            agterm_session::launch::EXECUTABLE_OVERRIDE,
            "/nonexistent/agterm-test/agterm-session",
        )
        .with("GHOSTTY_RESOURCES_DIR", &ghostty);
    let runtime = agterm_session::launch::Runtime::resolve(&env, None, Some("/bin/bash".to_owned()), true);
    // the launch probe rejects the host, so the pane falls back to a plain launch
    assert_eq!(
        runtime.configuration(Uuid::new_v4(), BTreeMap::new(), None),
        Err(LiveRejection::ExecutableUnavailable)
    );
    // a seed made while the server was there, run once it is gone: the attach fails and the pane ends
    let pane = Pane::launch(
        &launch(Some(
            "exec '/nonexistent/agterm-test/agterm-session' attach agterm-gone",
        )),
        &Host::from_runtime(&GhosttyConfig::default(), &runtime, None),
        &PaneConfig::new(SIZE),
    )?;
    let status = exit_of(&pane);
    // `exec` of a missing file: 127, or 126 from macOS's sh
    assert!(matches!(status.code, 126 | 127), "{status:?}");
    assert!(!status.held);
    Ok(())
}
