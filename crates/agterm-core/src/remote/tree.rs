//! `zmx.tree`: the attachable sessions, this app's own or another machine's (upstream's `RemoteTree.swift`, the
//! `remoteTree`/`localAttachableSessions` half of `agterm/Control/ControlServer+Zmx.swift`, and the parts of
//! `RemoteSession.swift` and `RemoteCommandProcessRunner.swift` the remote form runs on).
//!
//! The local form joins every open window's projection against the daemon inventory: only a session whose every pane
//! resolves to a claimed, running daemon is offered, since `zmx attach` creates a missing daemon and would hand back a
//! fresh shell wearing the session's name. The remote form runs that same bare form on the far side over ssh and reads
//! its one document back.

use std::io::{Read, Seek, SeekFrom};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use agterm_proto::{
    ControlRemotePane, ControlRemoteSession, ControlRemoteTree, ControlResult, ControlSessionNode,
    ControlWorkspaceNode, ControlZmxInventory, Response,
};

use super::frames;
use crate::command_restore::shell_quoted_line;
use crate::zmx::is_daemon_name;

/// How long the remote form waits for the far side's answer (upstream's `remoteTreeDeadline`).
pub const TREE_DEADLINE: Duration = Duration::from_secs(10);

/// A host or remote-session token: not empty, no whitespace, no control characters (upstream's
/// `RemoteSession.isPlain`). One predicate decides what may reach both an argv and an error message.
#[must_use]
pub fn is_plain(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || u32::from(ch) < 0x20 || u32::from(ch) == 0x7f)
}

/// Whether `value` may be handed to ssh as its destination: plain, and no leading `-` that ssh would read as an option
/// (upstream's `RemoteSession.isHost`).
#[must_use]
pub fn is_host(value: &str) -> bool {
    is_plain(value) && !value.starts_with('-')
}

/// Why an ssh invocation could not be built (upstream's `RemoteSession.InvocationError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationError {
    EmptyHost,
    InvalidHost,
}

fn validate(host: &str) -> Result<(), InvocationError> {
    if host.is_empty() {
        return Err(InvocationError::EmptyHost);
    }
    if !is_host(host) {
        return Err(InvocationError::InvalidHost);
    }
    Ok(())
}

/// sshd runs a remote command with a bare system PATH and a non-interactive shell reads no profile, so an installed
/// CLI is otherwise not found. The Linux build's install directories are appended, so a user's own `agtermctl` earlier
/// on PATH still wins (upstream's `cliPathPrefix`, with `CommandPath.standardDirectories` for Linux).
#[must_use]
pub fn cli_path_prefix() -> String {
    "PATH=\"$PATH:$HOME/.local/bin:/usr/local/bin\"".to_owned()
}

/// The ssh options every invocation carries: no pty unless `interactive`, never a prompt, a bounded handshake.
#[must_use]
pub fn ssh_arguments(host: &str, connect_timeout: u32, interactive: bool) -> Vec<String> {
    vec![
        "ssh".to_owned(),
        if interactive { "-tt" } else { "-T" }.to_owned(),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "-o".to_owned(),
        format!("ConnectTimeout={connect_timeout}"),
        host.to_owned(),
    ]
}

/// One ssh invocation running the far side's own bare `zmx tree`, which answers across every open window in one
/// document (upstream's `RemoteSession.treeCommand`). The chain is wrapped in `/bin/sh -c`, since sshd runs it through
/// the account's shell, where a bare `VAR=value` is a syntax error in fish.
///
/// # Errors
///
/// The host is empty or not something ssh may take as a destination.
pub fn tree_command(host: &str) -> Result<Vec<String>, InvocationError> {
    validate(host)?;
    let chain = format!("{} && agtermctl zmx tree --json", cli_path_prefix());
    let remote = shell_quoted_line(&["/bin/sh", "-c", chain.as_str()]);
    let mut argv = ssh_arguments(host, 5, false);
    argv.push(remote);
    Ok(argv)
}

/// What running a remote command produced. `status` is ssh's exit code, which carries the far side's own once the
/// connection itself succeeded; -1 when it never ran or did not finish in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCommandResult {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl RemoteCommandResult {
    fn failed(stderr: impl Into<String>) -> Self {
        Self {
            status: -1,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }
}

/// Runs an ssh invocation off the main thread (upstream's `RemoteCommandRunner`). The app installs
/// [`ProcessRemoteRunner`]; tests install a fake, which is what lets the remote form be covered without a second host.
pub trait RemoteCommandRunner: Send + Sync {
    /// Runs `argv` to its end or `deadline`, whichever comes first. Blocks.
    fn run(&self, argv: &[String], deadline: Duration) -> RemoteCommandResult;
}

/// Runs the command as a child process (upstream's `RemoteCommandProcessRunner`): through `/usr/bin/env`, so a user's
/// own `ssh` earlier on PATH wins; stdin closed; the deadline the caller's, since ssh's `ConnectTimeout` bounds only the
/// handshake.
///
/// The output goes to unlinked temporary files rather than pipes, read once the command exited: nothing blocks reading
/// it, so a background helper of a PATH-resolved ssh wrapper that keeps the output open strands no reader thread or
/// descriptor here (upstream cancels its pipe readers instead), and what it wrote by the exit is the answer.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRemoteRunner;

impl RemoteCommandRunner for ProcessRemoteRunner {
    fn run(&self, argv: &[String], deadline: Duration) -> RemoteCommandResult {
        let Some(executable) = argv.first() else {
            return RemoteCommandResult::failed("no command to run");
        };
        let could_not_run =
            |error: std::io::Error| RemoteCommandResult::failed(format!("could not run {executable}: {error}"));
        let (mut stdout, mut stderr) = match (tempfile::tempfile(), tempfile::tempfile()) {
            (Ok(stdout), Ok(stderr)) => (stdout, stderr),
            (Err(error), _) | (_, Err(error)) => return could_not_run(error),
        };
        let spawned = stdout.try_clone().and_then(|out| {
            let err = stderr.try_clone()?;
            Command::new("/usr/bin/env")
                .args(argv)
                .stdin(Stdio::null())
                .stdout(Stdio::from(out))
                .stderr(Stdio::from(err))
                .spawn()
        });
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => return could_not_run(error),
        };
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() < deadline => thread::sleep(Duration::from_millis(20)),
                Ok(None) | Err(_) => break None,
            }
        };
        let Some(status) = status else {
            end_child(&mut child);
            // the output stays empty on a miss: the caller prefers a remote error parsed from stdout, and a partial one
            // must not replace this diagnostic
            return RemoteCommandResult::failed("the remote did not answer in time");
        };
        RemoteCommandResult {
            status: status.code().unwrap_or(-1),
            stdout: read_capture(&mut stdout),
            stderr: read_capture(&mut stderr),
        }
    }
}

/// What a capture file holds from its start, decoded lossily.
fn read_capture(file: &mut std::fs::File) -> String {
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(0)).is_ok() {
        let _ = file.read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Kills and reaps a child past its deadline. Upstream terminates first and kills past a grace; the core stays free
/// of signal APIs, and an ssh client has nothing to flush.
fn end_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// One open window's projection, with the identity the tree payload does not carry.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteWindowProjection {
    pub id: String,
    pub name: String,
    pub tree: agterm_proto::ControlTree,
}

/// Why a session list could not be produced or read (upstream's `RemoteTreeMerger.MergeError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeError {
    /// The remote answered, but not with a readable `zmx tree` document: an older CLI, or a truncated read.
    MalformedProjection,
    /// The inventory does not say where zmx and its daemons live, so nothing can be attached.
    MissingEndpoint,
    /// The remote command itself failed; carries what it said.
    RemoteFailed(String),
}

impl MergeError {
    /// The error a caller reads.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::MalformedProjection => "the remote did not return a readable session list".to_owned(),
            Self::MissingEndpoint => "the remote agterm is too old to report its zmx endpoint".to_owned(),
            Self::RemoteFailed(reason) => reason.clone(),
        }
    }
}

/// The remote's own explanation for a nonzero exit, or `None` when it gave none (upstream's `remoteError(stdout:)`).
/// `agtermctl --json` prints a refusal to stdout and exits nonzero, so stderr is empty on exactly these failures.
#[must_use]
pub fn remote_error(stdout: &str) -> Option<String> {
    let response: Response<ControlResult> = serde_json::from_str(stdout).ok()?;
    if response.ok {
        return None;
    }
    response
        .error
        .map(|error| error.trim().to_owned())
        .filter(|error| !error.is_empty())
}

/// Reads back what the far side's own `zmx tree` printed (upstream's `decode(stdout:)`). The caller rejects a nonzero
/// exit before calling this, and stamps the host it was given onto the result.
///
/// # Errors
///
/// The output is not a response, the far side refused, or an older far side answered without a session list.
pub fn decode(stdout: &str) -> Result<ControlRemoteTree, MergeError> {
    let response: Response<ControlResult> =
        serde_json::from_str(stdout).map_err(|_| MergeError::MalformedProjection)?;
    if !response.ok {
        return Err(MergeError::RemoteFailed(
            response
                .error
                .unwrap_or_else(|| "the remote session list could not be read".to_owned()),
        ));
    }
    response
        .result
        .and_then(|result| result.remote)
        .ok_or(MergeError::MalformedProjection)
}

/// Every attachable session across `windows`, flat, each naming where it lives (upstream's `candidates`). An empty
/// list is a successful answer, which does not tell a store outside Live sessions from one with nothing eligible.
///
/// # Errors
///
/// The inventory carries no endpoint.
pub fn candidates(
    windows: &[RemoteWindowProjection],
    inventory: &ControlZmxInventory,
) -> Result<ControlRemoteTree, MergeError> {
    let endpoint = inventory.endpoint.clone().ok_or(MergeError::MissingEndpoint)?;
    let mut daemons: std::collections::HashMap<&str, std::collections::HashMap<&str, &str>> =
        std::collections::HashMap::new();
    for entry in &inventory.entries {
        if entry.state != "claimed" || entry.observation != "running" {
            continue;
        }
        let (Some(session), Some(pane)) = (entry.session_id.as_deref(), entry.pane.as_deref()) else {
            continue;
        };
        // every row returned must be attachable, so a name `zmx attach` would refuse never reaches the caller
        if !is_daemon_name(&entry.daemon) {
            continue;
        }
        daemons.entry(session).or_default().insert(pane, entry.daemon.as_str());
    }
    let none = std::collections::HashMap::new();
    let sessions = windows
        .iter()
        .flat_map(|window| {
            window
                .tree
                .workspaces
                .iter()
                .flat_map(move |workspace| workspace.sessions.iter().map(move |node| (window, workspace, node)))
        })
        .filter_map(|(window, workspace, node)| {
            remote_session(node, window, workspace, daemons.get(node.id.as_str()).unwrap_or(&none))
        })
        .collect();
    Ok(ControlRemoteTree {
        host: None,
        endpoint,
        sessions,
        presentation: Some(frames::VERSION),
    })
}

fn remote_session(
    node: &ControlSessionNode,
    window: &RemoteWindowProjection,
    workspace: &ControlWorkspaceNode,
    daemons: &std::collections::HashMap<&str, &str>,
) -> Option<ControlRemoteSession> {
    // a claimed, running daemon is no proof the owner shows it: a launch that fell back from live keeps its daemons
    // while showing ordinary shells, and `backedByZmx` is the store's own answer
    if node.backed_by_zmx != Some(true) {
        return None;
    }
    let pane = |role: &str, daemon: &str, foreground: Option<&Vec<String>>| ControlRemotePane {
        pane: role.to_owned(),
        daemon: daemon.to_owned(),
        foreground: foreground.cloned(),
    };
    let mut panes = vec![pane("left", daemons.get("left")?, node.foreground.as_ref())];
    let split = node.has_split == Some(true);
    if split {
        panes.push(pane("right", daemons.get("right")?, node.split_foreground.as_ref()));
    }
    Some(ControlRemoteSession {
        id: node.id.clone(),
        name: node.name.clone(),
        window_id: window.id.clone(),
        window_name: window.name.clone(),
        workspace_id: workspace.id.clone(),
        workspace_name: workspace.name.clone(),
        context: node.context.clone(),
        cwd: node.cwd.clone(),
        split_axis: if split { node.split_axis.clone() } else { None },
        panes,
    })
}

#[cfg(test)]
mod tests {
    use agterm_proto::{ControlTree, ControlZmxEndpoint};
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::zmx::daemon_name;

    fn endpoint() -> ControlZmxEndpoint {
        ControlZmxEndpoint {
            executable: "/opt/agterm/bin/zmx".to_owned(),
            socket_directory: "/tmp/agterm-zmx-abc".to_owned(),
        }
    }

    fn node(id: &str, name: &str, cwd: &str, split: bool, backed: Option<bool>) -> ControlSessionNode {
        let mut node: ControlSessionNode = serde_json::from_value(json!({
            "id": id, "name": name, "cwd": cwd, "active": false, "split": split, "overlay": false,
            "scratch": false, "flagged": false
        }))
        .unwrap();
        node.has_split = split.then_some(true);
        node.backed_by_zmx = backed;
        node.split_axis = split.then(|| "vertical".to_owned());
        node
    }

    fn workspace(id: &str, name: &str, sessions: Vec<ControlSessionNode>) -> ControlWorkspaceNode {
        let mut workspace: ControlWorkspaceNode =
            serde_json::from_value(json!({"id": id, "name": name, "active": true, "sessions": []})).unwrap();
        workspace.sessions = sessions;
        workspace
    }

    fn window(id: &str, name: &str, workspaces: Vec<ControlWorkspaceNode>) -> RemoteWindowProjection {
        let mut tree: ControlTree = serde_json::from_value(json!({"workspaces": []})).unwrap();
        tree.workspaces = workspaces;
        RemoteWindowProjection {
            id: id.to_owned(),
            name: name.to_owned(),
            tree,
        }
    }

    fn entry(session: &str, pane: &str, daemon: &str, state: &str, observation: &str) -> serde_json::Value {
        json!({"daemon": daemon, "state": state, "observation": observation, "sessionID": session, "pane": pane})
    }

    /// Built from raw JSON, so the fixture pins the wire shape rather than whatever the encoder emits.
    fn inventory(entries: &[serde_json::Value], with_endpoint: bool) -> ControlZmxInventory {
        let mut payload = json!({
            "restore": {"configured": "live", "requestedAtLaunch": "live", "active": "live", "restartRequired": false},
            "inventoryComplete": true,
            "entries": entries,
        });
        if with_endpoint {
            payload["endpoint"] = serde_json::to_value(endpoint()).unwrap();
        }
        serde_json::from_value(payload).unwrap()
    }

    fn running(session: &str, pane: &str, daemon: &str) -> serde_json::Value {
        entry(session, pane, daemon, "claimed", "running")
    }

    #[test]
    fn a_plain_session_resolves_to_one_left_daemon() {
        let left = daemon_name(Uuid::new_v4());
        let windows = [window(
            "w-1",
            "main",
            vec![workspace(
                "ws-1",
                "work",
                vec![node("s1", "build", "/repo", false, Some(true))],
            )],
        )];
        let tree = candidates(&windows, &inventory(&[running("s1", "left", &left)], true)).unwrap();
        assert_eq!(tree.host, None, "the local form sshed nowhere");
        assert_eq!(tree.endpoint, endpoint());
        assert_eq!(tree.presentation, Some(frames::VERSION));
        assert_eq!(tree.sessions.len(), 1);
        assert_eq!(
            tree.sessions[0].panes,
            vec![ControlRemotePane {
                pane: "left".to_owned(),
                daemon: left,
                foreground: None
            }]
        );
        assert_eq!(tree.sessions[0].split_axis, None);
        assert_eq!(
            (
                tree.sessions[0].window_name.as_str(),
                tree.sessions[0].workspace_name.as_str()
            ),
            ("main", "work")
        );
    }

    #[test]
    fn every_open_window_and_workspace_is_offered_apart_by_id() {
        let (first, second, third) = (
            daemon_name(Uuid::new_v4()),
            daemon_name(Uuid::new_v4()),
            daemon_name(Uuid::new_v4()),
        );
        let windows = [
            window(
                "w-1",
                "main",
                vec![
                    workspace("ws-1", "dev", vec![node("s1", "build", "/a", false, Some(true))]),
                    workspace("ws-2", "ops", vec![node("s2", "logs", "/var", false, Some(true))]),
                ],
            ),
            window(
                "w-2",
                "main",
                vec![workspace(
                    "ws-3",
                    "dev",
                    vec![node("s3", "build", "/b", false, Some(true))],
                )],
            ),
        ];
        let tree = candidates(
            &windows,
            &inventory(
                &[
                    running("s1", "left", &first),
                    running("s2", "left", &second),
                    running("s3", "left", &third),
                ],
                true,
            ),
        )
        .unwrap();
        let ids = |field: fn(&ControlRemoteSession) -> &str| tree.sessions.iter().map(field).collect::<Vec<_>>();
        assert_eq!(ids(|session| &session.id), ["s1", "s2", "s3"]);
        assert_eq!(ids(|session| &session.window_id), ["w-1", "w-1", "w-2"]);
        assert_eq!(ids(|session| &session.workspace_id), ["ws-1", "ws-2", "ws-3"]);
        assert_eq!(ids(|session| &session.workspace_name), ["dev", "ops", "dev"]);
    }

    #[test]
    fn a_pane_carries_what_runs_in_it_and_the_session_its_context() {
        let left = daemon_name(Uuid::new_v4());
        let mut busy = node("s1", "build", "/repo", false, Some(true));
        busy.foreground = Some(vec!["/usr/bin/tail".to_owned(), "-f".to_owned()]);
        busy.context = Some("release prep".to_owned());
        let windows = [window("w-1", "main", vec![workspace("ws-1", "work", vec![busy])])];
        let tree = candidates(&windows, &inventory(&[running("s1", "left", &left)], true)).unwrap();
        assert_eq!(tree.sessions[0].context.as_deref(), Some("release prep"));
        assert_eq!(
            tree.sessions[0].panes[0].foreground,
            Some(vec!["/usr/bin/tail".to_owned(), "-f".to_owned()])
        );
    }

    #[test]
    fn a_split_carries_both_daemons_and_its_axis_or_is_dropped_half_way() {
        let (left, right) = (daemon_name(Uuid::new_v4()), daemon_name(Uuid::new_v4()));
        let windows = [window(
            "w-1",
            "main",
            vec![workspace(
                "ws-1",
                "work",
                vec![node("s1", "build", "/repo", true, Some(true))],
            )],
        )];
        let both = candidates(
            &windows,
            &inventory(&[running("s1", "left", &left), running("s1", "right", &right)], true),
        )
        .unwrap();
        assert_eq!(
            both.sessions[0]
                .panes
                .iter()
                .map(|pane| (pane.pane.as_str(), pane.daemon.as_str()))
                .collect::<Vec<_>>(),
            [("left", left.as_str()), ("right", right.as_str())]
        );
        assert_eq!(both.sessions[0].split_axis.as_deref(), Some("vertical"));
        let half = candidates(&windows, &inventory(&[running("s1", "left", &left)], true)).unwrap();
        assert!(half.sessions.is_empty());
    }

    #[test]
    fn only_a_claimed_running_agterm_daemon_of_a_zmx_backed_session_is_offered() {
        let left = daemon_name(Uuid::new_v4());
        let backed = [window(
            "w-1",
            "main",
            vec![workspace(
                "ws-1",
                "work",
                vec![node("s1", "build", "/repo", false, Some(true))],
            )],
        )];
        for (state, observation) in [("orphan", "running"), ("claimed", "absent"), ("claimed", "unreadable")] {
            let tree = candidates(
                &backed,
                &inventory(&[entry("s1", "left", &left, state, observation)], true),
            )
            .unwrap();
            assert!(tree.sessions.is_empty(), "{state} {observation}");
        }
        let foreign = candidates(&backed, &inventory(&[running("s1", "left", "notes")], true)).unwrap();
        assert!(foreign.sessions.is_empty(), "a name outside agterm's scheme");
        let plain = [window(
            "w-1",
            "main",
            vec![workspace(
                "ws-1",
                "work",
                vec![node("s1", "build", "/repo", false, Some(false))],
            )],
        )];
        let fallback = candidates(&plain, &inventory(&[running("s1", "left", &left)], true)).unwrap();
        assert!(fallback.sessions.is_empty(), "a session not backed by zmx");
        let other = daemon_name(Uuid::new_v4());
        let stray = candidates(
            &backed,
            &inventory(&[running("s1", "left", &left), running("gone", "left", &other)], true),
        )
        .unwrap();
        assert_eq!(stray.sessions.len(), 1, "a row naming no listed session is ignored");
    }

    #[test]
    fn listing_errors_are_named() {
        assert_eq!(
            candidates(&[], &inventory(&[], false)),
            Err(MergeError::MissingEndpoint)
        );
        assert_eq!(
            MergeError::MissingEndpoint.message(),
            "the remote agterm is too old to report its zmx endpoint"
        );
        let empty = candidates(&[], &inventory(&[], true)).unwrap();
        assert!(
            empty.sessions.is_empty(),
            "no windows is an empty answer, not a failure"
        );
        assert_eq!(
            decode("command not found: agtermctl"),
            Err(MergeError::MalformedProjection)
        );
        assert_eq!(
            MergeError::MalformedProjection.message(),
            "the remote did not return a readable session list"
        );
        assert_eq!(decode(r#"{"ok":true}"#), Err(MergeError::MalformedProjection));
        assert_eq!(
            decode(r#"{"ok":false,"error":"zmx is not available"}"#),
            Err(MergeError::RemoteFailed("zmx is not available".to_owned()))
        );
        assert_eq!(
            decode(r#"{"ok":false}"#).map_err(|error| error.message()),
            Err("the remote session list could not be read".to_owned())
        );
    }

    #[test]
    fn the_far_sides_answer_is_decoded_whole_and_stamped() {
        let left = daemon_name(Uuid::new_v4());
        let remote = ControlRemoteTree {
            host: None,
            endpoint: endpoint(),
            sessions: vec![ControlRemoteSession {
                id: "s1".to_owned(),
                name: "build".to_owned(),
                window_id: "w-1".to_owned(),
                window_name: "main".to_owned(),
                workspace_id: "ws-1".to_owned(),
                workspace_name: "work".to_owned(),
                context: None,
                cwd: "/repo".to_owned(),
                split_axis: None,
                panes: vec![ControlRemotePane {
                    pane: "left".to_owned(),
                    daemon: left,
                    foreground: None,
                }],
            }],
            presentation: Some(1),
        };
        let stdout = serde_json::to_string(&json!({"ok": true, "result": {"remote": remote}})).unwrap();
        let decoded = decode(&stdout).unwrap();
        assert_eq!(decoded, remote);
        let stamped = decoded.stamped("buildbox");
        assert_eq!(
            stamped,
            ControlRemoteTree {
                host: Some("buildbox".to_owned()),
                ..remote
            }
        );
        let older =
            r#"{"ok":true,"result":{"remote":{"endpoint":{"executable":"/z","socketDirectory":"/t"},"sessions":[]}}}"#;
        assert_eq!(decode(older).unwrap().presentation, None);
    }

    #[test]
    fn the_remotes_own_refusal_is_read_from_stdout() {
        assert_eq!(
            remote_error(r#"{"ok":false,"error":"could not read the zmx session list"}"#).as_deref(),
            Some("could not read the zmx session list")
        );
        assert_eq!(remote_error(r#"{"ok":false,"error":"  "}"#), None);
        assert_eq!(
            remote_error(
                r#"{"ok":true,"result":{"remote":{"endpoint":{"executable":"/z","socketDirectory":"/t"},"sessions":[]}}}"#
            ),
            None
        );
        assert_eq!(remote_error("garbage"), None);
    }

    #[test]
    fn the_tree_command_runs_the_bare_form_through_sh_and_refuses_odd_hosts() {
        let argv = tree_command("buildbox").unwrap();
        assert_eq!(
            argv[..7],
            ["ssh", "-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "buildbox"]
        );
        assert_eq!(
            argv[7],
            r#"'/bin/sh' '-c' 'PATH="$PATH:$HOME/.local/bin:/usr/local/bin" && agtermctl zmx tree --json'"#
        );
        assert_eq!(tree_command(""), Err(InvocationError::EmptyHost));
        for host in ["-oProxyCommand=x", "bad host", "ho\u{7f}st", "a\u{1b}[31m"] {
            assert_eq!(tree_command(host), Err(InvocationError::InvalidHost), "{host:?}");
        }
        assert!(is_plain("user@host.example"));
        assert!(!is_plain("\u{a0}nbsp"));
    }

    #[test]
    fn the_process_runner_captures_output_and_bounds_the_wait() {
        let runner = ProcessRemoteRunner;
        let argv = |line: &str| vec!["/bin/sh".to_owned(), "-c".to_owned(), line.to_owned()];
        let done = runner.run(&argv("echo out; echo err >&2; exit 3"), Duration::from_secs(5));
        assert_eq!(
            done,
            RemoteCommandResult {
                status: 3,
                stdout: "out\n".to_owned(),
                stderr: "err\n".to_owned()
            }
        );
        // a background helper keeping the output open neither delays the answer nor strands a reader
        let started = Instant::now();
        let lingering = runner.run(&argv("echo out; sleep 5 & exit 0"), Duration::from_secs(5));
        assert_eq!((lingering.status, lingering.stdout.as_str()), (0, "out\n"));
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        let slow = runner.run(&argv("sleep 5"), Duration::from_millis(100));
        assert_eq!(slow, RemoteCommandResult::failed("the remote did not answer in time"));
        let missing = runner.run(&["/nonexistent/ssh".to_owned()], Duration::from_secs(5));
        assert_eq!(missing.status, 127, "env reports a missing program");
        assert_eq!(runner.run(&[], Duration::from_secs(1)).stderr, "no command to run");
    }
}
