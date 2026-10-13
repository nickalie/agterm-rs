//! The restore decision this launch takes before its first terminal (upstream's `GhosttyApp.init` latching
//! `restoreLaunchDecision` from the settings and `ZmxLaunch.liveUnavailableReason`), and the host facts it and the
//! foreground reads need: the bundled zmx, the password-database login shell, the user's `$SHELL`.

use std::ffi::CStr;
use std::path::PathBuf;

use agterm_core::command_restore::basename;
use agterm_core::paths::Env;
use agterm_core::persist::AppSettings;
use agterm_core::restore::mode::{LiveProbe, RestoreLaunchDecision, UI_TEST_OPT_IN};

/// The variable a debug build reads for another zmx executable (upstream's `AGTERM_ZMX_PATH`, honored in DEBUG only).
const ZMX_PATH_OVERRIDE: &str = "AGTERM_ZMX_PATH";

/// The decision for this launch: the settings' mode, `live` falling back to fresh shells with the probe's reason.
pub fn launch_decision(settings: &AppSettings, env: &dyn Env) -> RestoreLaunchDecision {
    let decision = RestoreLaunchDecision::new(settings.effective_restore_mode(), live_probe(env).unavailable_reason());
    tracing::info!(
        requested = decision.requested.wire_name(),
        active = decision.active.wire_name(),
        reason = decision.live_unavailable_reason.as_deref().unwrap_or(""),
        "restore mode for this launch"
    );
    decision
}

/// What the live-session probe reads of this host. Call it after libghostty's resources dir is exported.
fn live_probe(env: &dyn Env) -> LiveProbe {
    let var = |key: &str| env.var(key).filter(|value| !value.is_empty());
    let override_path = var(ZMX_PATH_OVERRIDE).filter(|_| cfg!(debug_assertions));
    let zmx_executable = override_path.map_or_else(
        || {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("zmx")))
                .unwrap_or_default()
        },
        PathBuf::from,
    );
    LiveProbe {
        ui_test_launch: agterm_core::is_ui_test_launch(env),
        ui_test_opt_in: var(UI_TEST_OPT_IN).is_some_and(|value| value == "1"),
        zmx_executable,
        login_shell: login_shell(),
        resources_dir: var("GHOSTTY_RESOURCES_DIR").map(PathBuf::from),
    }
}

/// The basename of the user's `$SHELL`, which widens shell recognition in the foreground reads.
pub fn shell_basename(env: &dyn Env) -> Option<String> {
    let shell = env.var("SHELL")?;
    let shell = shell.to_string_lossy();
    (!shell.is_empty()).then(|| basename(&shell).to_owned())
}

/// The login shell the password database names for this user.
fn login_shell() -> Option<String> {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let mut buffer: Vec<libc::c_char> = vec![0; 16 * 1024];
    // SAFETY: `passwd` is plain data, for which all zero bytes is a valid value; `getpwuid_r` fills it.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call and the buffer's length is passed beside it.
    let status = unsafe { libc::getpwuid_r(uid, &raw mut entry, buffer.as_mut_ptr(), buffer.len(), &raw mut found) };
    if status != 0 || found.is_null() || entry.pw_shell.is_null() {
        return None;
    }
    // SAFETY: on success `pw_shell` points at a NUL-terminated string inside `buffer`, which is alive here.
    let shell = unsafe { CStr::from_ptr(entry.pw_shell) };
    Some(shell.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use agterm_core::paths::MapEnv;
    use agterm_core::persist::RestoreMode;

    use super::*;

    #[test]
    fn a_live_request_without_a_bundled_zmx_falls_back_with_the_reason() {
        let settings = AppSettings {
            restore_mode: Some(RestoreMode::Live),
            ..AppSettings::default()
        };
        let decision = launch_decision(&settings, &MapEnv::new());
        assert_eq!(decision.active, RestoreMode::None);
        assert_eq!(
            decision.live_unavailable_reason.as_deref(),
            Some("the zmx executable is unavailable")
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

    #[test]
    fn the_password_database_names_a_login_shell() {
        assert!(login_shell().is_some_and(|shell| shell.starts_with('/')));
    }
}
