//! The ssh command lines a viewer runs against another machine's agterm (the attach half of upstream's
//! `RemoteSession.swift`; `live tree`'s own line lives in [`super::tree`]). Pure and host-free: callers run what these
//! return.

use agterm_proto::ControlLiveEndpoint;

use super::reconnect::RemoteLinkNotice;
use super::tree::{InvocationError, REMOTE_CLI, is_plain, remote_cli_chain, ssh_arguments, validate};
use crate::command_restore::shell_quoted_line;
use crate::live::is_daemon_name;
use crate::live::lead::LeadAttachment;

/// A filesystem path, which may hold spaces: only control characters and NUL are refused, neither of which survives a
/// path anyway (upstream's `isPath`).
fn is_path(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(|ch| u32::from(ch) < 0x20 || u32::from(ch) == 0x7f)
}

/// One ssh invocation carrying `session`'s presentation stream for as long as the row is shown (upstream's
/// `presentCommand`): `-T`, as frames travel on plain stdio a pty would mangle, no lifetime deadline, and `exec` so the
/// far shell does not linger between ssh and the bridge.
///
/// # Errors
///
/// A host ssh may not take, or a session id that is not one plain token.
pub fn present_command(host: &str, session: &str) -> Result<Vec<String>, InvocationError> {
    validate(host)?;
    if !is_plain(session) {
        return Err(InvocationError::InvalidSession);
    }
    let tail = format!("exec {REMOTE_CLI} live present {}", shell_quoted_line(&[session]));
    let mut argv = ssh_arguments(host, 5, false);
    argv.push(remote_cli_chain(&tail));
    Ok(argv)
}

/// One ssh invocation running the helper of `job`, an overlay the origin handed to this machine, for as long as the
/// overlay is up (upstream's `runJobCommand`): `-tt`, as the program is the user's and reads the pty, and `exec` so the
/// helper's hangup check sees the ssh session's own pty close.
///
/// # Errors
///
/// A host ssh may not take, or a job id that is not one plain token.
pub fn run_job_command(host: &str, job: &str) -> Result<Vec<String>, InvocationError> {
    validate(host)?;
    if !is_plain(job) {
        return Err(InvocationError::InvalidSession);
    }
    let tail = format!(
        "exec {REMOTE_CLI} session overlay run-job {}",
        shell_quoted_line(&[job])
    );
    let mut argv = ssh_arguments(host, 5, true);
    argv.push(remote_cli_chain(&tail));
    Ok(argv)
}

/// The create-only guard after the attach: an existing daemon ignores it, a vanished one runs it and fails, so the
/// attach never hands back a fresh shell wearing the old session's name.
const GONE_GUARD: &str = "printf '%s\\n' 'agterm: remote session is gone'; exit 1";

/// One ssh invocation attaching to `daemon` on `host` for the lifetime of the pane (upstream's `attachCommand`): `-tt`,
/// as zmx reads termios and the window size, no lifetime deadline, and `LogLevel=ERROR` so ssh's own banners stay off
/// the pane. The account's own zmx variables are cleared, as an inherited `ZMX_SESSION` would switch session instead
/// of reaching the guard; `lead` opts the far zmx client into explicit leadership.
///
/// # Errors
///
/// A host ssh may not take, a name that is not an agterm daemon's, or an endpoint path with control characters.
pub fn attach_command(
    host: &str,
    endpoint: &ControlLiveEndpoint,
    daemon: &str,
    lead: Option<&LeadAttachment>,
) -> Result<Vec<String>, InvocationError> {
    validate(host)?;
    if !is_daemon_name(daemon) {
        return Err(InvocationError::InvalidSession);
    }
    if !is_path(&endpoint.executable) || !is_path(&endpoint.socket_directory) {
        return Err(InvocationError::InvalidEndpoint);
    }
    let mut words: Vec<String> = [
        "/usr/bin/env",
        "ZMX_SESSION=",
        "ZMX_SESSION_PREFIX=",
        "ZMX_NO_DETACH_KEY=1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    words.push(format!("ZMX_DIR={}", endpoint.socket_directory));
    words.extend(lead.map(LeadAttachment::assignments).unwrap_or_default());
    words.extend(
        [
            endpoint.executable.as_str(),
            "attach",
            daemon,
            "/bin/sh",
            "-c",
            GONE_GUARD,
        ]
        .into_iter()
        .map(str::to_owned),
    );
    let mut argv = ssh_arguments(host, 5, true);
    argv.push(shell_quoted_line(&words));
    let at = argv.len() - 2;
    argv.splice(at..at, ["-o".to_owned(), "LogLevel=ERROR".to_owned()]);
    Ok(argv)
}

/// What a remote pane's command needs to name it in its last line.
#[derive(Debug, Clone, Copy)]
pub struct PaneLabel<'a> {
    /// The origin's session name.
    pub session: &'a str,
    /// `left` or `right`.
    pub pane: &'a str,
}

/// The pane's command (upstream's `attachPaneCommand`): the attach, then one line naming the host, the session and
/// the exit status, the pane exiting with ssh's status. A keep-alive is added when the user's ssh config sets none.
/// With a lead nonce, ssh's own failure (255) shows a reconnecting bar, reports the loss as a [`RemoteLinkNotice`] title
/// and waits on `cat` for the app to attach the pane again; the mouse and focus reporting a remote program left on is
/// switched off first, and echo with the cursor, so nothing a paste or a wheel sends prints under the bar.
///
/// # Errors
///
/// As [`attach_command`].
pub fn attach_pane_command(
    host: &str,
    endpoint: &ControlLiveEndpoint,
    daemon: &str,
    label: PaneLabel<'_>,
    lead: Option<&LeadAttachment>,
) -> Result<String, InvocationError> {
    let argv = attach_command(host, endpoint, daemon, lead)?;
    let mut probe = vec!["ssh".to_owned(), "-G".to_owned()];
    probe.extend(argv.iter().skip(1).cloned());
    // $ka is unquoted on purpose: four words or none
    let keep_alive = format!(
        "ka=; {} 2>/dev/null | grep -qx 'serveraliveinterval 0' && ka='-o ServerAliveInterval=5 -o ServerAliveCountMax=2'",
        shell_quoted_line(&probe)
    );
    let split = argv.len() - 2;
    let attach = format!(
        "{} $ka {}",
        shell_quoted_line(&argv[..split]),
        shell_quoted_line(&argv[split..])
    );
    let line = format!(
        "agterm: {} ({}) on {host} disconnected, exit",
        label.session, label.pane
    );
    // the pane exits with ssh's status, not printf's zero, or a failed connection reads as a clean one
    let report = format!("printf '%s %s\\n' {} \"$status\"", shell_quoted_line(&[line.as_str()]));
    let script = match lead {
        None => format!("{keep_alive}; {attach}; status=$?; {report}; exit \"$status\""),
        Some(lead) => {
            let bar = shell_quoted_line(&[format!(
                "Connection to {host} lost · reconnecting… · any key retries now"
            )]);
            let title = shell_quoted_line(&[RemoteLinkNotice::title(&lead.nonce)]);
            let reset = "\\033[0m\\033[?1000l\\033[?1002l\\033[?1003l\\033[?1006l\\033[?1004l\\033[?2004l\\033[?2031l\\033[?2048l";
            format!(
                "{keep_alive}; {attach}; status=$?; if [ \"$status\" -eq 255 ]; then \
                 printf '{reset}\\r\\n\\033[30;43m %s \\033[K\\033[0m\\n' {bar}; printf '\\033]2;%s\\007' {title}; \
                 printf '\\033[?25l'; stty -echo 2>/dev/null; cat >/dev/null; else {report}; fi; exit \"$status\""
            )
        }
    };
    // `env` takes the exec a terminal runs its command under, and a plain `sh` beneath it reads no login profile
    Ok(shell_quoted_line(&["/usr/bin/env", "/bin/sh", "-c", script.as_str()]))
}

/// One ssh invocation that only proves the host answers again, before a waiting pane is attached anew (upstream's
/// `probeCommand`).
///
/// # Errors
///
/// A host ssh may not take.
pub fn probe_command(host: &str) -> Result<Vec<String>, InvocationError> {
    validate(host)?;
    let mut argv = ssh_arguments(host, 5, false);
    argv.push("true".to_owned());
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    use uuid::Uuid;

    use super::*;
    use crate::live::daemon_name;

    fn endpoint() -> ControlLiveEndpoint {
        ControlLiveEndpoint {
            executable: "/opt/agterm/bin/zmx".to_owned(),
            socket_directory: "/tmp/agterm-zmx-abc".to_owned(),
        }
    }

    fn lead(nonce: &str, claim: bool) -> LeadAttachment {
        LeadAttachment {
            nonce: nonce.to_owned(),
            claim,
        }
    }

    const LABEL: PaneLabel<'static> = PaneLabel {
        session: "build",
        pane: "left",
    };

    #[test]
    fn present_is_non_interactive_and_runs_the_bridge_for_exactly_that_session() {
        let argv = present_command("buildbox", "S1").unwrap();
        assert_eq!(
            &argv[..7],
            ["ssh", "-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "buildbox"]
        );
        let remote = argv.last().unwrap();
        assert!(remote.starts_with("'/bin/sh' '-c' "));
        assert!(remote.contains(r"exec ngtermctl live present '\''S1'\''"), "{remote}");
        for bad in ["", "s 1", "s1\u{1b}[31m"] {
            assert_eq!(present_command("buildbox", bad), Err(InvocationError::InvalidSession));
        }
        assert_eq!(
            present_command("-oProxyCommand=x", "S1"),
            Err(InvocationError::InvalidHost)
        );
    }

    #[test]
    fn run_job_forces_a_pty_and_reaches_the_installed_cli() {
        let argv = run_job_command("buildbox", "J1").unwrap();
        assert_eq!(argv[1], "-tt");
        assert!(argv.last().unwrap().contains("exec ngtermctl session overlay run-job "));
        assert!(argv.last().unwrap().contains("$HOME/.local/bin"));
        assert_eq!(run_job_command("buildbox", "j 1"), Err(InvocationError::InvalidSession));
    }

    #[test]
    fn attach_forces_a_pty_and_puts_the_lead_words_ahead_of_the_executable() {
        let daemon = daemon_name(Uuid::new_v4());
        let plain = attach_command("buildbox", &endpoint(), &daemon, None).unwrap();
        assert_eq!(plain[1], "-tt");
        assert_eq!(&plain[plain.len() - 4..plain.len() - 2], ["-o", "LogLevel=ERROR"]);
        assert_eq!(plain[plain.len() - 2], "buildbox");
        assert!(!plain.last().unwrap().contains("ZMX_MANAGED"));
        let claimed = attach_command("buildbox", &endpoint(), &daemon, Some(&lead("n1", true))).unwrap();
        let remote = claimed.last().unwrap();
        let managed = remote.find("'ZMX_MANAGED=n1'").unwrap();
        assert!(managed < remote.find("'/opt/agterm/bin/zmx'").unwrap());
        assert!(remote.contains("'ZMX_MANAGED_CLAIM=1'"));
    }

    #[test]
    fn anything_but_an_agterm_daemon_name_or_a_plain_path_is_refused() {
        for name in ["agterm 42", "agterm\n42", "", "notes", "agterm-nothex"] {
            assert_eq!(
                attach_command("buildbox", &endpoint(), name, None),
                Err(InvocationError::InvalidSession),
                "{name}"
            );
        }
        let daemon = daemon_name(Uuid::new_v4());
        let spaced = ControlLiveEndpoint {
            executable: "/home/me/My Apps/agterm/bin/zmx".to_owned(),
            socket_directory: "/tmp/x".to_owned(),
        };
        assert!(attach_command("buildbox", &spaced, &daemon, None).is_ok());
        let hostile = ControlLiveEndpoint {
            executable: "/opt/zmx\n".to_owned(),
            socket_directory: "/tmp/x".to_owned(),
        };
        assert_eq!(
            attach_command("buildbox", &hostile, &daemon, None),
            Err(InvocationError::InvalidEndpoint)
        );
    }

    #[test]
    fn the_probe_only_asks_the_host_to_answer() {
        assert_eq!(
            probe_command("mini").unwrap(),
            [
                "ssh",
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "mini",
                "true"
            ]
        );
        assert_eq!(probe_command(""), Err(InvocationError::EmptyHost));
    }

    /// A scratch directory with a fake `ssh` on its own `PATH`, which records its arguments and exits with `code`.
    struct FakeRemote {
        root: PathBuf,
    }

    impl FakeRemote {
        fn new(code: i32) -> Self {
            let root = std::env::temp_dir().join(format!("agterm-fake-ssh-{}", Uuid::new_v4().simple()));
            std::fs::create_dir_all(root.join("bin")).unwrap();
            let script = format!(
                "#!/bin/sh\nif [ \"$1\" = -G ]; then echo 'serveraliveinterval 0'; exit 0; fi\n\
                 printf '%s\\n' \"$@\" > '{}'\nexit {code}\n",
                root.join("calls").display()
            );
            let ssh = root.join("bin/ssh");
            agterm_host::fakes::write_executable(&ssh, &script).unwrap();
            Self { root }
        }

        fn path(&self) -> String {
            format!("{}:/usr/bin:/bin", self.root.join("bin").display())
        }

        /// Runs `command` as a terminal runs a pane's command line, stdin closed.
        fn run(&self, command: &str) -> (Option<i32>, String) {
            let output = Command::new("/bin/sh")
                .arg("-c")
                .arg(command)
                .env("PATH", self.path())
                .stdin(Stdio::null())
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
            )
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.root.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn marker(&self) -> PathBuf {
            self.root.join("pwned")
        }
    }

    impl Drop for FakeRemote {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "runs its fake ssh and the pane command with the POSIX shell /bin/sh, which Windows has not"
    )]
    fn the_pane_command_keeps_the_ssh_exit_status_and_says_what_died() {
        let fake = FakeRemote::new(23);
        let daemon = daemon_name(Uuid::new_v4());
        let command = attach_pane_command(
            "buildbox",
            &endpoint(),
            &daemon,
            PaneLabel {
                session: "build",
                pane: "right",
            },
            None,
        )
        .unwrap();
        let (status, stdout) = fake.run(&command);
        assert_eq!(stdout, "agterm: build (right) on buildbox disconnected, exit 23\n");
        assert_eq!(status, Some(23), "a failed connection must not read as a clean exit");
        let calls = fake.calls();
        let keep_alive = ["-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2"];
        assert!(
            calls.windows(4).any(|window| window == keep_alive),
            "a config with no keep-alive gets one: {calls:?}"
        );
        assert_eq!(calls.last().map(String::as_str), argv_last(&daemon, None).as_deref());
    }

    fn argv_last(daemon: &str, lead: Option<&LeadAttachment>) -> Option<String> {
        attach_command("buildbox", &endpoint(), daemon, lead)
            .ok()
            .and_then(|argv| argv.last().cloned())
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "runs its fake ssh and the pane command with the POSIX shell /bin/sh, which Windows has not"
    )]
    fn without_a_lead_nonce_a_lost_connection_exits_as_before() {
        let fake = FakeRemote::new(255);
        let command = attach_pane_command("buildbox", &endpoint(), &daemon_name(Uuid::new_v4()), LABEL, None).unwrap();
        assert_eq!(
            fake.run(&command),
            (
                Some(255),
                "agterm: build (left) on buildbox disconnected, exit 255\n".to_owned()
            )
        );
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "runs its fake ssh and the pane command with the POSIX shell /bin/sh, which Windows has not"
    )]
    fn a_lost_connection_with_a_lead_says_it_is_reconnecting_reports_it_and_waits() {
        let fake = FakeRemote::new(255);
        let attachment = lead("n1", true);
        let command = attach_pane_command(
            "buildbox",
            &endpoint(),
            &daemon_name(Uuid::new_v4()),
            LABEL,
            Some(&attachment),
        )
        .unwrap();
        assert!(command.contains("stty -echo 2>/dev/null; cat >/dev/null"));
        // stdin is closed, so the wait on `cat` ends at once and the bar is all that printed
        let (status, stdout) = fake.run(&command);
        assert_eq!(status, Some(255));
        assert_eq!(
            stdout,
            "\u{1b}[0m\u{1b}[?1000l\u{1b}[?1002l\u{1b}[?1003l\u{1b}[?1006l\u{1b}[?1004l\u{1b}[?2004l\u{1b}[?2031l\
             \u{1b}[?2048l\r\n\u{1b}[30;43m Connection to buildbox lost · reconnecting… · any key retries now \
             \u{1b}[K\u{1b}[0m\n\u{1b}]2;agterm-remote;n1:lost\u{7}\u{1b}[?25l"
        );
        // any other exit keeps the plain line and status
        let fake = FakeRemote::new(1);
        assert_eq!(
            fake.run(&command),
            (
                Some(1),
                "agterm: build (left) on buildbox disconnected, exit 1\n".to_owned()
            )
        );
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "runs its fake ssh and the pane command with the POSIX shell /bin/sh, which Windows has not"
    )]
    fn a_hostile_session_name_cannot_escape_the_diagnostic_line() {
        let fake = FakeRemote::new(0);
        let name = format!("build'; touch {}; echo '", fake.marker().display());
        let command = attach_pane_command(
            "buildbox",
            &endpoint(),
            &daemon_name(Uuid::new_v4()),
            PaneLabel {
                session: &name,
                pane: "left",
            },
            None,
        )
        .unwrap();
        fake.run(&command);
        assert!(!fake.marker().exists(), "the name is data, never shell syntax");
    }
}
