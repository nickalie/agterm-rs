//! The restore decision this launch takes before its first terminal (upstream's `GhosttyApp.init` latching
//! `restoreLaunchDecision` from the settings and `ZmxLaunch.liveUnavailableReason`), and the host facts the foreground
//! reads need: the user's `$SHELL`. The live probe's inputs (the staged `agterm-session`, the password-database login
//! shell, its integration) are [`Runtime`]'s.

use agterm_core::command_restore::basename;
use agterm_core::paths::Env;
use agterm_core::persist::AppSettings;
use agterm_core::restore::mode::RestoreLaunchDecision;
use agterm_session::launch::Runtime;

/// The decision for this launch: the settings' mode, `live` falling back to fresh shells with the probe's reason. Call
/// it after libghostty's resources dir is exported.
pub fn launch_decision(settings: &AppSettings, env: &dyn Env) -> RestoreLaunchDecision {
    let probe = Runtime::capture(env).live_probe(env);
    let decision = RestoreLaunchDecision::new(settings.effective_restore_mode(), probe.unavailable_reason());
    tracing::info!(
        requested = decision.requested.wire_name(),
        active = decision.active.wire_name(),
        reason = decision.live_unavailable_reason.as_deref().unwrap_or(""),
        "restore mode for this launch"
    );
    decision
}

/// The basename of the user's `$SHELL`, which widens shell recognition in the foreground reads.
pub fn shell_basename(env: &dyn Env) -> Option<String> {
    let shell = env.var("SHELL")?;
    let shell = shell.to_string_lossy();
    (!shell.is_empty()).then(|| basename(&shell).to_owned())
}

#[cfg(test)]
mod tests {
    use agterm_core::paths::MapEnv;
    use agterm_core::persist::RestoreMode;

    use super::*;

    #[test]
    fn a_live_request_without_a_bundled_session_server_falls_back_with_the_reason() {
        let settings = AppSettings {
            restore_mode: Some(RestoreMode::Live),
            ..AppSettings::default()
        };
        let decision = launch_decision(&settings, &MapEnv::new());
        assert_eq!(decision.active, RestoreMode::None);
        assert_eq!(
            decision.live_unavailable_reason.as_deref(),
            Some("the live-session executable is unavailable")
        );
    }

    #[test]
    fn the_shell_basename_comes_from_shell() {
        assert_eq!(
            shell_basename(&MapEnv::new().with("SHELL", "/usr/bin/fish")).as_deref(),
            Some("fish")
        );
        assert_eq!(shell_basename(&MapEnv::new().with("SHELL", "")), None);
        assert_eq!(shell_basename(&MapEnv::new()), None);
    }
}
