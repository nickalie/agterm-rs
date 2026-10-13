//! Live sessions on this host (upstream's `restoredRuntime` zmx wiring in `agtermApp.swift`, `GhosttyApp`'s live
//! probe and `LiveResetRelauncher.swift`): the staged zmx as the core's session host, the launch steps that run before
//! any window mounts, which live capabilities this host claims (`live-sessions`, `live-any-shell`), and the reopen
//! after a Live sessions reset quit.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::SystemTime;

use agterm_core::paths::Env;
use agterm_core::zmx::build_record::launch_cutoff;
use agterm_core::zmx::shell_integration::LoginShell;
use agterm_core::zmx::support::Inputs;
use agterm_core::{Core, LiveInstall};
use agterm_proto::ControlZmxEndpoint;
use agterm_zmx::Runtime;
use agterm_zmx::launch::bundled_build_id;

/// The capability a host claims when its panes can run in zmx daemons in a zsh login shell, upstream's only one
/// (`conformance/docs/rust-build.md`).
pub const LIVE_SESSIONS: &str = "live-sessions";
/// The Rust-only capability a host claims when its panes can run in zmx daemons in its login shell, zsh, bash or fish.
pub const LIVE_ANY_SHELL: &str = "live-any-shell";

/// The script the relauncher runs: wait for `$1` to exit (at most `$3` polls of 0.2 s), then start `$2` with the
/// environment the app ran with. Arguments travel positionally, never inside the script text.
const RELAUNCH_SCRIPT: &str = r#"i=0
while kill -0 "$1" 2>/dev/null; do
  i=$((i + 1))
  [ "$i" -ge "$3" ] && exit 1
  sleep 0.2
done
exec "$2"
"#;
/// Polls of 0.2 s before the waiter gives up without launching.
const RELAUNCH_MAX_WAITS: u32 = 300;

/// The core's session host over the staged zmx and the wrap inputs of this host, installed before the first terminal;
/// then the launch reap and a pending Live sessions reset run. Call it once the restore decision and the process
/// table are in.
pub fn install(core: &mut Core, env: &dyn Env) {
    let runtime = Runtime::capture(env);
    let client = runtime.client();
    let endpoint = ControlZmxEndpoint {
        executable: runtime.zmx_executable.to_string_lossy().into_owned(),
        socket_directory: client.socket_directory().to_owned(),
    };
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().and_then(bundled_build_id));
    let outdated_before = launch_cutoff(bundled.as_deref(), &runtime.state_dir, SystemTime::now());
    let wrap = wrap_inputs(&runtime);
    core.install_live_sessions(LiveInstall {
        host: Arc::new(client),
        endpoint,
        wrap,
        state_dir: runtime.state_dir.clone(),
        outdated_before,
    });
    core.run_live_launch();
}

/// What every pane's wrap configuration starts from on this host; the pane's identity, environment and lead are
/// filled per pane. Linux has no supervising session host.
fn wrap_inputs(runtime: &Runtime) -> Inputs {
    Inputs {
        zmx_executable: runtime.zmx_executable.clone(),
        login_shell: runtime.login_shell.clone(),
        resources_dir: runtime.resources_dir.clone(),
        state_dir: runtime.state_dir.clone(),
        inherited: runtime.inherited.clone(),
        ..Inputs::default()
    }
}

/// The live capabilities this host claims: none unless it can wrap panes in zmx daemons (zmx staged beside the app, a
/// zsh, bash or fish password-database login shell and that shell's bundled integration); then `live-sessions` for a
/// zsh login shell, as upstream, and `live-any-shell` for any of the three.
#[must_use]
pub fn live_capabilities(env: &dyn Env) -> Vec<&'static str> {
    claims(&Runtime::capture(env), env)
}

fn claims(runtime: &Runtime, env: &dyn Env) -> Vec<&'static str> {
    if runtime.live_probe(env).rejection().is_some() {
        return Vec::new();
    }
    let zsh = runtime.login_shell.as_deref().and_then(LoginShell::of) == Some(LoginShell::Zsh);
    zsh.then_some(LIVE_SESSIONS)
        .into_iter()
        .chain([LIVE_ANY_SHELL])
        .collect()
}

/// Starts the waiter that reopens the app once this process exited, after a Live sessions reset quit; it inherits the
/// app's environment, so the new instance runs on the same state directory and socket. Whether it started.
pub fn spawn_relauncher() -> bool {
    let Ok(executable) = std::env::current_exe() else {
        tracing::error!("could not resolve the app executable to reopen after the live sessions reset");
        return false;
    };
    match relaunch_command(&executable, std::process::id()).spawn() {
        Ok(_) => true,
        Err(error) => {
            tracing::error!(%error, "could not start the relauncher");
            false
        }
    }
}

/// The waiter's command for `executable` after process `pid` exits, detached from the app's process group.
fn relaunch_command(executable: &Path, pid: u32) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(RELAUNCH_SCRIPT)
        .arg("agterm-live-reset")
        .arg(pid.to_string())
        .arg(executable)
        .arg(RELAUNCH_MAX_WAITS.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    use agterm_core::paths::MapEnv;

    use super::*;

    #[test]
    fn a_host_without_a_staged_zmx_claims_no_live_sessions() {
        // the test binary has no zmx beside it
        assert!(live_capabilities(&MapEnv::new()).is_empty());
    }

    #[test]
    fn live_sessions_need_zsh_and_live_any_shell_takes_bash_and_fish_too() {
        let dir = tempfile::tempdir().unwrap();
        let zmx = dir.path().join("zmx");
        std::fs::write(&zmx, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&zmx, std::fs::Permissions::from_mode(0o755)).unwrap();
        let resources = dir.path().join("ghostty");
        for loader in [
            "shell-integration/zsh/.zshenv",
            "shell-integration/bash/ghostty.bash",
            "shell-integration/fish/vendor_conf.d/ghostty-shell-integration.fish",
        ] {
            let path = resources.join(loader);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let runtime = |shell: &str| Runtime {
            zmx_executable: zmx.clone(),
            login_shell: Some(shell.to_owned()),
            resources_dir: Some(resources.clone()),
            state_dir: dir.path().join("state"),
            ..Runtime::default()
        };
        let env = MapEnv::new();
        assert_eq!(claims(&runtime("/bin/zsh"), &env), [LIVE_SESSIONS, LIVE_ANY_SHELL]);
        assert_eq!(claims(&runtime("/bin/bash"), &env), [LIVE_ANY_SHELL]);
        assert_eq!(claims(&runtime("/usr/bin/fish"), &env), [LIVE_ANY_SHELL]);
        assert!(claims(&runtime("/bin/tcsh"), &env).is_empty());
        std::fs::remove_file(resources.join("shell-integration/bash/ghostty.bash")).unwrap();
        assert!(claims(&runtime("/bin/bash"), &env).is_empty());
    }

    #[test]
    fn the_relauncher_waits_for_the_pid_then_starts_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("started");
        let app = dir.path().join("app");
        std::fs::write(&app, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut waited = Command::new("sleep").arg("0.3").spawn().unwrap();
        let mut relauncher = relaunch_command(&app, waited.id()).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!marker.exists(), "started before the app exited");
        waited.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(marker.exists(), "never started");
        relauncher.wait().unwrap();
    }

    #[test]
    fn the_wrap_inputs_carry_the_host_and_no_session_host() {
        let runtime = Runtime {
            zmx_executable: "/opt/agterm/bin/zmx".into(),
            login_shell: Some("/bin/zsh".to_owned()),
            resources_dir: Some("/opt/agterm/share/ghostty".into()),
            state_dir: "/tmp/state".into(),
            inherited: agterm_core::zmx::shell_integration::Inherited {
                zdotdir: Some("/home/u/.zsh".to_owned()),
                ..Default::default()
            },
        };
        let inputs = wrap_inputs(&runtime);
        assert_eq!(inputs.zmx_executable, runtime.zmx_executable);
        assert_eq!(inputs.login_shell, runtime.login_shell);
        assert_eq!(inputs.state_dir, runtime.state_dir);
        assert_eq!(inputs.inherited, runtime.inherited);
        assert_eq!(inputs.session_host_executable, None);
    }
}
