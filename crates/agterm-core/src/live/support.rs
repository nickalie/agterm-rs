//! Whether and how a pane is wrapped in a zmx daemon (upstream's `ZmxSupport.swift`): the host's eligibility walk,
//! shared with the launch probe ([`crate::restore::mode::LiveProbe`]) so the rejection order lives once, the wrap
//! configuration, the launch disposition and the attach command a wrapped pane's surface runs. Upstream wraps a zsh
//! login shell only; this build also wraps bash and fish ([`super::shell_integration`], capability `live-any-shell`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::lead::{CLAIM_VARIABLE, LeadAttachment, NONCE_VARIABLE};
use super::replay::FinalShell;
use super::shell_integration::{self, Inherited, LoginShell};
use super::{daemon_name, replay, socket_directory};
use crate::command_restore::{shell_quoted_line, should_restore};
use crate::pane_env::uuid_string;
use crate::persist::RestoreMode;
use crate::restore::mode::LiveRejection;

/// What the host offers a wrap: the zmx executable, the login shell and its bundled integration, all checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eligible {
    /// The zmx executable, standardized.
    pub executable: PathBuf,
    /// The password-database login shell.
    pub shell: String,
    /// Which shell it is.
    pub kind: LoginShell,
    /// The Ghostty resources directory bundling the shell's integration, standardized.
    pub resources: PathBuf,
}

/// The first reason this host cannot wrap a pane, in upstream's order, or what a wrap runs with: a relative zmx path,
/// a zmx that is not an executable file, a login shell other than zsh, bash or fish (`$SHELL` cannot override the
/// password database), a resources directory without that shell's integration.
///
/// # Errors
///
/// The first [`LiveRejection`] that applies.
pub fn eligibility(
    executable: &Path,
    login_shell: Option<&str>,
    resources_dir: Option<&Path>,
) -> Result<Eligible, LiveRejection> {
    if !executable.is_absolute() {
        return Err(LiveRejection::ExecutablePathNotAbsolute);
    }
    let executable = super::standardized_path(executable);
    if !is_executable_file(&executable) {
        return Err(LiveRejection::ExecutableUnavailable);
    }
    let (shell, kind) = login_shell
        .and_then(|shell| LoginShell::of(shell).map(|kind| (shell, kind)))
        .ok_or(LiveRejection::UnsupportedLoginShell)?;
    let resources = resources_dir.ok_or(LiveRejection::MissingShellIntegration(kind))?;
    let resources = super::standardized_path(resources);
    if !kind.is_bundled(&resources) {
        return Err(LiveRejection::MissingShellIntegration(kind));
    }
    Ok(Eligible {
        executable,
        shell: shell.to_owned(),
        kind,
        resources,
    })
}

/// Whether `path` is a regular file this process may execute: on Unix one with an execute bit, on Windows one whose
/// extension `CreateProcessW` starts directly ([`WINDOWS_EXECUTABLE_EXTENSIONS`], in any case), since a Windows file
/// has no execute bit.
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(windows)]
    {
        metadata.is_file() && has_windows_executable_extension(path)
    }
}

/// The extensions of the files Windows' `CreateProcessW` runs as programs (a `.cmd` or `.bat` needs `cmd.exe`).
pub const WINDOWS_EXECUTABLE_EXTENSIONS: [&str; 2] = ["exe", "com"];

/// Whether `path` ends in one of [`WINDOWS_EXECUTABLE_EXTENSIONS`], in any case. Defined on every OS, so the rule
/// is tested everywhere.
#[must_use]
pub fn has_windows_executable_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            WINDOWS_EXECUTABLE_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Everything a pane's wrap configuration is built from (upstream's `ZmxSupport.Inputs`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inputs {
    pub executable: PathBuf,
    /// The login shell the password database names.
    pub login_shell: Option<String>,
    /// The Ghostty resources directory holding `shell-integration/<shell>`.
    pub resources_dir: Option<PathBuf>,
    pub state_dir: PathBuf,
    pub pane_identity: Uuid,
    /// The pane's own environment, which the wrap extends.
    pub base_environment: BTreeMap<String, String>,
    /// The app's own values of the variables the shell integration hands on or extends.
    pub inherited: Inherited,
    /// A supervising session host (macOS's `agterm-session-host`); Linux has none.
    pub session_host_executable: Option<PathBuf>,
    /// `None` leaves the attach unmanaged, as a zmx without explicit leadership expects.
    pub lead: Option<LeadAttachment>,
}

/// How a wrapped pane attaches to its daemon (upstream's `ZmxSupport.Configuration`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configuration {
    pub executable: String,
    pub session_host_executable: Option<String>,
    /// The surface's environment.
    pub environment: BTreeMap<String, String>,
    pub daemon_name: String,
    pub socket_directory: String,
    /// The pane identity as `AGTERM_PANE_ID` spells it (upper case).
    pub pane_id: String,
}

impl Configuration {
    /// `zmx attach <daemon>`.
    #[must_use]
    pub fn attach_arguments(&self) -> Vec<String> {
        vec![self.executable.clone(), "attach".to_owned(), self.daemon_name.clone()]
    }

    /// The bare attach as one shell line.
    #[must_use]
    pub fn command(&self) -> String {
        shell_quoted_line(&self.attach_arguments())
    }
}

/// The wrap configuration of `inputs`' pane.
///
/// # Errors
///
/// The host cannot wrap a pane ([`eligibility`]).
pub fn configuration(inputs: &Inputs) -> Result<Configuration, LiveRejection> {
    let eligible = eligibility(
        &inputs.executable,
        inputs.login_shell.as_deref(),
        inputs.resources_dir.as_deref(),
    )?;
    let socket_directory = socket_directory(&inputs.state_dir);
    let pane_id = uuid_string(inputs.pane_identity);
    let mut environment = inputs.base_environment.clone();
    let mut set = |key: &str, value: &str| environment.insert(key.to_owned(), value.to_owned());
    set("AGTERM_PANE_ID", &pane_id);
    set("SHELL", &eligible.shell);
    set("ZMX_DIR", &socket_directory);
    set("ZMX_SESSION", "");
    set("ZMX_SESSION_PREFIX", "");
    set("ZMX_NO_DETACH_KEY", "1");
    shell_integration::inject(eligible.kind, &eligible.resources, &inputs.inherited, &mut environment);
    // a stale value inherited from the app's own environment would opt the client in under a nonce no pane expects
    environment.remove(NONCE_VARIABLE);
    environment.remove(CLAIM_VARIABLE);
    if let Some(lead) = &inputs.lead {
        environment.extend(lead.environment());
    }
    let session_host_executable = inputs
        .session_host_executable
        .as_deref()
        .filter(|path| path.is_absolute() && is_executable_file(path))
        .map(|path| super::standardized_path(path).to_string_lossy().into_owned());
    Ok(Configuration {
        executable: eligible.executable.to_string_lossy().into_owned(),
        session_host_executable,
        environment,
        daemon_name: daemon_name(inputs.pane_identity),
        socket_directory,
        pane_id,
    })
}

/// How a pane starts (upstream's `ZmxSupport.LaunchDisposition`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchDisposition {
    /// Not a live launch: a plain surface.
    Ordinary,
    /// Wrapped in its daemon.
    Wrapped(Configuration),
    /// Live was requested but is unavailable. Restored panes start plain shells without consuming rerun state; fresh
    /// primary panes still honor their creation command.
    Fallback,
}

impl LaunchDisposition {
    /// Whether the pane runs inside a daemon.
    #[must_use]
    pub fn backed_by_live(&self) -> bool {
        matches!(self, Self::Wrapped(_))
    }
}

/// The disposition of a pane in a launch that requested `requested` and runs `active`, with the pane's configuration
/// when one could be built.
#[must_use]
pub fn launch_disposition(
    requested: RestoreMode,
    active: RestoreMode,
    configuration: Option<Configuration>,
) -> LaunchDisposition {
    if requested != RestoreMode::Live {
        return LaunchDisposition::Ordinary;
    }
    match configuration {
        Some(configuration) if active == RestoreMode::Live => LaunchDisposition::Wrapped(configuration),
        _ => LaunchDisposition::Fallback,
    }
}

/// The shell line that waits for the terminal's first resize before it runs: up to 20 polls 10 ms apart, until
/// `stty size` reads something new.
const SETTLE_PREFIX: &str = "s=$(stty size 2>/dev/null); i=0; \
    while [ \"$i\" -lt 20 ] && [ \"$(stty size 2>/dev/null)\" = \"$s\" ]; do sleep 0.01; i=$((i + 1)); done; ";

/// `line` (an attach, run by the terminal through `/bin/sh -c`) started once the terminal has its size. A Linux
/// adaptation made for the GTK build: libghostty spawned a surface's program at its 800×600 px initial size and
/// applied the widget's size a moment later. A GTK pane was often narrower than that, so a daemon the attach created
/// would start its shell at the initial width and reflow the first prompt row (zsh's `PROMPT_SP` spaces fill it) into
/// soft-wrapped rows on the shrink, which every `zmx screen` read then joins to the next line. Upstream's macOS views
/// only ever grow from that size, which reflows nothing. An attach to a daemon that exists waits the same, harmlessly.
#[must_use]
pub fn settled(line: &str) -> String {
    format!("{SETTLE_PREFIX}exec {line}")
}

/// The command a wrapped pane's surface runs: the attach, with the creation payload a new daemon runs (a replayable
/// captured `argv` first, else the durable `creation_command`), behind the session host's client when there is one.
/// A refused replay (denylisted, unreplayable) keeps the bare attach rather than falling back to the creation command.
/// Without a payload a bash daemon still starts its login shell in POSIX mode, the one way it reads its integration.
#[must_use]
pub fn attach_command<S: AsRef<str>>(
    configuration: &Configuration,
    replaying: Option<&[String]>,
    creation_command: Option<&str>,
    denylist: &[S],
) -> String {
    let mut arguments = configuration.attach_arguments();
    arguments.extend(creation_arguments(configuration, replaying, creation_command, denylist));
    if let Some(host) = &configuration.session_host_executable {
        let mut wrapped = vec![
            host.clone(),
            "client".to_owned(),
            configuration.daemon_name.clone(),
            "--".to_owned(),
        ];
        wrapped.append(&mut arguments);
        arguments = wrapped;
    }
    shell_quoted_line(&arguments)
}

fn creation_arguments<S: AsRef<str>>(
    configuration: &Configuration,
    replaying: Option<&[String]>,
    creation_command: Option<&str>,
    denylist: &[S],
) -> Vec<String> {
    let environment = &configuration.environment;
    let Some((shell, kind)) = environment
        .get("SHELL")
        .and_then(|shell| LoginShell::of(shell).map(|kind| (shell, kind)))
    else {
        return Vec::new();
    };
    let login = kind.login_arguments(shell);
    let Some(rearm) = shell_integration::rearm(kind, environment) else {
        return login;
    };
    let final_shell = FinalShell {
        path: shell,
        kind,
        rearm: &rearm,
    };
    let script = match (replaying, creation_command) {
        (Some(argv), _) if !should_restore(argv, denylist) => return login,
        (Some(argv), _) => replay::render_argv(argv, &final_shell),
        (None, Some(line)) => replay::render_command_line(line, &final_shell),
        (None, None) => return login,
    };
    let mut arguments = vec![shell.clone()];
    arguments.extend(kind.script_flags().iter().map(|flag| (*flag).to_owned()));
    arguments.push(script);
    arguments
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::shell_integration::Rearm;
    use super::*;

    const NO_DENYLIST: [&str; 0] = [];

    fn resources(with_loader: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let integration = dir.path().join("shell-integration/zsh");
        fs::create_dir_all(&integration).unwrap();
        if with_loader {
            fs::write(integration.join(".zshenv"), "").unwrap();
        }
        dir
    }

    fn inputs(resources: &Path) -> Inputs {
        Inputs {
            executable: super::super::test_executable(resources, "echo"),
            login_shell: Some("/bin/zsh".to_owned()),
            resources_dir: Some(resources.to_path_buf()),
            state_dir: PathBuf::from("/tmp/agterm-state"),
            pane_identity: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
            ..Inputs::default()
        }
    }

    /// The replay script `replay_configuration`'s zsh renders `argv` with.
    fn zsh_script(argv: &[String]) -> String {
        let rearm = [
            Rearm::Export("ZDOTDIR", "/bundle resources/zsh".to_owned()),
            Rearm::Export("GHOSTTY_ZSH_ZDOTDIR", "/user/zsh".to_owned()),
        ];
        let shell = FinalShell {
            path: "/bin/zsh",
            kind: LoginShell::Zsh,
            rearm: &rearm,
        };
        replay::render_argv(argv, &shell)
    }

    fn replay_configuration(host: Option<&str>) -> Configuration {
        Configuration {
            executable: "/bin/zmx".to_owned(),
            session_host_executable: host.map(str::to_owned),
            environment: BTreeMap::from([
                ("SHELL".to_owned(), "/bin/zsh".to_owned()),
                ("ZDOTDIR".to_owned(), "/bundle resources/zsh".to_owned()),
                ("GHOSTTY_ZSH_ZDOTDIR".to_owned(), "/user/zsh".to_owned()),
            ]),
            daemon_name: "agterm-pane".to_owned(),
            socket_directory: "/tmp/zmx".to_owned(),
            pane_id: "pane".to_owned(),
        }
    }

    #[test]
    fn the_configuration_uses_the_password_database_zsh_and_the_full_pane_identity() {
        let resources = resources(true);
        let mut inputs = inputs(resources.path());
        inputs.pane_identity = Uuid::parse_str("ABCDEF01-2345-6789-ABCD-EF0123456789").unwrap();
        inputs.base_environment = BTreeMap::from([
            ("AGTERM_ENABLED".to_owned(), "1".to_owned()),
            ("AGTERM_PANE_ID".to_owned(), "old".to_owned()),
            ("ZMX_SESSION".to_owned(), "parent-session".to_owned()),
            ("ZMX_SESSION_PREFIX".to_owned(), "parent.".to_owned()),
        ]);
        inputs.inherited.zdotdir = Some("/home/test/.config/zsh".to_owned());
        let configuration = configuration(&inputs).unwrap();
        assert_eq!(configuration.daemon_name, "agterm-abcdef0123456789abcdef0123456789");
        assert_eq!(configuration.pane_id, "ABCDEF01-2345-6789-ABCD-EF0123456789");
        assert_eq!(
            configuration.command(),
            shell_quoted_line(&[
                inputs.executable.to_str().unwrap(),
                "attach",
                "agterm-abcdef0123456789abcdef0123456789"
            ])
        );
        assert_eq!(
            configuration.socket_directory,
            socket_directory(Path::new("/tmp/agterm-state"))
        );
        let env = |key: &str| configuration.environment.get(key).map(String::as_str);
        assert_eq!(env("AGTERM_ENABLED"), Some("1"));
        assert_eq!(env("AGTERM_PANE_ID"), Some("ABCDEF01-2345-6789-ABCD-EF0123456789"));
        assert_eq!(env("SHELL"), Some("/bin/zsh"));
        let integration = resources.path().join("shell-integration/zsh");
        assert_eq!(env("ZDOTDIR").map(Path::new), Some(integration.as_path()));
        assert_eq!(env("GHOSTTY_ZSH_ZDOTDIR"), Some("/home/test/.config/zsh"));
        assert_eq!(env("ZMX_DIR"), Some(configuration.socket_directory.as_str()));
        assert_eq!(env("ZMX_NO_DETACH_KEY"), Some("1"));
        assert_eq!((env("ZMX_SESSION"), env("ZMX_SESSION_PREFIX")), (Some(""), Some("")));
        assert_eq!(configuration.session_host_executable, None);
    }

    #[test]
    fn a_lead_attachment_reaches_the_attach_environment_and_an_inherited_one_does_not() {
        let resources = resources(true);
        let environment = |lead: Option<LeadAttachment>| {
            let mut inputs = inputs(resources.path());
            inputs.base_environment = BTreeMap::from([
                ("ZMX_MANAGED".to_owned(), "stale".to_owned()),
                ("ZMX_MANAGED_CLAIM".to_owned(), "1".to_owned()),
            ]);
            inputs.lead = lead;
            configuration(&inputs).unwrap().environment
        };
        let unmanaged = environment(None);
        assert!(!unmanaged.contains_key("ZMX_MANAGED") && !unmanaged.contains_key("ZMX_MANAGED_CLAIM"));
        let recovering = environment(Some(LeadAttachment {
            nonce: "n1".to_owned(),
            claim: false,
        }));
        assert_eq!(recovering.get("ZMX_MANAGED").map(String::as_str), Some("n1"));
        assert!(!recovering.contains_key("ZMX_MANAGED_CLAIM"));
        let claiming = environment(Some(LeadAttachment {
            nonce: "n2".to_owned(),
            claim: true,
        }));
        assert_eq!(claiming.get("ZMX_MANAGED").map(String::as_str), Some("n2"));
        assert_eq!(claiming.get("ZMX_MANAGED_CLAIM").map(String::as_str), Some("1"));
    }

    #[test]
    fn a_windows_executable_is_known_by_its_extension() {
        for path in [r"C:\agterm\agterm-session.exe", "x.EXE", "tool.Com"] {
            assert!(has_windows_executable_extension(Path::new(path)), "{path}");
        }
        for path in [
            r"C:\agterm\agterm-session",
            "run.cmd",
            "run.bat",
            "x.exe.txt",
            ".exe",
            "exe",
        ] {
            assert!(!has_windows_executable_extension(Path::new(path)), "{path}");
        }
    }

    /// The `cfg` pair of [`is_executable_file`]: what makes a regular file executable is its mode on Unix and its
    /// extension on Windows; a directory or a missing path never is.
    #[test]
    fn an_executable_file_is_one_by_the_os_rule() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("tool");
        let exe = dir.path().join("tool.exe");
        fs::write(&plain, "").unwrap();
        fs::write(&exe, "").unwrap();
        assert!(!is_executable_file(dir.path()));
        assert!(!is_executable_file(&dir.path().join("missing.exe")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert!(!is_executable_file(&plain));
            assert!(!is_executable_file(&exe), "an extension is no execute bit");
            fs::set_permissions(&plain, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(is_executable_file(&plain));
        }
        #[cfg(windows)]
        {
            assert!(!is_executable_file(&plain));
            assert!(is_executable_file(&exe));
        }
    }

    #[test]
    fn wrap_rejections_come_in_upstreams_order() {
        let with_loader = resources(true);
        let without_loader = resources(false);
        let mut relative = inputs(with_loader.path());
        relative.executable = PathBuf::from("zmx");
        assert_eq!(configuration(&relative), Err(LiveRejection::ExecutablePathNotAbsolute));
        let mut missing = inputs(with_loader.path());
        missing.executable = with_loader.path().join("no-such-zmx");
        assert_eq!(configuration(&missing), Err(LiveRejection::ExecutableUnavailable));
        let mut not_executable = inputs(with_loader.path());
        not_executable.executable = with_loader.path().join("shell-integration/zsh/.zshenv");
        assert_eq!(
            configuration(&not_executable),
            Err(LiveRejection::ExecutableUnavailable)
        );
        let mut tcsh = inputs(with_loader.path());
        tcsh.login_shell = Some("/bin/tcsh".to_owned());
        // the pane's own `$SHELL` never overrides the password database
        tcsh.base_environment = BTreeMap::from([("SHELL".to_owned(), "/bin/zsh".to_owned())]);
        assert_eq!(configuration(&tcsh), Err(LiveRejection::UnsupportedLoginShell));
        // bash is supported, and its own integration is the one checked for
        let mut bash = inputs(with_loader.path());
        bash.login_shell = Some("/bin/bash".to_owned());
        assert_eq!(
            configuration(&bash),
            Err(LiveRejection::MissingShellIntegration(LoginShell::Bash))
        );
        let mut no_shell = inputs(with_loader.path());
        no_shell.login_shell = None;
        assert_eq!(configuration(&no_shell), Err(LiveRejection::UnsupportedLoginShell));
        assert_eq!(
            configuration(&inputs(without_loader.path())),
            Err(LiveRejection::MissingShellIntegration(LoginShell::Zsh))
        );
        let mut no_resources = inputs(with_loader.path());
        no_resources.resources_dir = None;
        assert_eq!(
            configuration(&no_resources),
            Err(LiveRejection::MissingShellIntegration(LoginShell::Zsh))
        );
        assert!(configuration(&inputs(with_loader.path())).is_ok());
    }

    #[test]
    fn rejection_messages_are_the_user_facing_settings_reasons() {
        for (rejection, message) in [
            (
                LiveRejection::ExecutablePathNotAbsolute,
                "the live-session executable path is not absolute",
            ),
            (
                LiveRejection::ExecutableUnavailable,
                "the live-session executable is unavailable",
            ),
            (
                LiveRejection::UnsupportedLoginShell,
                "the password-database login shell is not zsh, bash or fish",
            ),
            (
                LiveRejection::MissingShellIntegration(LoginShell::Zsh),
                "the bundled zsh integration is unavailable",
            ),
            (
                LiveRejection::MissingShellIntegration(LoginShell::Bash),
                "the bundled bash integration is unavailable",
            ),
            (
                LiveRejection::MissingShellIntegration(LoginShell::Fish),
                "the bundled fish integration is unavailable",
            ),
        ] {
            assert_eq!(rejection.message(), message);
        }
    }

    #[test]
    fn an_unavailable_session_host_keeps_the_bare_attach() {
        let resources = resources(true);
        for host in [None, Some("relative/helper"), Some("/no/such/session-host")] {
            let mut inputs = inputs(resources.path());
            inputs.session_host_executable = host.map(PathBuf::from);
            let configuration = configuration(&inputs).unwrap();
            assert_eq!(configuration.session_host_executable, None);
            assert_eq!(
                attach_command(&configuration, None, None, &NO_DENYLIST),
                configuration.command()
            );
        }
        let mut inputs = inputs(resources.path());
        let host = super::super::test_executable(resources.path(), "cat");
        inputs.session_host_executable = Some(host.clone());
        let configuration = configuration(&inputs).unwrap();
        assert_eq!(
            attach_command(&configuration, None, None, &NO_DENYLIST),
            shell_quoted_line(&[
                host.to_str().unwrap(),
                "client",
                &configuration.daemon_name,
                "--",
                inputs.executable.to_str().unwrap(),
                "attach",
                &configuration.daemon_name
            ])
        );
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "runs its script with the POSIX shell /bin/sh, which Windows has not"
    )]
    fn a_settled_line_runs_after_the_wait_in_place_of_the_shell() {
        let line = settled("echo \"$0\" settled");
        assert!(line.ends_with("; exec echo \"$0\" settled"), "{line}");
        // without a terminal `stty` reads nothing new, so the wait runs out and the line still runs
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &line, "attached"])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "attached settled\n");
    }

    #[test]
    fn the_launch_disposition_keeps_live_fallback_state_unconsumed() {
        let configuration = replay_configuration(None);
        assert_eq!(
            launch_disposition(RestoreMode::Rerun, RestoreMode::Rerun, Some(configuration.clone())),
            LaunchDisposition::Ordinary
        );
        let wrapped = launch_disposition(RestoreMode::Live, RestoreMode::Live, Some(configuration.clone()));
        assert!(wrapped.backed_by_live());
        assert_eq!(wrapped, LaunchDisposition::Wrapped(configuration.clone()));
        assert_eq!(
            launch_disposition(RestoreMode::Live, RestoreMode::None, None),
            LaunchDisposition::Fallback
        );
        assert_eq!(
            launch_disposition(RestoreMode::Live, RestoreMode::None, Some(configuration)),
            LaunchDisposition::Fallback
        );
    }

    #[test]
    fn an_eligible_replay_wins_over_the_durable_command_as_one_outer_quoted_payload() {
        let configuration = replay_configuration(None);
        let argv: Vec<String> = ["printf", "%s", "two words", "single'quote"]
            .map(str::to_owned)
            .to_vec();
        let script = zsh_script(&argv);
        assert_eq!(
            attach_command(&configuration, Some(&argv), Some("echo durable"), &NO_DENYLIST),
            format!(
                "{} {}",
                configuration.command(),
                shell_quoted_line(&["/bin/zsh", "-lic", &script])
            )
        );
    }

    #[test]
    fn the_session_host_wraps_the_replay_without_changing_the_script() {
        let configuration = replay_configuration(Some("/bundle's files/agterm-session-host"));
        let argv: Vec<String> = ["printf", "%s", "two words", "single'quote", "$literal"]
            .map(str::to_owned)
            .to_vec();
        let script = zsh_script(&argv);
        assert_eq!(
            attach_command(&configuration, Some(&argv), Some("do not run"), &NO_DENYLIST),
            shell_quoted_line(&[
                "/bundle's files/agterm-session-host",
                "client",
                "agterm-pane",
                "--",
                "/bin/zmx",
                "attach",
                "agterm-pane",
                "/bin/zsh",
                "-lic",
                &script
            ])
        );
    }

    #[test]
    fn a_refused_replay_keeps_the_bare_attach() {
        let configuration = replay_configuration(None);
        for (argv, denylist) in [
            (vec!["tmux"], vec!["tmux"]),
            (vec!["echo", "a\nb"], vec![]),
            (vec!["grep", "caf\u{FFFD}"], vec![]),
            (vec![""], vec![]),
        ] {
            let argv: Vec<String> = argv.into_iter().map(str::to_owned).collect();
            assert_eq!(
                attach_command(&configuration, Some(&argv), Some("echo durable"), &denylist),
                configuration.command()
            );
        }
    }

    #[test]
    fn a_missing_replay_uses_the_durable_command_as_a_shell_line() {
        let configuration = replay_configuration(None);
        let line = "printf durable && echo 'two words'";
        let script = replay::render_command_line(
            line,
            &FinalShell {
                path: "/bin/zsh",
                kind: LoginShell::Zsh,
                rearm: &[
                    Rearm::Export("ZDOTDIR", "/bundle resources/zsh".to_owned()),
                    Rearm::Export("GHOSTTY_ZSH_ZDOTDIR", "/user/zsh".to_owned()),
                ],
            },
        );
        assert_eq!(
            attach_command(&configuration, None, Some(line), &NO_DENYLIST),
            format!(
                "{} {}",
                configuration.command(),
                shell_quoted_line(&["/bin/zsh", "-lic", &script])
            )
        );
        assert_eq!(
            attach_command(&configuration, None, None, &NO_DENYLIST),
            configuration.command()
        );
    }

    /// A resources directory bundling the integration file `loader` (under `shell-integration`) with `contents`.
    fn bundled(loader: &str, contents: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell-integration").join(loader);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        dir
    }

    fn shell_inputs(resources: &Path, shell: &str) -> Inputs {
        let mut inputs = inputs(resources);
        inputs.login_shell = Some(shell.to_owned());
        inputs.inherited = Inherited {
            home: Some("/home/u".to_owned()),
            xdg_data_dirs: Some("/usr/share".to_owned()),
            ..Inherited::default()
        };
        inputs
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the zsh, bash and fish integrations are Unix shells' and spelled with /; a Windows live pane runs PowerShell"
    )]
    fn a_bash_wrap_injects_through_env_and_starts_bash_in_posix_mode() {
        let resources = bundled("bash/ghostty.bash", "");
        let configuration = configuration(&shell_inputs(resources.path(), "/bin/bash")).unwrap();
        let env = |key: &str| configuration.environment.get(key).map(String::as_str);
        let script = resources.path().join("shell-integration/bash/ghostty.bash");
        assert_eq!(env("SHELL"), Some("/bin/bash"));
        assert_eq!(env("ENV"), script.to_str());
        assert_eq!(env("GHOSTTY_BASH_INJECT"), Some("1"));
        assert_eq!(env("HISTFILE"), Some("/home/u/.bash_history"));
        assert_eq!(env("GHOSTTY_BASH_UNEXPORT_HISTFILE"), Some("1"));
        assert_eq!(env("ZDOTDIR"), None);
        assert_eq!(env("ZMX_DIR"), Some(configuration.socket_directory.as_str()));
        // nothing to run: the attach still names bash in POSIX mode, the one way it reads ENV
        let posix_login = format!(
            "{} {}",
            configuration.command(),
            shell_quoted_line(&["/bin/bash", "--posix", "-l"])
        );
        assert_eq!(attach_command(&configuration, None, None, &NO_DENYLIST), posix_login);
        let denied: Vec<String> = vec!["tmux".to_owned()];
        assert_eq!(
            attach_command(&configuration, Some(&denied), Some("echo durable"), &["tmux"]),
            posix_login
        );
        let argv: Vec<String> = vec!["top".to_owned()];
        let rearm = shell_integration::rearm(LoginShell::Bash, &configuration.environment).unwrap();
        let shell = FinalShell {
            path: "/bin/bash",
            kind: LoginShell::Bash,
            rearm: &rearm,
        };
        assert_eq!(
            attach_command(&configuration, Some(&argv), None, &NO_DENYLIST),
            format!(
                "{} {}",
                configuration.command(),
                shell_quoted_line(&["/bin/bash", "--posix", "-lic", &replay::render_argv(&argv, &shell)])
            )
        );
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the zsh, bash and fish integrations are Unix shells' and spelled with /; a Windows live pane runs PowerShell"
    )]
    fn a_fish_wrap_puts_the_integration_on_the_data_dirs_and_attaches_bare() {
        let resources = bundled("fish/vendor_conf.d/ghostty-shell-integration.fish", "");
        let configuration = configuration(&shell_inputs(resources.path(), "/usr/bin/fish")).unwrap();
        let env = |key: &str| configuration.environment.get(key).map(String::as_str);
        let integration = resources.path().join("shell-integration").display().to_string();
        assert_eq!(env("SHELL"), Some("/usr/bin/fish"));
        assert_eq!(env("GHOSTTY_SHELL_INTEGRATION_XDG_DIR"), Some(integration.as_str()));
        assert_eq!(env("XDG_DATA_DIRS"), Some(format!("{integration}:/usr/share").as_str()));
        assert_eq!((env("ENV"), env("ZDOTDIR")), (None, None));
        // zmx starts fish as a login shell itself
        assert_eq!(
            attach_command(&configuration, None, None, &NO_DENYLIST),
            configuration.command()
        );
        let line = "cd /tmp; and ls";
        let rearm = shell_integration::rearm(LoginShell::Fish, &configuration.environment).unwrap();
        let shell = FinalShell {
            path: "/usr/bin/fish",
            kind: LoginShell::Fish,
            rearm: &rearm,
        };
        assert_eq!(
            attach_command(&configuration, None, Some(line), &NO_DENYLIST),
            format!(
                "{} {}",
                configuration.command(),
                shell_quoted_line(&["/usr/bin/fish", "-lic", &replay::render_command_line(line, &shell)])
            )
        );
    }

    /// The bash payload against a real bash: the POSIX-mode shell that runs the command reads `ENV` (the integration,
    /// here a stand-in that logs and consumes its variables as Ghostty's does), and the final shell the script execs
    /// reads the re-armed one again. `HOME` is a throwaway directory, so no user startup file is read.
    #[test]
    #[cfg_attr(
        windows,
        ignore = "the zsh, bash and fish integrations are Unix shells' and spelled with /; a Windows live pane runs PowerShell"
    )]
    #[cfg_attr(
        target_os = "macos",
        ignore = "macOS' /bin/bash is bash 3.2, which never runs the integration from ENV; Ghostty injects none into it"
    )]
    fn a_bash_payload_runs_the_command_and_rearms_the_integration_for_the_final_shell() {
        let home = tempfile::tempdir().unwrap();
        let log = home.path().join("log");
        let quoted_log = shell_quoted_line(&[log.display().to_string()]);
        let resources = bundled(
            "bash/ghostty.bash",
            &format!(
                "builtin printf 'integration %s %s %s\\n' \"$GHOSTTY_BASH_INJECT\" \"$HISTFILE\" \"$-\" >> {quoted_log}\n\
                 builtin unset ENV GHOSTTY_BASH_INJECT GHOSTTY_BASH_UNEXPORT_HISTFILE\nbuiltin set +o posix\n"
            ),
        );
        let mut inputs = shell_inputs(resources.path(), "/bin/bash");
        inputs.inherited.home = Some(home.path().display().to_string());
        let configuration = configuration(&inputs).unwrap();
        let argv: Vec<String> = ["/bin/sh", "-c", &format!("echo command >> {quoted_log}")]
            .map(str::to_owned)
            .to_vec();
        let payload = creation_arguments(&configuration, Some(&argv), None, &NO_DENYLIST);
        let output = std::process::Command::new(&payload[0])
            .args(&payload[1..])
            .env_clear()
            .envs(&configuration.environment)
            .env("HOME", home.path())
            .env("PATH", "/usr/bin:/bin")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let history = home.path().join(".bash_history").display().to_string();
        let lines: Vec<String> = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| line.replacen(&history, "HISTORY", 1))
            .collect();
        assert_eq!(lines.len(), 3, "{lines:?}");
        // both the shell that ran the command and the final one are interactive: `$-` (the last field) holds `i`
        let interactive = |line: &str| {
            line.strip_prefix("integration 1 HISTORY ")
                .is_some_and(|flags| !flags.contains(' ') && flags.contains('i'))
        };
        assert!(interactive(&lines[0]), "{lines:?}");
        assert_eq!(lines[1], "command");
        assert!(interactive(&lines[2]), "{lines:?}");
    }
}
