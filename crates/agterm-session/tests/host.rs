//! `ServerHost`, the `SessionHost` over the session server, on every OS: `agterm-zmx`'s host tests (upstream's
//! `ZmxClientTests.swift` and the real-zmx tests) ported onto it, against a scripted peer for each answer and error
//! path, and against a real server run in the test process for the round trip, the forced batch kill that leaves
//! nothing of the namespace running, and a restart's foreground job.
//!
//! Every session gets the test's environment without the live agterm's `AGTERM_*` and `ZMX_*` variables, in a private
//! namespace.

#[expect(
    dead_code,
    reason = "the helpers serve the pane, lead, snapshot, attach, api, client and host tests alike"
)]
mod support;

#[expect(dead_code, reason = "the helpers serve the client and host tests alike")]
mod peer;

use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use agterm_host::{HostError, KillOutcome, LiveScreen, LiveSessionRecord, ProcessRecord, SessionHost};
use agterm_session::client::{AttachEvent, Client};
use agterm_session::host::ServerHost;
use agterm_session::job::{ProcessEntry, Signal, Sweeper};
use agterm_session::protocol::{
    ApiError, ApiErrorKind, ApiRequest, ApiResponse, AttachRequest, KillResult, ReplyMetadata, Screen, SessionRecord,
    SessionSpec,
};
use agterm_session::server::lifecycle::StopReason;
use peer::{Answer, Server, api_peer, calls};
use support::{PATIENCE, SIZE, TestResult, shell, spec, tagged};

const NAME: &str = "agterm-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A host of `directory`'s namespace, whose executable is this test binary.
fn host(directory: &std::path::Path) -> TestResult<ServerHost> {
    Ok(ServerHost::new(std::env::current_exe()?, directory))
}

fn refusal(kind: ApiErrorKind, message: &str) -> Answer {
    Answer::Reply(Err(ApiError {
        kind,
        message: message.to_owned(),
    }))
}

fn host_error(result: Result<impl std::fmt::Debug, HostError>) -> String {
    match result {
        Err(HostError(message)) => message,
        Ok(answer) => format!("not a failure: {answer:?}"),
    }
}

#[test]
fn the_listing_asks_only_for_the_list_and_keeps_every_field() -> TestResult {
    let (peer, log) = api_peer(|_| {
        Answer::Reply(Ok(ApiResponse::Sessions(vec![
            SessionRecord {
                name: NAME.to_owned(),
                clients: 2,
                leader_pid: Some(41),
                created_at: 1_700_000_000,
            },
            SessionRecord {
                name: "agterm-ended".to_owned(),
                clients: 0,
                leader_pid: None,
                created_at: 7,
            },
        ])))
    })?;
    let records = host(peer.directory())?.list()?;
    assert_eq!(calls(&log), [ApiRequest::List], "the read must mutate nothing");
    assert_eq!(
        records,
        [
            LiveSessionRecord {
                name: NAME.to_owned(),
                clients: Some(2),
                leader_pid: Some(41),
                created_at: Some(1_700_000_000),
            },
            LiveSessionRecord {
                name: "agterm-ended".to_owned(),
                clients: Some(0),
                leader_pid: None,
                created_at: Some(7),
            }
        ]
    );
    Ok(())
}

#[test]
fn an_empty_namespace_is_an_answer_and_a_failure_is_not() -> TestResult {
    let (empty, _) = api_peer(|_| Answer::Reply(Ok(ApiResponse::Sessions(Vec::new()))))?;
    assert_eq!(host(empty.directory())?.list(), Ok(Vec::new()));
    // no server runs: it holds no session
    let none = tempfile::tempdir()?;
    assert_eq!(host(&none.path().join("live"))?.list(), Ok(Vec::new()));
    let (failing, _) = api_peer(|_| refusal(ApiErrorKind::Failed, "the table broke"))?;
    assert_eq!(
        host_error(host(failing.directory())?.list()),
        "live session list failed: the table broke"
    );
    let broken_answers: [fn() -> Answer; 3] = [
        || Answer::Reply(Ok(ApiResponse::Done)),
        || Answer::WrongId(ApiResponse::Sessions(Vec::new())),
        || Answer::Close,
    ];
    for answer in broken_answers {
        let (broken, _) = api_peer(move |_| answer())?;
        let message = host_error(host(broken.directory())?.list());
        assert!(
            message.starts_with("live session list failed: "),
            "a broken answer must not read as an empty namespace: {message}"
        );
    }
    Ok(())
}

fn kill_peer() -> TestResult<(peer::Peer, peer::Calls)> {
    api_peer(|request| {
        let ApiRequest::Kill { names, .. } = request else {
            return Answer::Close;
        };
        let outcome = |name: &str| match name {
            "agterm-live" => agterm_session::protocol::KillOutcome::Killed,
            "agterm-gone" => agterm_session::protocol::KillOutcome::Gone,
            _ => agterm_session::protocol::KillOutcome::Failed("signal refused".to_owned()),
        };
        let results = names
            .iter()
            .filter(|name| *name != "agterm-unanswered")
            .map(|name| KillResult {
                name: name.clone(),
                outcome: outcome(name),
            })
            .collect();
        Answer::Reply(Ok(ApiResponse::Killed(results)))
    })
}

#[test]
fn a_kill_maps_every_outcome_and_forces_only_when_asked() -> TestResult {
    let (peer, log) = kill_peer()?;
    let host = host(peer.directory())?;
    assert_eq!(host.kill("agterm-live", false), KillOutcome::Killed);
    assert_eq!(host.kill("agterm-gone", true), KillOutcome::StaleSocket);
    assert_eq!(
        host.kill("agterm-stuck", true),
        KillOutcome::Failed("signal refused".to_owned())
    );
    assert_eq!(
        host.kill("agterm-unanswered", true),
        KillOutcome::Failed("the live session server answered no kill result for agterm-unanswered".to_owned())
    );
    let kill = |name: &str, force| ApiRequest::Kill {
        names: vec![name.to_owned()],
        force,
    };
    assert_eq!(
        calls(&log),
        [
            kill("agterm-live", false),
            kill("agterm-gone", true),
            kill("agterm-stuck", true),
            kill("agterm-unanswered", true)
        ]
    );
    // no server: nothing was left to kill
    let none = tempfile::tempdir()?;
    assert_eq!(
        self::host(&none.path().join("live"))?.kill(NAME, true),
        KillOutcome::StaleSocket
    );
    let (failing, _) = api_peer(|_| Answer::Close)?;
    assert!(matches!(
        self::host(failing.directory())?.kill(NAME, true),
        KillOutcome::Failed(reason) if reason.contains("connection")
    ));
    Ok(())
}

#[test]
fn a_batch_kill_forces_every_distinct_name_once() -> TestResult {
    let (peer, log) = kill_peer()?;
    let host = host(peer.directory())?;
    let names = ["agterm-live", "agterm-gone", "agterm-live"].map(str::to_owned);
    assert!(host.kill_batch(&names));
    assert_eq!(
        calls(&log),
        [ApiRequest::Kill {
            names: vec!["agterm-live".to_owned(), "agterm-gone".to_owned()],
            force: true
        }]
    );
    assert!(host.kill_batch(&[]));
    assert_eq!(calls(&log).len(), 1, "nothing to kill sends nothing");
    assert!(
        !host.kill_batch(&["agterm-live".to_owned(), "agterm-stuck".to_owned()]),
        "a failed kill fails the batch"
    );
    let (failing, _) = api_peer(|_| refusal(ApiErrorKind::Failed, "boom"))?;
    assert!(!self::host(failing.directory())?.kill_batch(&[NAME.to_owned()]));
    let none = tempfile::tempdir()?;
    assert!(self::host(&none.path().join("live"))?.kill_batch(&[NAME.to_owned()]));
    Ok(())
}

#[test]
fn the_screen_reads_the_servers_terminal_and_a_failure_is_never_a_blank_screen() -> TestResult {
    let (peer, log) = api_peer(|request| match request {
        ApiRequest::Screen { name, .. } if name == "agterm-a" => Answer::Reply(Ok(ApiResponse::Screen(Screen {
            revision: 7,
            columns: 101,
            rows: 33,
            cursor_column: 2,
            cursor_row: 9,
            alternate: true,
            text: "> draft".to_owned(),
        }))),
        ApiRequest::Screen { name, .. } => {
            refusal(ApiErrorKind::NoSuchSession, &format!("no live session named {name:?}"))
        }
        _ => Answer::Close,
    })?;
    let host = host(peer.directory())?;
    let active = host.screen("agterm-a", false)?;
    host.screen("agterm-a", true)?;
    assert_eq!(
        calls(&log),
        [
            ApiRequest::Screen {
                name: "agterm-a".to_owned(),
                all: false
            },
            ApiRequest::Screen {
                name: "agterm-a".to_owned(),
                all: true
            }
        ]
    );
    assert_eq!(
        active,
        LiveScreen {
            revision: 7,
            columns: 101,
            rows: 33,
            cursor_column: 2,
            cursor_row: 9,
            alternate: true,
            text: "> draft".to_owned(),
        }
    );
    assert_eq!(
        host_error(host.screen("agterm-b", false)),
        "live session screen failed: no live session named \"agterm-b\""
    );
    let (wrong, _) = api_peer(|_| Answer::Reply(Ok(ApiResponse::Done)))?;
    assert!(self::host(wrong.directory())?.screen("agterm-a", false).is_err());
    let none = tempfile::tempdir()?;
    assert!(
        host_error(self::host(&none.path().join("live"))?.screen("agterm-a", false))
            .starts_with("live session screen failed: no live session server runs at ")
    );
    Ok(())
}

#[test]
fn typing_sends_the_exact_bytes_and_reports_a_rejection() -> TestResult {
    let (peer, log) = api_peer(|request| match request {
        ApiRequest::Type { name, .. } if name == "agterm-full" => {
            refusal(ApiErrorKind::Failed, "the session's input queue is full")
        }
        _ => Answer::Reply(Ok(ApiResponse::Done)),
    })?;
    let host = host(peer.directory())?;
    host.type_input("agterm-a", b"\r")?;
    assert_eq!(
        calls(&log),
        [ApiRequest::Type {
            name: "agterm-a".to_owned(),
            data: b"\r".to_vec()
        }],
        "a lone Return must reach the server"
    );
    assert_eq!(
        host_error(host.type_input("agterm-full", b"a")),
        "live session type failed: the session's input queue is full"
    );
    let none = tempfile::tempdir()?;
    assert!(
        self::host(&none.path().join("live"))?
            .type_input("agterm-a", b"a")
            .is_err()
    );
    Ok(())
}

#[test]
fn a_silent_server_fails_each_call_within_the_timeout() -> TestResult {
    let (peer, log) = api_peer(|_| Answer::Silent)?;
    let host = host(peer.directory())?.with_timeout(Duration::from_millis(300));
    let started = Instant::now();
    assert!(host.list().is_err());
    assert!(host.screen(NAME, false).is_err());
    assert!(host.type_input(NAME, b"x").is_err());
    assert!(matches!(host.kill(NAME, true), KillOutcome::Failed(_)));
    assert!(!host.kill_batch(&[NAME.to_owned()]));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?} for five calls",
        started.elapsed()
    );
    assert_eq!(calls(&log).len(), 5, "each call reached the server once, never retried");
    Ok(())
}

#[test]
fn a_reply_trickling_in_fails_the_call_at_its_timeout() -> TestResult {
    // a reply of some twenty bytes, one every 150 ms: each read is quick, the whole reply some three seconds
    let (peer, log) = api_peer(|_| Answer::Trickle {
        response: ApiResponse::Sessions(Vec::new()),
        pause: Duration::from_millis(150),
    })?;
    let host = host(peer.directory())?.with_timeout(Duration::from_millis(300));
    let started = Instant::now();
    assert!(host.list().is_err(), "a reply past the timeout is a failed call");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the timeout bounds the whole call: {:?}",
        started.elapsed()
    );
    assert_eq!(calls(&log).len(), 1);
    Ok(())
}

#[test]
fn availability_is_an_executable_session_binary() -> TestResult {
    assert!(!ServerHost::new("/nonexistent/agterm-session", "/tmp/live").is_available());
    assert!(ServerHost::new(std::env::current_exe()?, "/tmp/live").is_available());
    Ok(())
}

/// A shell (pid 10) whose terminal's foreground is its job (pid 11, a group of its own and its child), which both the
/// Unix and the Windows rule find.
fn job_table() -> Vec<ProcessEntry> {
    let shell = ProcessRecord {
        pid: 10,
        started: 1,
        group: 10,
        foreground: 11,
    };
    let job = ProcessRecord {
        pid: 11,
        started: 2,
        group: 11,
        foreground: 11,
    };
    vec![
        ProcessEntry {
            record: shell,
            parent: 1,
        },
        ProcessEntry {
            record: job,
            parent: 10,
        },
    ]
}

#[test]
fn a_restarts_job_is_read_and_signalled_through_the_hosts_own_process_table() {
    let signalled: Arc<Mutex<Vec<(Signal, i32)>>> = Arc::default();
    let sink = Arc::clone(&signalled);
    let sweeper = Sweeper::new(
        Box::new(|| Some(job_table())),
        Box::new(move |signal, record| {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((signal, record.pid));
        }),
    );
    // no server runs: the job calls do not need one
    let host = ServerHost::new("/nonexistent/agterm-session", "/nonexistent/live").with_sweeper(sweeper);
    let job = host.foreground_job(10).unwrap_or_default();
    assert_eq!(job.iter().map(|record| record.pid).collect::<Vec<_>>(), [11]);
    assert!(host.job_running(&job));
    host.end_job(&job);
    assert_eq!(
        *signalled.lock().unwrap_or_else(PoisonError::into_inner),
        [(Signal::Kill, 11)]
    );
    let unreadable = ServerHost::new("/nonexistent/agterm-session", "/nonexistent/live")
        .with_sweeper(Sweeper::new(Box::new(|| None), Box::new(|_, _| {})));
    assert_eq!(
        unreadable.foreground_job(10),
        None,
        "an unreadable table is not \"no job\""
    );
    assert!(unreadable.job_running(&job), "an unreadable table counts as running");
}

// ---------------------------------------------------------------------------------------------------------------------
// Against a real server
// ---------------------------------------------------------------------------------------------------------------------

/// Creates the session `name` running `spec` through the client library, then detaches, as zmx's `run -d` did.
fn create_detached(server: &Server, name: &str, spec: SessionSpec) -> TestResult {
    let mut attachment = Client::new(server.directory()).attach(AttachRequest {
        name: name.to_owned(),
        create: Some(spec),
        size: SIZE,
        managed: None,
        metadata: ReplyMetadata::default(),
    })?;
    // the snapshot is in: the session runs
    while !matches!(attachment.reader.recv()?, Some(AttachEvent::SnapshotEnd)) {}
    attachment.writer.detach()?;
    while attachment.reader.recv()?.is_some() {}
    Ok(())
}

/// Wait for `name` in the host's listing; its record.
fn listed(host: &ServerHost, name: &str) -> TestResult<LiveSessionRecord> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(record) = host
            .list()?
            .into_iter()
            .find(|record| record.name == name && record.clients == Some(0))
        {
            return Ok(record);
        }
        assert!(Instant::now() < deadline, "{name} never listed detached");
        thread::sleep(Duration::from_millis(50));
    }
}

/// Wait until the host's listing no longer holds `name`.
fn unlisted(host: &ServerHost, name: &str) -> TestResult {
    let deadline = Instant::now() + PATIENCE;
    while host.list()?.iter().any(|record| record.name == name) {
        assert!(Instant::now() < deadline, "{name} is still listed");
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
fn the_host_lists_reads_types_into_and_kills_a_real_session() -> TestResult {
    let server = Server::start()?;
    let host = host(server.directory())?;
    assert!(host.is_available());
    assert_eq!(host.list()?, Vec::new(), "an empty namespace is an answer");

    create_detached(&server, NAME, shell())?;
    let record = listed(&host, NAME)?;
    assert!(record.leader_pid.is_some_and(|pid| pid > 0));
    assert!(record.created_at.is_some_and(|created| created > 0));

    host.type_input(NAME, format!("{}\r", tagged("typed-42")).as_bytes())?;
    let deadline = Instant::now() + PATIENCE;
    loop {
        let screen = host.screen(NAME, false)?;
        assert!(screen.columns > 0 && screen.rows > 0);
        if screen.text.contains("got:typed-42") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "typed input never reached the screen: {:?}",
            screen.text
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert!(host.screen(NAME, true)?.text.contains("got:typed-42"));

    assert_eq!(host.kill(NAME, true), KillOutcome::Killed);
    unlisted(&host, NAME)?;
    assert!(
        host.screen(NAME, false).is_err(),
        "a gone session is never a blank screen"
    );
    assert!(host.type_input(NAME, b"x").is_err());
    assert_eq!(
        host.kill(NAME, true),
        KillOutcome::StaleSocket,
        "nothing was left to kill"
    );
    drop(host);
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

/// The pids of every process whose environment holds `AGTERM_LIVE_DIR=<directory>`, which every process of the
/// namespace's sessions inherits, as the conformance harness's leftover check reads them.
#[cfg(target_os = "linux")]
fn processes_in(directory: &std::path::Path) -> Vec<u32> {
    let marker = format!("AGTERM_LIVE_DIR={}", directory.display());
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environ| {
                environ
                    .split(|byte| *byte == 0)
                    .any(|variable| variable == marker.as_bytes())
            })
        })
        .collect()
}

/// The line that runs a long foreground job in [`shell`].
fn long_job() -> &'static str {
    if cfg!(windows) {
        "ping -n 600 127.0.0.1 >nul\r"
    } else {
        "sleep 600\r"
    }
}

#[test]
fn a_forced_batch_kill_leaves_nothing_of_the_namespace_running() -> TestResult {
    let server = Server::start()?;
    let host = host(server.directory())?;
    let names = ["agterm-one", "agterm-two"].map(str::to_owned);
    for name in &names {
        create_detached(&server, name, shell())?;
        listed(&host, name)?;
        // a foreground job in each shell, as a pane's program runs
        host.type_input(name, long_job().as_bytes())?;
    }
    thread::sleep(Duration::from_millis(300));
    #[cfg(target_os = "linux")]
    assert!(!processes_in(server.directory()).is_empty());

    // what the pane finalizer and the harness's reap run: one forced batch over the claimed names
    assert!(host.kill_batch(&names));
    for name in &names {
        unlisted(&host, name)?;
    }
    #[cfg(target_os = "linux")]
    {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = processes_in(server.directory());
            if left.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "processes left in the namespace: {left:?}");
            thread::sleep(Duration::from_millis(50));
        }
    }
    assert_eq!(host.list()?, Vec::new());
    drop(host);
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}

/// A restart's calls on a real session: the shell's foreground job read through the host's own process table, then
/// waited for and ended, as `agterm-core`'s `restart_live` drives them.
#[test]
fn a_real_shells_foreground_job_is_read_waited_for_and_ended() -> TestResult {
    let server = Server::start()?;
    let host = host(server.directory())?;
    let interactive = if cfg!(windows) {
        spec(&["cmd.exe", "/d", "/q", "/k"])
    } else {
        spec(&["/bin/sh", "-i"])
    };
    create_detached(&server, "agterm-jobs", interactive)?;
    let shell = listed(&host, "agterm-jobs")?.leader_pid.ok_or("no shell")?;
    host.type_input("agterm-jobs", long_job().as_bytes())?;
    let deadline = Instant::now() + PATIENCE;
    let job = loop {
        match host.foreground_job(shell) {
            Some(job) if !job.is_empty() => break job,
            _ => {}
        }
        assert!(Instant::now() < deadline, "the job never started");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(job.iter().all(|process| process.pid != shell), "{job:?}");
    assert!(host.job_running(&job));
    host.end_job(&job);
    let deadline = Instant::now() + PATIENCE;
    while host.job_running(&job) {
        assert!(Instant::now() < deadline, "the job outlived its end");
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(host.kill("agterm-jobs", true), KillOutcome::Killed);
    drop(host);
    assert_eq!(server.stopped()?, StopReason::Idle);
    Ok(())
}
