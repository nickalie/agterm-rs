//! The restore mode this process launched with (upstream's `RestoreMode.launchDecision` in `RestoreMode.swift` and
//! `GhosttyApp.restoreLaunchDecision`), what `restore.mode` reports of it (`ControlRestoreStatus`), and why `live` is
//! unavailable on this host (`ZmxLaunch.liveUnavailableReason` over `ZmxSupport.configuration`'s rejections).
//!
//! The decision is taken once, before the first terminal, and never changes for the process: a pane is wrapped in a
//! live-session daemon or not when it is created, so a mode set later applies to the next launch only.

use std::path::PathBuf;

use agterm_proto::ControlRestoreStatus;

use crate::live::shell_integration::LoginShell;
use crate::live::support::eligibility;
use crate::persist::RestoreMode;

/// The mode the settings asked this launch for, the one it runs in, and why `live` fell back when it did (upstream's
/// `RestoreLaunchDecision`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RestoreLaunchDecision {
    /// What `settings.json` held at launch.
    pub requested: RestoreMode,
    /// What this launch runs in: `none` when `live` was requested and is unavailable.
    pub active: RestoreMode,
    /// Why `live` cannot run on this host, probed whatever the requested mode.
    pub live_unavailable_reason: Option<String>,
}

impl RestoreLaunchDecision {
    /// The decision for a launch requesting `requested` on a host where `live` is unavailable for
    /// `live_unavailable_reason` (`None` when it is available).
    #[must_use]
    pub fn new(requested: RestoreMode, live_unavailable_reason: Option<String>) -> Self {
        let active = if requested == RestoreMode::Live && live_unavailable_reason.is_some() {
            RestoreMode::None
        } else {
            requested
        };
        Self {
            requested,
            active,
            live_unavailable_reason,
        }
    }

    /// Whether this launch replays captured commands and pins (`rerun`).
    #[must_use]
    pub fn replays(&self) -> bool {
        self.active == RestoreMode::Rerun
    }

    /// Whether `live` was requested and fell back (upstream's `.fallback` launch disposition): such a launch reads
    /// neither pending slot, so both stay armed for the next one.
    #[must_use]
    pub fn live_fell_back(&self) -> bool {
        self.requested == RestoreMode::Live && self.active != RestoreMode::Live
    }

    /// The `restore.mode` read-back against the mode the settings hold now (upstream's `restoreStatus`). The reason is
    /// reported only when `live` was requested and refused: a probed reason under `none` or `rerun` would tell the user
    /// their host lacks something for a mode they never asked for.
    #[must_use]
    pub fn status(&self, configured: RestoreMode) -> ControlRestoreStatus {
        ControlRestoreStatus {
            configured: configured.wire_name().to_owned(),
            requested_at_launch: self.requested.wire_name().to_owned(),
            active: self.active.wire_name().to_owned(),
            restart_required: configured != self.requested,
            unavailable_reason: self
                .live_fell_back()
                .then(|| self.live_unavailable_reason.clone())
                .flatten(),
        }
    }
}

/// Whether the quit and last-window-close captures run for the configured next-launch mode (upstream's
/// `GhosttyApp.capturesForegroundOnExit`): `rerun` replays them, `live` falls back to them.
#[must_use]
pub fn captures_on_exit(configured: RestoreMode) -> bool {
    matches!(configured, RestoreMode::Rerun | RestoreMode::Live)
}

/// [`LiveRejection::UnsupportedLoginShell`]'s reason on this OS.
pub const UNSUPPORTED_LOGIN_SHELL: &str = if cfg!(windows) {
    "neither pwsh.exe nor powershell.exe is available"
} else {
    "the password-database login shell is not zsh, bash, fish or PowerShell"
};

/// Why this host cannot wrap a pane in a live-session daemon (upstream's `ZmxSupport.Rejection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveRejection {
    /// A default UI-test launch, which opts out of live sessions unless asked in.
    UiTestLaunch,
    ExecutablePathNotAbsolute,
    ExecutableUnavailable,
    UnsupportedLoginShell,
    /// The resources lack the integration of the login shell, which is supported.
    MissingShellIntegration(LoginShell),
}

impl LiveRejection {
    /// The reason `restore.mode` reports, upstream's wording; the shell names widen with the bash, fish and PowerShell
    /// extension. Windows has no password database: its shell is the PowerShell found on the host
    /// (`agterm-session::launch`), so there the unsupported shell is a missing PowerShell.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::UiTestLaunch => "Live sessions are disabled for default UI tests.",
            Self::ExecutablePathNotAbsolute => "the live-session executable path is not absolute",
            Self::ExecutableUnavailable => "the live-session executable is unavailable",
            Self::UnsupportedLoginShell => UNSUPPORTED_LOGIN_SHELL,
            Self::MissingShellIntegration(LoginShell::Zsh) => "the bundled zsh integration is unavailable",
            Self::MissingShellIntegration(LoginShell::Bash) => "the bundled bash integration is unavailable",
            Self::MissingShellIntegration(LoginShell::Fish) => "the bundled fish integration is unavailable",
            Self::MissingShellIntegration(LoginShell::Pwsh) => "the bundled PowerShell integration is unavailable",
        }
    }
}

/// What the live-session eligibility probe reads of the host (upstream's `ZmxLaunch.Runtime` inputs).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LiveProbe {
    /// A UI-test launch (`is_ui_test_launch`).
    pub ui_test_launch: bool,
    /// `AGTERM_UITEST_ENABLE_LIVE=1` opts a UI-test launch back in.
    pub ui_test_opt_in: bool,
    /// The bundled zmx executable.
    pub executable: PathBuf,
    /// The login shell in the password database.
    pub login_shell: Option<String>,
    /// The bundled resources directory holding `shell-integration/<shell>`.
    pub resources_dir: Option<PathBuf>,
    /// ngterm's own resources directory, holding `shell-integration/pwsh`.
    pub own_resources_dir: Option<PathBuf>,
}

/// The variable a UI-test launch sets to keep live sessions (upstream's `ZmxLaunch.uiTestOptInKey`).
pub const UI_TEST_OPT_IN: &str = "AGTERM_UITEST_ENABLE_LIVE";

impl LiveProbe {
    /// The first reason this host cannot run live sessions, in upstream's order; `None` when it can. After the UI-test
    /// bypass it is the wrap's own walk ([`crate::live::support::eligibility`]), so the order lives once.
    #[must_use]
    pub fn rejection(&self) -> Option<LiveRejection> {
        if self.ui_test_launch && !self.ui_test_opt_in {
            return Some(LiveRejection::UiTestLaunch);
        }
        eligibility(
            &self.executable,
            self.login_shell.as_deref(),
            self.resources_dir.as_deref(),
            self.own_resources_dir.as_deref(),
        )
        .err()
    }

    /// [`LiveProbe::rejection`]'s message.
    #[must_use]
    pub fn unavailable_reason(&self) -> Option<String> {
        self.rejection().map(|rejection| rejection.message().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn live_falls_back_to_fresh_shells_only_when_unavailable() {
        let reason = Some("the live-session executable is unavailable".to_owned());
        let fallback = RestoreLaunchDecision::new(RestoreMode::Live, reason.clone());
        assert_eq!(
            (fallback.requested, fallback.active),
            (RestoreMode::Live, RestoreMode::None)
        );
        assert!(fallback.live_fell_back());
        let live = RestoreLaunchDecision::new(RestoreMode::Live, None);
        assert_eq!(live.active, RestoreMode::Live);
        assert!(!live.live_fell_back());
        let rerun = RestoreLaunchDecision::new(RestoreMode::Rerun, reason);
        assert_eq!(rerun.active, RestoreMode::Rerun);
        assert!(rerun.replays());
    }

    #[test]
    fn the_status_flags_a_restart_and_names_only_a_refused_live_request() {
        let reason = Some("the live-session executable is unavailable".to_owned());
        let none = RestoreLaunchDecision::new(RestoreMode::None, reason.clone());
        let status = none.status(RestoreMode::None);
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({"configured": "none", "requestedAtLaunch": "none", "active": "none",
                               "restartRequired": false})
        );
        assert!(none.status(RestoreMode::Live).restart_required);
        let fallback = RestoreLaunchDecision::new(RestoreMode::Live, reason.clone());
        let status = fallback.status(RestoreMode::Live);
        assert_eq!(status.active, "none");
        assert_eq!(status.unavailable_reason, reason);
        assert!(!status.restart_required);
    }

    #[test]
    fn exit_captures_follow_the_configured_mode() {
        assert!(!captures_on_exit(RestoreMode::None));
        assert!(captures_on_exit(RestoreMode::Rerun));
        assert!(captures_on_exit(RestoreMode::Live));
    }

    #[test]
    fn the_probe_rejects_in_upstreams_order() {
        let dir = tempfile::tempdir().unwrap();
        let zmx = dir.path().join("zmx");
        let resources = dir.path().join("share");
        let own = dir.path().join("own");
        let mut probe = LiveProbe {
            ui_test_launch: true,
            ui_test_opt_in: false,
            executable: PathBuf::from("zmx"),
            login_shell: Some("/bin/tcsh".to_owned()),
            resources_dir: Some(resources.clone()),
            own_resources_dir: Some(own.clone()),
        };
        assert_eq!(probe.rejection(), Some(LiveRejection::UiTestLaunch));
        probe.ui_test_opt_in = true;
        assert_eq!(probe.rejection(), Some(LiveRejection::ExecutablePathNotAbsolute));
        probe.executable.clone_from(&zmx);
        assert_eq!(
            probe.unavailable_reason().as_deref(),
            Some("the live-session executable is unavailable")
        );
        fs::write(&zmx, "#!/bin/sh\n").unwrap();
        assert_eq!(probe.rejection(), Some(LiveRejection::ExecutableUnavailable));
        // what makes it executable: its mode on Unix, its extension on Windows
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&zmx, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        {
            let exe = zmx.with_extension("exe");
            fs::rename(&zmx, &exe).unwrap();
            probe.executable = exe;
        }
        assert_eq!(probe.rejection(), Some(LiveRejection::UnsupportedLoginShell));
        probe.login_shell = Some("/usr/bin/zsh".to_owned());
        assert_eq!(
            probe.rejection(),
            Some(LiveRejection::MissingShellIntegration(LoginShell::Zsh))
        );
        fs::create_dir_all(resources.join("shell-integration/zsh")).unwrap();
        fs::write(resources.join("shell-integration/zsh/.zshenv"), "").unwrap();
        assert_eq!(probe.rejection(), None);
        // bash and fish are probed for their own integration
        probe.login_shell = Some("/bin/bash".to_owned());
        assert_eq!(
            probe.unavailable_reason().as_deref(),
            Some("the bundled bash integration is unavailable")
        );
        fs::create_dir_all(resources.join("shell-integration/bash")).unwrap();
        fs::write(resources.join("shell-integration/bash/ghostty.bash"), "").unwrap();
        assert_eq!(probe.rejection(), None);
        probe.login_shell = Some("/usr/bin/fish".to_owned());
        assert_eq!(
            probe.unavailable_reason().as_deref(),
            Some("the bundled fish integration is unavailable")
        );
        // PowerShell by either name, and as Windows spells it: pwsh.exe, else powershell.exe
        for shell in [
            "/usr/bin/pwsh",
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        ] {
            probe.login_shell = Some(shell.to_owned());
            assert_eq!(
                probe.unavailable_reason().as_deref(),
                Some("the bundled PowerShell integration is unavailable"),
                "{shell}"
            );
        }
        fs::create_dir_all(own.join("shell-integration").join("pwsh")).unwrap();
        fs::write(own.join("shell-integration").join("pwsh").join("ngterm.ps1"), "").unwrap();
        assert_eq!(probe.rejection(), None);
        // a host without either (Windows' discovery found none) keeps the unsupported-shell rejection
        probe.login_shell = None;
        assert_eq!(probe.unavailable_reason().as_deref(), Some(UNSUPPORTED_LOGIN_SHELL));
    }

    #[test]
    fn powershell_is_probed_for_ngterms_own_integration_not_ghosttys() {
        let dir = tempfile::tempdir().unwrap();
        let resources = dir.path().join("share");
        let own = dir.path().join("own");
        let mut probe = LiveProbe {
            ui_test_launch: false,
            ui_test_opt_in: false,
            executable: crate::live::test_executable(dir.path(), "sh"),
            login_shell: Some("/usr/bin/pwsh".to_owned()),
            resources_dir: Some(resources.clone()),
            own_resources_dir: Some(own.clone()),
        };
        // in Ghostty's resources (an inherited GHOSTTY_RESOURCES_DIR's) it does not count
        for root in [&resources, &own] {
            fs::create_dir_all(root.join("shell-integration").join("pwsh")).unwrap();
            fs::write(root.join("shell-integration").join("pwsh").join("ngterm.ps1"), "").unwrap();
            assert_eq!(probe.rejection().is_none(), root == &own, "{}", root.display());
        }
        probe.own_resources_dir = None;
        assert_eq!(
            probe.rejection(),
            Some(LiveRejection::MissingShellIntegration(LoginShell::Pwsh))
        );
    }

    #[test]
    fn the_unsupported_shell_reason_names_what_this_os_runs() {
        let expected = if cfg!(windows) {
            "neither pwsh.exe nor powershell.exe is available"
        } else {
            "the password-database login shell is not zsh, bash, fish or PowerShell"
        };
        assert_eq!(LiveRejection::UnsupportedLoginShell.message(), expected);
    }
}
