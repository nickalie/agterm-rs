//! Live panes attached in process to the cargo-built session server (`crate::session_io`), on every OS: a fresh
//! session's prompt, typing and a resize reaching it, its environment and directory, the pane dropped leaving the
//! session running and a reattach painting its snapshot, the role a managed pane is told, the session's end and an
//! attach that cannot start ending the pane, and, as reply owner, the server answering the program with the pane's
//! theme, a clipboard read and a Kitty query answered through the pane. Each test runs under its own state directory,
//! so its own namespace and server, reaped through that server alone.
//!
//! The session's shell is the OS's live shell with a stand-in integration that sets the prompt `live-ready$ `: bash
//! on Linux; zsh on macOS, whose bash (3.2) never reads its integration and whose `/etc/profile` sets a prompt of its
//! own; PowerShell on Windows. The replies' tests read the program's raw input with a POSIX script, so they run on
//! Unix (on Windows conhost answers or drops those queries itself, `agterm-session`'s `server::replies`).
//!
//! `AGTERM_TEST_LIVE_LAUNCHES` sets how many fresh live launches must all paint their prompt (default 50, 10 on
//! Windows, whose PowerShell takes a second to start, then as many again in parallel).

#[expect(dead_code, reason = "the file helpers serve the other test files")]
mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agterm_core::ghostty_config::GhosttyConfig;
use agterm_core::live::lead::LeadAttachment;
use agterm_core::live::support::{Configuration, LiveAttach, attach_line, live_attach, settled};
use agterm_core::paths::MapEnv;
use agterm_core::restore::mode::LiveRejection;
use agterm_core::restore::spawn_plan::LaunchSeed;
use agterm_host::{KillOutcome, SessionHost};
use agterm_session::host::ServerHost;
use agterm_term::{ExitStatus, Frame, GridSize, Host, Launch, Pane, PaneConfig, PaneEvent};
use support::{SIZE, TestResult};
use uuid::Uuid;

/// How long a wait may take: PowerShell can take seconds to start on a CI runner.
const PATIENCE: Duration = Duration::from_secs(60);

/// The names of the inherited `AGTERM_*`/`ZMX_*` variables, which no test pane may see.
fn isolation() -> Vec<String> {
    std::env::vars_os()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .filter(|name| {
            let name = name.to_ascii_uppercase();
            name.starts_with("AGTERM_") || name.starts_with("ZMX_")
        })
        .collect()
}

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

/// The OS's live shell, and its stand-in integration's file and contents under the resources directory.
fn shell() -> TestResult<(String, &'static str, &'static str)> {
    if cfg!(windows) {
        let pwsh = agterm_session::launch::login_shell().ok_or("no PowerShell on this machine")?;
        Ok((
            pwsh,
            "shell-integration/pwsh/ngterm.ps1",
            "function global:prompt { 'live-ready$ ' }\n",
        ))
    } else if cfg!(target_os = "macos") {
        // read after macOS's `/etc/zshrc`, whose prompt it replaces; `.zshenv` is the integration's loader
        Ok((
            "/bin/zsh".to_owned(),
            "shell-integration/zsh/.zshrc",
            "PS1='live-ready$ '\n",
        ))
    } else {
        Ok((
            "/bin/bash".to_owned(),
            "shell-integration/bash/ghostty.bash",
            "PS1='live-ready$ '\n",
        ))
    }
}

/// A live launch's inputs under a private state directory (so a private namespace, its own server).
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
        let resources = tempfile::tempdir()?;
        let (shell, integration, contents) = shell()?;
        let file = resources.path().join(integration);
        std::fs::create_dir_all(file.parent().ok_or("no integration directory")?)?;
        std::fs::write(&file, contents)?;
        if cfg!(target_os = "macos") {
            std::fs::write(resources.path().join("shell-integration/zsh/.zshenv"), "")?;
        }
        // no terminfo beside them: the shell sees xterm-256color, which every system describes
        let env = MapEnv::new()
            .with("AGTERM_STATE_DIR", state.path())
            .with(agterm_session::launch::EXECUTABLE_OVERRIDE, &executable)
            .with("GHOSTTY_RESOURCES_DIR", resources.path())
            .with("HOME", home.path());
        let mut runtime = agterm_session::launch::Runtime::resolve(&env, None, Some(shell), true);
        runtime.own_resources_dir = Some(resources.path().to_path_buf());
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

    /// A fresh pane's wrap configuration, managed under `lead`.
    fn configuration_with(&self, lead: Option<LeadAttachment>) -> TestResult<Configuration> {
        let configuration = self.runtime.configuration(Uuid::new_v4(), BTreeMap::new(), lead);
        Ok(configuration.map_err(|rejection| format!("the host cannot wrap a pane: {rejection:?}"))?)
    }

    fn configuration(&self) -> TestResult<Configuration> {
        self.configuration_with(None)
    }

    /// The launch of `configuration`'s attach in `cwd` with the pane's `env`, as `resolve_live_seed` builds it.
    fn launch_in(&self, configuration: &Configuration, cwd: &Path, env: &[(&str, &str)]) -> Launch {
        let attach = live_attach(configuration, None, None, &[] as &[&str]);
        let seed = LaunchSeed {
            command: Some(settled(&attach_line(configuration, &attach))),
            env: configuration.environment.clone().into_iter().collect(),
            live: true,
            attach: configuration.in_process(attach),
            ..LaunchSeed::default()
        };
        let mut pane_env = vec![("HOME".to_owned(), self.home.path().display().to_string())];
        pane_env.extend(env.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())));
        let mut launch = Launch::from_seed(&seed, Some(cwd.to_path_buf()), &pane_env);
        launch.env_remove = isolation();
        launch
    }

    fn launch(&self, configuration: &Configuration) -> Launch {
        self.launch_in(configuration, self.home.path(), &[])
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
    }
}

/// The pids of this namespace's session processes, read from `/proc` (Linux): every process whose environment carries
/// `AGTERM_LIVE_DIR=<dir>`.
#[cfg(target_os = "linux")]
fn processes_in(dir: &str) -> Vec<u32> {
    let marker = format!("{}={dir}", agterm_core::live::DIRECTORY_VARIABLE);
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .unwrap_or_default()
                .split(|byte| *byte == 0)
                .any(|var| var == marker.as_bytes())
        })
        .collect()
}

/// Whether `pid` is still in this machine's process table (an unreadable table counts as running).
fn runs(pid: u32) -> bool {
    agterm_session::job::read_table()
        .is_none_or(|table| table.iter().any(|entry| u32::try_from(entry.record.pid) == Ok(pid)))
}

/// End this test's namespace through its own server, and only it: every session it lists killed, then the server's
/// own exit once idle (`agterm_session::server::lifecycle::IDLE_GRACE` after its last client) and, on Linux, every
/// process of the namespace gone; the namespace directory then goes. What outlives the deadline fails the test.
fn reap(host: &ServerHost, dir: &str) {
    let server = agterm_session::client::Client::new(dir)
        .api()
        .ok()
        .flatten()
        .map(|api| api.welcome.pid);
    if let Ok(records) = host.list() {
        let names: Vec<String> = records.iter().map(|record| record.name.clone()).collect();
        if !names.is_empty() {
            host.kill_batch(&names);
        }
    }
    let deadline = Instant::now() + agterm_session::server::lifecycle::IDLE_GRACE + PATIENCE;
    let left = || {
        #[cfg(target_os = "linux")]
        let sessions = processes_in(dir);
        #[cfg(not(target_os = "linux"))]
        let sessions: Vec<u32> = Vec::new();
        let server = server.filter(|pid| runs(*pid));
        (server, sessions)
    };
    loop {
        let (server, sessions) = left();
        if server.is_none() && sessions.is_empty() {
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        if Instant::now() > deadline {
            let message = format!("left in the namespace {dir}: server {server:?}, session processes {sessions:?}");
            // a test that already failed keeps its own report
            assert!(std::thread::panicking(), "{message}");
            eprintln!("{message}");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
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
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_text(pane: &Pane, needle: &str) -> Arc<Frame> {
    wait_frame(pane, |frame| frame.lines().iter().any(|line| line.contains(needle)))
}

fn wait_events(pane: &Pane, mut done: impl FnMut(&[PaneEvent]) -> bool) -> Vec<PaneEvent> {
    let deadline = Instant::now() + PATIENCE;
    let mut events = Vec::new();
    loop {
        events.extend(pane.take_events());
        if done(&events) {
            return events;
        }
        assert!(Instant::now() < deadline, "timed out; events so far: {events:#?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The pane's exit, waited for.
fn exit_of(pane: &Pane) -> ExitStatus {
    let events = wait_events(pane, |events| {
        events.iter().any(|event| matches!(event, PaneEvent::Exited(_)))
    });
    let Some(PaneEvent::Exited(status)) = events.into_iter().rfind(|event| matches!(event, PaneEvent::Exited(_)))
    else {
        unreachable!("waited for an exit")
    };
    status
}

fn prompt_painted(frame: &Frame) -> bool {
    frame.lines().iter().any(|line| line.contains("live-ready$"))
}

/// A line the session's shell runs, typed with its Return: `posix` in bash and zsh, `pwsh` in PowerShell.
fn typed(posix: &str, pwsh: &str) -> Vec<u8> {
    format!("{}\r", if cfg!(windows) { pwsh } else { posix }).into_bytes()
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
        .unwrap_or(if cfg!(windows) { 10 } else { 50 });
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
fn a_live_pane_round_trips_input_and_resizes_its_session() -> TestResult {
    let live = Live::new()?;
    let pane = live.pane(&live.configuration()?)?;
    wait_frame(&pane, prompt_painted);
    assert_eq!(pane.pid(), None, "no child of the pane's own");
    pane.write(&typed("echo round-\"\"trip", "echo ('round-' + 'trip')"))?;
    wait_text(&pane, "round-trip");
    // the pane leads (the first client of an unmanaged session), so its grid is the session's
    pane.resize(GridSize {
        cols: 100,
        rows: 30,
        ..SIZE
    })?;
    pane.write(&typed(
        "echo \"size=$(stty size)\"",
        "echo \"size=$([Console]::WindowHeight) $([Console]::WindowWidth)\"",
    ))?;
    wait_text(&pane, "size=30 100");
    Ok(())
}

#[test]
fn a_live_sessions_environment_and_directory_are_the_launchs() -> TestResult {
    let live = Live::new()?;
    let configuration = live.configuration()?;
    let cwd = tempfile::tempdir()?;
    let marker = cwd
        .path()
        .file_name()
        .ok_or("no directory name")?
        .to_string_lossy()
        .into_owned();
    let launch = live.launch_in(&configuration, cwd.path(), &[("NGTERM_TEST_VALUE", "given")]);
    let pane = Pane::launch(&launch, &live.host, &PaneConfig::new(SIZE))?;
    wait_frame(&pane, prompt_painted);
    pane.write(&typed(
        "echo \"value=$NGTERM_TEST_VALUE pane=$AGTERM_PANE_ID\"; echo \"session=$AGTERM_LIVE_SESSION\"; pwd",
        "echo \"value=$env:NGTERM_TEST_VALUE pane=$env:AGTERM_PANE_ID\"; echo \"session=$env:AGTERM_LIVE_SESSION\"; \
         (Get-Location).Path",
    ))?;
    wait_text(&pane, &format!("value=given pane={}", configuration.pane_id));
    wait_text(&pane, &format!("session={}", configuration.daemon_name));
    wait_text(&pane, &marker);
    Ok(())
}

#[test]
fn a_dropped_live_pane_leaves_its_session_and_a_reattach_paints_its_snapshot() -> TestResult {
    let live = Live::new()?;
    let configuration = live.configuration()?;
    let pane = live.pane(&configuration)?;
    wait_frame(&pane, prompt_painted);
    pane.write(&typed("echo kept-\"\"here", "echo ('kept-' + 'here')"))?;
    wait_text(&pane, "kept-here");
    drop(pane);
    // the session runs on, with no client
    let host = live.session_host();
    let deadline = Instant::now() + PATIENCE;
    loop {
        let records = host.list()?;
        let record = records
            .iter()
            .find(|record| record.name == configuration.daemon_name)
            .ok_or("the session ended with its pane")?;
        if record.clients == Some(0) {
            break;
        }
        assert!(Instant::now() < deadline, "the dropped pane is still attached");
        std::thread::sleep(Duration::from_millis(20));
    }
    // a new pane of the same session paints what it showed, and its shell is the same one
    let again = live.pane(&configuration)?;
    wait_text(&again, "kept-here");
    again.write(&typed("echo still-\"\"there", "echo ('still-' + 'there')"))?;
    wait_text(&again, "still-there");
    Ok(())
}

#[test]
fn a_managed_live_pane_reports_its_role_as_its_title() -> TestResult {
    let live = Live::new()?;
    let lead = LeadAttachment {
        nonce: "n1".to_owned(),
        claim: true,
    };
    let configuration = live.configuration_with(Some(lead))?;
    let pane = live.pane(&configuration)?;
    let events = wait_events(&pane, |events| {
        events
            .iter()
            .any(|event| matches!(event, PaneEvent::Title(title) if title.contains(":leader:")))
    });
    let report = events
        .iter()
        .find_map(|event| match event {
            PaneEvent::Title(title) => agterm_core::live::lead::LeadNotice::parse(title),
            _ => None,
        })
        .ok_or("no role report")?;
    assert_eq!(report.nonce, "n1");
    // the report never reached the pane's terminal: its own title stays the session's
    wait_frame(&pane, prompt_painted);
    Ok(())
}

#[test]
fn a_live_pane_ends_when_its_session_is_gone() -> TestResult {
    let live = Live::new()?;
    let configuration = live.configuration()?;
    let pane = live.pane(&configuration)?;
    wait_frame(&pane, prompt_painted);
    assert_eq!(
        live.session_host().kill(&configuration.daemon_name, true),
        KillOutcome::Killed
    );
    // the session's end closes the pane: a live pane's attach is never waited for
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
    #[cfg(target_os = "linux")]
    assert!(processes_in(&blocked.path().display().to_string()).is_empty());
    Ok(())
}

#[test]
fn a_missing_session_server_cannot_wrap_and_its_attach_ends_the_pane() -> TestResult {
    let state = tempfile::tempdir()?;
    let resources = tempfile::tempdir()?;
    let missing = Path::new("/nonexistent/agterm-test").join(agterm_session::launch::EXECUTABLE_FILE);
    let env = MapEnv::new()
        .with("AGTERM_STATE_DIR", state.path())
        .with(agterm_session::launch::EXECUTABLE_OVERRIDE, &missing)
        .with("GHOSTTY_RESOURCES_DIR", resources.path());
    let runtime = agterm_session::launch::Runtime::resolve(&env, None, Some(shell()?.0), true);
    // the launch probe rejects the host, so the pane falls back to a plain launch
    assert!(matches!(
        runtime.configuration(Uuid::new_v4(), BTreeMap::new(), None),
        Err(LiveRejection::ExecutablePathNotAbsolute | LiveRejection::ExecutableUnavailable)
    ));
    // a seed made while the server was there, run once it is gone: the attach cannot start a server and the pane ends
    let socket_directory = agterm_core::live::socket_directory(&runtime.state_dir);
    let launch = Launch {
        command: Some("unused".to_owned()),
        working_directory: Some(state.path().to_path_buf()),
        env: vec![
            (
                agterm_core::live::DIRECTORY_VARIABLE.to_owned(),
                socket_directory.clone(),
            ),
            (agterm_core::live::SESSION_VARIABLE.to_owned(), String::new()),
        ],
        env_remove: isolation(),
        live: Some(LiveAttach {
            executable: missing.display().to_string(),
            session: "agterm-gone".to_owned(),
            argv: Vec::new(),
        }),
        ..Launch::default()
    };
    let pane = Pane::launch(
        &launch,
        &Host::from_runtime(&GhosttyConfig::default(), &runtime, None),
        &PaneConfig::new(SIZE),
    )?;
    let status = exit_of(&pane);
    assert!(status.code != 0 && !status.held, "{status:?}");
    let _ = std::fs::remove_dir_all(&socket_directory);
    Ok(())
}

// -- the pane as reply owner (Unix: a POSIX script reads the program's raw input)

/// Run, in the live pane's session, a script that prints `request`, then the `count` bytes it reads back raw, as
/// `cat -v` shows them, after `reply:`, then `NODUP` unless a byte more came within a second (a second answer).
#[cfg(unix)]
fn ask(live: &Live, pane: &Pane, request: &str, count: usize) -> TestResult {
    let script = live.home.path().join(format!("ask-{count}.sh"));
    std::fs::write(
        &script,
        format!(
            "stty raw -echo; printf '{request}'; r=$(dd bs=1 count={count} 2>/dev/null | cat -v); stty sane\n\
             printf 'reply:%s\\n' \"$r\"; if read -r -t 1 -n 1 _; then echo DUP; else echo NODUP; fi\n"
        ),
    )?;
    pane.write(format!("/bin/bash '{}'\r", script.display()).as_bytes())?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_live_panes_programs_are_answered_by_the_server_with_the_panes_theme() -> TestResult {
    let live = Live::new()?;
    let configuration = live.configuration()?;
    let mut config = PaneConfig::new(SIZE);
    config.colors.background = agterm_term::Rgb::new(0x12, 0x34, 0x56);
    let pane = live.pane_with(&configuration, &config)?;
    wait_frame(&pane, prompt_painted);
    // the server's terminal answers, with the colours the pane sent as its facts; the pane's own terminal answers no
    // query, so it is answered once
    ask(&live, &pane, "\\033]11;?\\033\\\\", 25)?;
    wait_text(&pane, "reply:^[]11;rgb:1212/3434/5656^[\\");
    wait_text(&pane, "NODUP");
    // a theme change reaches the server's answers
    let mut colors = config.colors.clone();
    colors.background = agterm_term::Rgb::new(0x65, 0x43, 0x21);
    pane.set_colors(&colors)?;
    ask(&live, &pane, "\\033]11;?\\033\\\\", 25)?;
    wait_text(&pane, "reply:^[]11;rgb:6565/4343/2121^[\\");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_live_panes_clipboard_read_is_answered_through_the_pane() -> TestResult {
    let live = Live::new()?;
    let pane = live.pane(&live.configuration()?)?;
    wait_frame(&pane, prompt_painted);
    pane.take_events();
    // the server delegates the read to its reply owner, the pane, which asks its frontend once
    ask(&live, &pane, "\\033]52;c;?\\007", 12)?;
    let events = wait_events(&pane, |events| {
        events.iter().any(|event| matches!(event, PaneEvent::ClipboardRead(_)))
    });
    let reads: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            PaneEvent::ClipboardRead(read) => Some(*read),
            _ => None,
        })
        .collect();
    assert_eq!(reads.len(), 1, "{reads:?}");
    pane.answer_clipboard_read(reads[0].id, agterm_term::ClipboardAnswer::Text(Some("abc".to_owned())));
    // "abc" is YWJj
    wait_text(&pane, "reply:^[]52;c;YWJj^G");
    wait_text(&pane, "NODUP");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_live_panes_kitty_query_is_answered_by_the_panes_terminal() -> TestResult {
    let live = Live::new()?;
    let pane = live.pane(&live.configuration()?)?;
    wait_frame(&pane, prompt_painted);
    // a query of a direct 1x1 RGB image: the pane's terminal answers it, through the server's barrier, once
    ask(&live, &pane, "\\033_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\\033\\\\", 12)?;
    wait_text(&pane, "reply:^[_Gi=31;OK^[\\");
    wait_text(&pane, "NODUP");
    Ok(())
}
