//! The restore adapter run for real: which `ngtermctl session restore` calls it makes for a hook's event, payload and
//! environment. A stub `ngtermctl` records each call's argv; the environment is built from scratch, so no `NGTERM_*`
//! variable of a live instance reaches the script and nothing talks to an app.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ID: &str = "0f8a4c3e-6b1d-4e2a-9c7f-1a2b3c4d5e6f";
const SESSION: &str = "06882D55-AC80-4B1B-8BBE-682FDDDEC95A";
const PANE: &str = "05E0AB65-271B-4389-9946-C3A7E2060CAC";
const SOCKET: &str = "/nonexistent/ngterm-restore-test.sock";

fn adapter() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/agent-status/ngterm-agent-restore.sh")
}

/// A Claude Code hook payload as it writes one: compact JSON, the common fields first.
fn claude_payload(event: &str, extra: &str) -> String {
    format!(
        r#"{{"session_id":"{ID}","transcript_path":"/home/u/.claude/projects/-p/{ID}.jsonl","cwd":"/p","hook_event_name":"{event}"{extra}}}"#
    )
}

/// A scratch directory with a stub `ngtermctl` that appends each call's arguments, tab-separated, to `calls`.
struct Bench {
    dir: tempfile::TempDir,
}

impl Bench {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("ngtermctl");
        fs::write(
            &stub,
            "#!/usr/bin/env bash\n(IFS=$'\\t'; printf '%s\\n' \"$*\") >> \"$(dirname \"$0\")/calls\"\n",
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The calls the stub recorded so far, each as its argument list.
    fn calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.path("calls"))
            .unwrap_or_default()
            .lines()
            .map(|line| line.split('\t').map(str::to_owned).collect())
            .collect()
    }

    /// A pane's environment from scratch: the pane variables, the stub, a private TMPDIR, plus `vars`.
    fn command(&self, program: &str, vars: &[(&str, &str)]) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("NGTERMCTL", self.path("ngtermctl"))
            .env("TMPDIR", self.dir.path())
            .env("NGTERM_SESSION_ID", SESSION)
            .env("NGTERM_PANE_ID", PANE)
            .env("NGTERM_SOCKET", SOCKET);
        for (key, value) in vars {
            command.env(key, value);
        }
        command
    }

    fn run(mut command: Command, payload: &str) -> Output {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(payload.as_bytes()).unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "the hook exits 0: {output:?}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "the hook is silent: {output:?}"
        );
        output
    }

    /// The adapter as Claude Code runs it: a child of the agent, `vars` on top of the pane's environment.
    fn claude(&self, event: &str, payload: &str, vars: &[(&str, &str)]) -> Vec<Vec<String>> {
        let mut command = self.command("bash", vars);
        command.arg(adapter()).args(["claude", event]);
        Self::run(command, payload);
        self.calls()
    }
}

fn pin(line: &str) -> Vec<String> {
    [
        "session",
        "restore",
        line,
        "--target",
        SESSION,
        "--pane-id",
        PANE,
        "--socket",
        SOCKET,
    ]
    .map(str::to_owned)
    .to_vec()
}

fn clear() -> Vec<String> {
    pin("--clear")
}

const CLI: (&str, &str) = ("CLAUDE_CODE_ENTRYPOINT", "cli");

#[test]
fn a_claude_start_pins_the_live_conversation() {
    let bench = Bench::new();
    for source in ["startup", "resume", "clear", "compact"] {
        let payload = claude_payload("SessionStart", &format!(r#","source":"{source}","model":"m""#));
        bench.claude("start", &payload, &[CLI]);
    }
    let line = format!("claude --resume {ID} || claude");
    assert_eq!(bench.calls(), vec![pin(&line); 4]);
}

#[test]
fn a_claude_start_without_a_pane_id_or_socket_targets_the_session_alone() {
    let bench = Bench::new();
    let mut command = bench.command("bash", &[CLI]);
    command
        .env_remove("NGTERM_PANE_ID")
        .env_remove("NGTERM_SOCKET")
        .arg(adapter())
        .args(["claude", "start"]);
    Bench::run(command, &claude_payload("SessionStart", ""));
    let line = format!("claude --resume {ID} || claude");
    assert_eq!(
        bench.calls(),
        [["session", "restore", line.as_str(), "--target", SESSION]
            .map(str::to_owned)
            .to_vec()]
    );
}

/// The pin is shell code typed into the login shell: anything but a UUID-shaped hex id leaves the pin alone.
#[test]
fn a_claude_start_with_a_bad_id_pins_nothing() {
    let bench = Bench::new();
    let bad = [
        String::new(),
        r#"{"hook_event_name":"SessionStart"}"#.to_owned(),
        r#"{"session_id":"","hook_event_name":"SessionStart"}"#.to_owned(),
        r#"{"session_id":"abc123","hook_event_name":"SessionStart"}"#.to_owned(),
        r#"{"session_id":"0f8a4c3e-6b1d-4e2a-9c7f-1a2b3c4d5e6g","hook_event_name":"SessionStart"}"#.to_owned(),
        r#"{"session_id":"0f8a4c3e-6b1d-4e2a-9c7f-1a2b3c4d5e6f; rm -rf ~","hook_event_name":"SessionStart"}"#
            .to_owned(),
        r#"{"session_id":"$(touch /tmp/x)","hook_event_name":"SessionStart"}"#.to_owned(),
        r#"{"session_id":"served:0f8a4c3e-6b1d-4e2a-9c7f-1a2b3c4d5e6f","hook_event_name":"SessionStart"}"#.to_owned(),
        r#"{"session_id":"0f8a4c3e-6b1d-4e2a-9c7f-1a2b3c4d5e6f\" || evil \"","hook_event_name":"SessionStart"}"#
            .to_owned(),
        r#"{"session_id":7,"hook_event_name":"SessionStart"}"#.to_owned(),
        // an id quoted inside a string value is escaped, so it is not the payload's
        format!(r#"{{"last":"\"session_id\":\"{ID}\"","hook_event_name":"SessionStart"}}"#),
    ];
    for payload in &bad {
        assert_eq!(
            bench.claude("start", payload, &[CLI]),
            Vec::<Vec<String>>::new(),
            "{payload}"
        );
    }
}

/// `claude -p` (spawned by a tool call of the pane's claude, it inherits `cli` and Claude Code rewrites it to
/// `sdk-cli` before any hook), Agent SDK runs and subagents never pin over the interactive conversation.
#[test]
fn a_non_interactive_claude_or_a_subagent_pins_nothing() {
    let bench = Bench::new();
    let start = claude_payload("SessionStart", r#","source":"startup""#);
    for entrypoint in ["sdk-cli", "sdk-ts", "sdk-py", "mcp", "claude-vscode", ""] {
        bench.claude("start", &start, &[("CLAUDE_CODE_ENTRYPOINT", entrypoint)]);
    }
    bench.claude("start", &start, &[]);
    let subagent = claude_payload("SessionStart", r#","agent_id":"a1b2","agent_type":"Explore""#);
    bench.claude("start", &subagent, &[CLI]);
    let exit = claude_payload("SessionEnd", r#","reason":"prompt_input_exit""#);
    bench.claude("end", &exit, &[("CLAUDE_CODE_ENTRYPOINT", "sdk-cli")]);
    assert_eq!(bench.calls(), Vec::<Vec<String>>::new());
}

#[test]
fn a_claude_end_clears_the_pin_only_on_a_deliberate_exit() {
    let bench = Bench::new();
    for reason in ["other", "clear", "resume", "bypass_permissions_disabled"] {
        let payload = claude_payload("SessionEnd", &format!(r#","reason":"{reason}""#));
        assert_eq!(
            bench.claude("end", &payload, &[CLI]),
            Vec::<Vec<String>>::new(),
            "{reason}"
        );
    }
    assert_eq!(
        bench.claude("end", &claude_payload("SessionEnd", ""), &[CLI]),
        Vec::<Vec<String>>::new()
    );
    for reason in ["prompt_input_exit", "logout"] {
        let payload = claude_payload("SessionEnd", &format!(r#","reason":"{reason}""#));
        bench.claude("end", &payload, &[CLI]);
    }
    assert_eq!(bench.calls(), [clear(), clear()]);
}

#[test]
fn outside_ngterm_the_adapter_does_nothing() {
    let bench = Bench::new();
    for (agent, event, payload) in [
        ("claude", "start", claude_payload("SessionStart", "")),
        ("claude", "end", claude_payload("SessionEnd", r#","reason":"logout""#)),
    ] {
        let mut command = bench.command("bash", &[CLI]);
        command
            .env_remove("NGTERM_SESSION_ID")
            .arg(adapter())
            .args([agent, event]);
        Bench::run(command, &payload);
    }
    let mut command = bench.command("bash", &[CLI]);
    command.arg(adapter());
    Bench::run(command, "");
    assert_eq!(bench.calls(), Vec::<Vec<String>>::new());
}
