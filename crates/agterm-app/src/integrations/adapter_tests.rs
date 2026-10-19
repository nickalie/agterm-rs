//! The Claude adapter script run for real: what it forwards to the shared wrapper for a hook's argv and payload. A
//! stub wrapper records the forwarded argv; nothing reaches an app.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const IN_FLIGHT: &str =
    r#"[{"id":"b1","type":"shell","status":"running","description":"cargo test","command":"cargo test"}]"#;

/// A hook payload as Claude Code writes it: compact JSON, the common fields first.
fn payload(event: &str, message: &str, tasks: &str) -> String {
    format!(
        r#"{{"session_id":"s","hook_event_name":"{event}","stop_hook_active":false,"last_assistant_message":"{message}","background_tasks":{tasks},"session_crons":[]}}"#
    )
}

fn adapter() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/agent-status/agterm-claude-status.sh")
}

/// The argv the adapter hands the wrapper for `args` with `payload` on stdin. The adapter starts only once its
/// launching shell is reaped, so it is an orphan whose ancestor walk never meets the agent that may be running these
/// tests (a second agent above the hook would make it a silent worker).
fn forwarded(args: &[&str], payload: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("wrapper.sh");
    fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s' \"$*\" > \"$OUT.tmp\" && mv \"$OUT.tmp\" \"$OUT\"\n",
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let out = dir.path().join("argv");
    let input = dir.path().join("payload.json");
    fs::write(&input, payload).unwrap();
    let launch = r#"(while kill -0 $$ 2>/dev/null; do :; done; exec bash "$0" "$@" <"$PAYLOAD" >/dev/null 2>&1) &"#;
    let status = Command::new("sh")
        .args(["-c", launch])
        .arg(adapter())
        .args(args)
        .env("PAYLOAD", &input)
        .env("OUT", &out)
        .env("AGTERM_STATUS_WRAPPER", &stub)
        .env("AGTERM_SESSION_ID", "S")
        .env_remove("AGTERM_AGENT_RE")
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(argv) = fs::read_to_string(&out) {
            return argv;
        }
        assert!(
            Instant::now() < deadline,
            "the adapter never called the wrapper for {args:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_stop_with_background_work_in_flight_stays_active() {
    let stop = payload("Stop", "started the tests", IN_FLIGHT);
    assert_eq!(forwarded(&["completed", "--auto-reset"], &stop), "active --blink");
    let spaced = stop
        .replace(r#""hook_event_name":"#, r#""hook_event_name": "#)
        .replace(r#""background_tasks":["#, "\"background_tasks\": [\n  ");
    assert_eq!(forwarded(&["completed", "--auto-reset"], &spaced), "active --blink");
}

#[test]
fn a_stop_with_nothing_in_flight_completes() {
    let done = payload("Stop", "all green", "[]");
    assert_eq!(
        forwarded(&["completed", "--auto-reset"], &done),
        "completed --auto-reset"
    );
    // a key quoted in the last message is escaped, so it is not the payload's list
    let quoted = payload("Stop", r#"saw \"background_tasks\":[{\"id\":1}] in the log"#, "[]");
    assert_eq!(
        forwarded(&["completed", "--auto-reset"], &quoted),
        "completed --auto-reset"
    );
    // an older Claude Code sends no list at all
    assert_eq!(
        forwarded(&["completed", "--auto-reset"], r#"{"hook_event_name":"Stop"}"#),
        "completed --auto-reset"
    );
}

#[test]
fn other_states_and_events_forward_verbatim() {
    let stop = payload("Stop", "started the tests", IN_FLIGHT);
    assert_eq!(forwarded(&["active", "--blink"], &stop), "active --blink");
    assert_eq!(forwarded(&["blocked"], &stop), "blocked");
    let subagent = payload("SubagentStop", "started the tests", IN_FLIGHT);
    assert_eq!(
        forwarded(&["completed", "--auto-reset"], &subagent),
        "completed --auto-reset"
    );
    assert_eq!(forwarded(&["completed", "--auto-reset"], ""), "completed --auto-reset");
}
