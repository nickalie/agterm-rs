//! Upstream's `agtermTests/ZmxClientTests.swift`, the invocation and runner half.

use std::sync::{Arc, Mutex};

use super::*;

const NAME: &str = "agterm-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

type Log = Arc<Mutex<Vec<Invocation>>>;

/// A client whose runner records every invocation and answers with `reply`.
fn recording(reply: impl Fn(&Invocation) -> Result<String, CommandError> + Send + Sync + 'static) -> (ZmxClient, Log) {
    let log = Log::default();
    let sink = Arc::clone(&log);
    let runner: Runner = Box::new(move |invocation| {
        sink.lock().unwrap().push(invocation.clone());
        reply(invocation)
    });
    (
        ZmxClient::with_runner("/tmp/zmx", "/tmp/zmx-dir", Duration::from_millis(1500), runner),
        log,
    )
}

fn arguments(log: &Log) -> Vec<Vec<String>> {
    log.lock()
        .unwrap()
        .iter()
        .map(|invocation| invocation.arguments.clone())
        .collect()
}

#[test]
fn the_listing_invokes_only_list_in_the_instance_namespace() {
    let (client, log) = recording(|_| {
        Ok(format!(
            "name={NAME}\tpid=1\tclients=0\tcreated=1\nname=agterm-busy\terr=Timeout\tstatus=unreachable\n"
        ))
    });
    let records = client.list().unwrap();
    assert_eq!(arguments(&log), [["list"]], "the read must mutate nothing");
    let invocation = log.lock().unwrap()[0].clone();
    assert_eq!(
        invocation.environment,
        [
            ("ZMX_DIR".to_owned(), Some("/tmp/zmx-dir".to_owned())),
            ("ZMX_SESSION".to_owned(), None),
            ("ZMX_SESSION_PREFIX".to_owned(), None)
        ],
        "the app must not inherit a pane's own session into its listing"
    );
    assert_eq!(
        (invocation.timeout, invocation.merges_stderr),
        (Duration::from_millis(1500), false)
    );
    assert_eq!(
        records.iter().map(|record| record.name.as_str()).collect::<Vec<_>>(),
        [NAME, "agterm-busy"]
    );
    assert_eq!((records[0].clients, records[1].clients), (Some(0), None));
}

#[test]
fn an_empty_namespace_is_an_answer_and_a_failure_is_not() {
    assert_eq!(recording(|_| Ok(String::new())).0.list(), Ok(Vec::new()));
    assert!(recording(|_| Err(CommandError::TimedOut)).0.list().is_err());
    assert!(
        recording(|_| Ok("pid=10\tclients=0\n".to_owned())).0.list().is_err(),
        "an unparseable line must not read as an empty namespace"
    );
}

#[test]
fn a_listing_can_override_only_its_own_timeout() {
    let (client, log) = recording(|_| Ok(String::new()));
    client.session_records(Some(CAPTURE_INVOCATION_TIMEOUT)).unwrap();
    client.list().unwrap();
    let timeouts: Vec<Duration> = log
        .lock()
        .unwrap()
        .iter()
        .map(|invocation| invocation.timeout)
        .collect();
    assert_eq!(timeouts, [CAPTURE_INVOCATION_TIMEOUT, Duration::from_millis(1500)]);
}

#[test]
fn the_kill_outcome_trusts_only_an_exact_confirmation_line() {
    assert_eq!(
        kill_outcome(&format!("killed session {NAME}\n"), NAME),
        KillOutcome::Killed
    );
    assert_eq!(
        kill_outcome(&format!("cleaned up stale session {NAME}\n"), NAME),
        KillOutcome::StaleSocket
    );
    // zmx exits zero here too, after merely reporting on stderr it could not reach the daemon
    let unresponsive = format!("failed to kill session={NAME}: SessionUnresponsive\n");
    assert_eq!(
        kill_outcome(&unresponsive, NAME),
        KillOutcome::Failed(format!("failed to kill session={NAME}: SessionUnresponsive"))
    );
    // a broken pipe after the kill was sent returns with nothing printed
    assert_eq!(kill_outcome("", NAME), KillOutcome::Failed("no output".to_owned()));
    assert_eq!(
        kill_outcome(&format!("not killed session {NAME}\n"), NAME),
        KillOutcome::Failed(format!("not killed session {NAME}")),
        "a line merely containing the confirmation must not count as one"
    );
    assert_eq!(
        kill_outcome("killed session agterm-other\n", NAME),
        KillOutcome::Failed("killed session agterm-other".to_owned()),
        "another daemon's confirmation is not this one's"
    );
}

#[test]
fn a_kill_merges_stderr_forces_only_when_asked_and_separates_a_stale_socket() {
    let (client, log) = recording(|invocation| {
        Ok(if invocation.arguments.contains(&"agterm-gone".to_owned()) {
            "cleaned up stale session agterm-gone\n".to_owned()
        } else {
            "killed session agterm-live\n".to_owned()
        })
    });
    assert_eq!(client.kill("agterm-live", false), KillOutcome::Killed);
    assert_eq!(client.kill("agterm-gone", true), KillOutcome::StaleSocket);
    assert_eq!(
        arguments(&log),
        [vec!["kill", "agterm-live"], vec!["kill", "agterm-gone", "--force"]]
    );
    assert!(log.lock().unwrap().iter().all(|invocation| invocation.merges_stderr));
    let (failing, _) = recording(|_| Err(CommandError::TimedOut));
    assert_eq!(failing.kill(NAME, true), KillOutcome::Failed("timed out".to_owned()));
}

#[test]
fn the_screen_reads_the_daemons_terminal_and_a_failure_is_never_a_blank_screen() {
    let (client, log) = recording(|_| Ok("7 101 33 2 9 0\n> draft\n".to_owned()));
    let active = client.screen("agterm-a", false).unwrap();
    client.screen("agterm-a", true).unwrap();
    assert_eq!(
        arguments(&log),
        [vec!["screen", "agterm-a"], vec!["screen", "agterm-a", "--all"]]
    );
    assert_eq!(
        (active.columns, active.cursor_column, active.text.as_str()),
        (101, 2, "> draft\n")
    );
    let old = recording(|_| Err(CommandError::Failed(1, "error: screen failed".to_owned()))).0;
    assert!(
        old.screen("agterm-a", false).is_err(),
        "a zmx without the query must not read as blank"
    );
    assert!(
        recording(|_| Ok("usage: zmx".to_owned()))
            .0
            .screen("agterm-a", false)
            .is_err()
    );
}

#[test]
fn typing_sends_the_exact_bytes_on_stdin_and_reports_a_rejection() {
    let (client, log) = recording(|_| Ok(String::new()));
    client.type_input("agterm-a", b"\r").unwrap();
    assert_eq!(arguments(&log), [["type", "agterm-a"]]);
    assert_eq!(
        log.lock().unwrap()[0].input.as_deref(),
        Some(&b"\r"[..]),
        "a lone Return must reach zmx"
    );
    let full = recording(|_| Err(CommandError::Failed(1, "error: type rejected".to_owned()))).0;
    assert!(full.type_input("agterm-a", b"a").is_err());
}

#[test]
fn availability_is_an_executable_zmx() {
    assert!(!ZmxClient::new("/nonexistent/zmx", "/tmp/zmx-dir").is_available());
    assert!(ZmxClient::new("/bin/sh", "/tmp/zmx-dir").is_available());
}

fn shell(script: &str, merges_stderr: bool, input: Option<&[u8]>) -> Invocation {
    Invocation {
        executable: PathBuf::from("/bin/sh"),
        arguments: vec!["-c".to_owned(), script.to_owned()],
        environment: Vec::new(),
        timeout: Duration::from_secs(5),
        merges_stderr,
        input: input.map(<[u8]>::to_vec),
    }
}

#[test]
fn run_feeds_input_to_the_childs_stdin_and_closes_it() {
    assert_eq!(run(&shell("cat", false, Some(b"typed\r"))).unwrap(), "typed\r");
}

#[test]
fn run_appends_stderr_only_when_asked_and_applies_the_environment() {
    assert_eq!(run(&shell("echo notice >&2; echo row", false, None)).unwrap(), "row\n");
    assert_eq!(
        run(&shell("echo notice >&2; echo row", true, None)).unwrap(),
        "row\nnotice\n"
    );
    let mut invocation = shell("echo \"$ZMX_DIR:${HOME-unset}\"", false, None);
    invocation.environment = vec![
        ("ZMX_DIR".to_owned(), Some("/tmp/x".to_owned())),
        ("HOME".to_owned(), None),
    ];
    assert_eq!(run(&invocation).unwrap(), "/tmp/x:unset\n");
}

#[test]
fn run_drains_output_larger_than_the_pipe_buffer_before_the_process_exits() {
    let started = Instant::now();
    let output = run(&shell(
        "head -c 200000 /dev/zero | tr '\\0' x; head -c 200000 /dev/zero | tr '\\0' y >&2",
        true,
        None,
    ))
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(output.len(), 400_000);
    assert!(output.starts_with('x') && output.ends_with('y'));
}

#[test]
fn run_reports_a_nonzero_exit_with_its_output() {
    match run(&shell("echo out; echo err >&2; exit 3", false, None)) {
        Err(CommandError::Failed(3, output)) => assert_eq!(output, "out\nerr\n"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn run_gives_up_on_a_write_end_held_past_the_childs_exit() {
    let started = Instant::now();
    let result = run(&shell("sleep 5 & echo row", false, None));
    assert!(matches!(result, Err(CommandError::TimedOut)), "{result:?}");
    assert!(started.elapsed() < TERMINATION_GRACE + Duration::from_secs(1));
}

#[test]
fn run_terminates_a_child_past_its_timeout_within_the_grace() {
    let mut invocation = shell("exec sleep 30", false, None);
    invocation.timeout = CAPTURE_INVOCATION_TIMEOUT;
    let started = Instant::now();
    assert!(matches!(run(&invocation), Err(CommandError::TimedOut)));
    assert!(started.elapsed() < CAPTURE_INVOCATION_TIMEOUT + TERMINATION_GRACE + Duration::from_millis(500));
    // a child that ignores SIGTERM is killed after the grace
    let mut stubborn = shell("trap '' TERM; while :; do sleep 0.01; done", false, None);
    stubborn.timeout = CAPTURE_INVOCATION_TIMEOUT;
    let started = Instant::now();
    assert!(matches!(run(&stubborn), Err(CommandError::TimedOut)));
    assert!(started.elapsed() < CAPTURE_INVOCATION_TIMEOUT + 2 * TERMINATION_GRACE + Duration::from_secs(1));
}

#[test]
fn run_fails_at_once_for_an_executable_that_cannot_start() {
    let invocation = Invocation {
        executable: PathBuf::from("/nonexistent/zmx"),
        arguments: vec!["list".to_owned()],
        environment: Vec::new(),
        timeout: Duration::from_secs(5),
        merges_stderr: false,
        input: None,
    };
    assert!(matches!(run(&invocation), Err(CommandError::Launch(_))));
}
