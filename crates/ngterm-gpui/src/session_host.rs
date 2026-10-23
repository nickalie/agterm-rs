//! What every pane runs under (the GTK build's `zmx_runtime.rs` launch part and `ghostty_app.rs` config part, over
//! `ngterm-term` instead of libghostty): the staged session server (`ngterm-session`, in upstream's zmx's place) as the
//! core's session host, the live capabilities this host claims, the ghostty config the terminals draw with, and the
//! [`Host`] their launches start under.
//!
//! - **Live sessions** (upstream's `restoredRuntime` zmx wiring in `agtermApp.swift`): [`install`] hands a running
//!   server of an older build over to the staged one (the update handoff, recorded for the outdated cutoff), gives the
//!   core the server's `SessionHost` and this host's wrap inputs before any window mounts, then runs the launch reap
//!   and a pending Live sessions reset; [`live_capabilities`] claims `live-sessions` (a zsh login shell, as upstream)
//!   and `live-any-shell` (zsh, bash, fish or PowerShell) where `ngterm-session` and the shell's integration are
//!   staged. A live pane attaches to its session in process (`ngterm-term`'s live panes), as the session's reply
//!   owner.
//! - **Config**: [`AppConfig::load`] loads the ghostty config as libghostty did for the GTK build (defaults, the
//!   user's files, their includes; the theme under them), on the side of a dual theme the desktop's appearance picks
//!   (`appearance.rs`); its diagnostic count is what `config.reload` answers.
//! - **Launch host**: Ghostty's resources (staged beside the binary, else a system package) give the terminal its
//!   identity and shell integration. A pane without a command runs the config's `command` when one is set (libghostty
//!   ran a surface's command from the config, which the conformance harness's `command = /bin/sh` relies on), else the
//!   password database's login shell.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use ngterm_core::ghostty_config::{self, FinalConfig, GhosttyConfig, LoadedConfig, Step};
use ngterm_core::live::build_record::{launch_cutoff, record_handoff};
use ngterm_core::live::shell_integration::LoginShell;
use ngterm_core::live::support::Inputs;
use ngterm_core::paths::{self, Env};
use ngterm_core::{Core, LiveInstall};
use ngterm_proto::ControlLiveEndpoint;
use ngterm_session::client::Client;
use ngterm_session::host::ServerHost;
use ngterm_session::launch::Runtime;
use ngterm_session::server::spawn::{HandoffAttempt, StartOptions};
use ngterm_term::{Host, Launch, Pane};

use crate::pane::Launcher;
use crate::terminal::{Spawn, TerminalConfig};

/// The capability a host claims when its panes can run in live sessions in a zsh login shell, upstream's only one
/// (`conformance/docs/rust-build.md`).
pub const LIVE_SESSIONS: &str = "live-sessions";
/// The Rust-only capability a host claims when its panes can run in live sessions in its login shell, zsh, bash, fish
/// or PowerShell.
pub const LIVE_ANY_SHELL: &str = "live-any-shell";

/// The core's session host over the staged `ngterm-session` and the wrap inputs of this host, installed before the
/// first terminal, once a running server of an older build handed its sessions over ([`update`]); then the launch
/// reap and a pending Live sessions reset run. Call it once the restore decision and the process table are in.
pub fn install(core: &mut Core, env: &dyn Env) {
    let runtime = Runtime::capture(env);
    let directory = ngterm_core::live::socket_directory(&runtime.state_dir);
    let endpoint = ControlLiveEndpoint {
        executable: runtime.executable.to_string_lossy().into_owned(),
        socket_directory: directory.clone(),
    };
    // the build every client of this host expects: the one staged for the executable it starts (the debug override's
    // when one is set), as the host's own calls and each pane's attach expect it
    let options = StartOptions::new(&runtime.executable);
    let bundled = options.build.clone();
    update(&runtime, &directory, &options);
    let outdated_before = launch_cutoff(bundled.as_deref(), &runtime.state_dir, SystemTime::now());
    let wrap = wrap_inputs(&runtime);
    core.install_live_sessions(LiveInstall {
        host: Arc::new(ServerHost::new(runtime.executable.clone(), directory)),
        endpoint,
        wrap,
        state_dir: runtime.state_dir.clone(),
        outdated_before,
    });
    core.run_live_launch();
}

/// The update handoff (`ngterm_session::server::handoff`): a server of another build than the one `options` expect
/// running in `directory` is asked to hand its sessions over to the staged `ngterm-session`, and a committed handoff is
/// recorded, so the sessions it carried over are not offered for reset as predating this build
/// ([`record_handoff`]); a failed one records nothing, and the reset offer stands. Nothing without a running server
/// or a staged id (an unstaged build checks no build). Before any pane attaches: after a committed handoff each pane's
/// `ngterm-session attach` finds the staged build and asks for nothing; after a failed one each asks the old server
/// once more (a process of its own, it never saw this attempt), which answers with the same failure at once and
/// disconnects nobody, and the pane attaches to it (to its session the old server holds under the name from before the
/// build's rename, when it holds that one: `Client::legacy_stand_in`, whose host lists it under the pane's name).
fn update(runtime: &Runtime, directory: &str, options: &StartOptions) {
    let Some(bundled) = options.build.as_deref() else {
        return;
    };
    if !ngterm_core::live::support::is_executable_file(&runtime.executable) {
        return;
    }
    match Client::new(directory).update(options) {
        Ok(None) => {}
        Ok(Some(HandoffAttempt::Committed { from })) => {
            tracing::info!(%from, to = bundled, "live sessions handed over to this build");
            record_handoff(Some(bundled), &runtime.state_dir, SystemTime::now());
        }
        Ok(Some(HandoffAttempt::Failed { from, reason })) => {
            tracing::warn!(%from, to = bundled, %reason, "the live sessions stay with the older build");
        }
        Err(error) => tracing::warn!(%error, "the live session server could not be updated"),
    }
}

/// What every pane's wrap configuration starts from on this host; the pane's identity, environment and lead are
/// filled per pane. Linux has no supervising session host.
#[must_use]
pub fn wrap_inputs(runtime: &Runtime) -> Inputs {
    Inputs {
        executable: runtime.executable.clone(),
        login_shell: runtime.login_shell.clone(),
        resources_dir: runtime.resources_dir.clone(),
        own_resources_dir: runtime.own_resources_dir.clone(),
        state_dir: runtime.state_dir.clone(),
        inherited: runtime.inherited.clone(),
        ..Inputs::default()
    }
}

/// The live capabilities this host claims: none unless it can wrap panes in live sessions (`ngterm-session` staged
/// beside the app, a zsh, bash, fish or PowerShell login shell and that shell's bundled integration); then
/// `live-sessions` for a zsh login shell, as upstream, and `live-any-shell` for any of them.
#[must_use]
pub fn live_capabilities(env: &dyn Env) -> Vec<&'static str> {
    claims(&Runtime::capture(env), env)
}

/// [`live_capabilities`] of `runtime`.
#[must_use]
pub fn claims(runtime: &Runtime, env: &dyn Env) -> Vec<&'static str> {
    if runtime.live_probe(env).rejection().is_some() {
        return Vec::new();
    }
    let zsh = runtime.login_shell.as_deref().and_then(LoginShell::of) == Some(LoginShell::Zsh);
    zsh.then_some(LIVE_SESSIONS)
        .into_iter()
        .chain([LIVE_ANY_SHELL])
        .collect()
}

/// Ghostty's resources for this executable: staged beside it, else a system package; `None` without the compiled
/// `xterm-ghostty` beside them.
#[must_use]
pub fn resources() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    ngterm_term::resolve_resources(&ngterm_term::resource_candidates(exe.as_deref()), Path::exists)
}

/// The bundled themes' directory of `resources`.
#[must_use]
pub fn themes_dir(resources: Option<&Path>) -> Option<PathBuf> {
    resources.map(|dir| dir.join("themes"))
}

/// The app's ghostty config as loaded last.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub config: GhosttyConfig,
    /// The config's `command`, what a pane without one runs; `None` when unset.
    pub command: Option<String>,
    /// How many diagnostics loading it gave (`config.reload`'s answer).
    pub diagnostics: usize,
    /// The side of the desktop's appearance it was loaded on.
    pub dark: bool,
}

impl AppConfig {
    /// Loads the config in libghostty's order, the theme looked up in the user's and the bundled `themes`, on the
    /// `dark` side of a dual theme. Each diagnostic is logged.
    #[must_use]
    pub fn load(env: &dyn Env, resources: Option<&Path>, dark: bool) -> Self {
        Self::load_with(env, resources, dark, None)
    }

    /// [`AppConfig::load`] with a terminal's own `overlay` loaded last (a session or pane background).
    #[must_use]
    pub fn load_with(env: &dyn Env, resources: Option<&Path>, dark: bool, overlay: Option<&str>) -> Self {
        let loaded = ghostty_config::load_app_config(env, resources.is_some(), overlay);
        let theme_dirs = ghostty_config::theme_dirs(env, themes_dir(resources).as_deref());
        let FinalConfig {
            config,
            diagnostics,
            theme_path,
        } = loaded.finalize(dark, &theme_dirs);
        for diagnostic in &diagnostics {
            tracing::warn!(%diagnostic, "ghostty config");
        }
        tracing::info!(
            theme = ?theme_path,
            diagnostics = diagnostics.len(),
            "ghostty config loaded"
        );
        Self {
            command: command_of(&loaded),
            config,
            diagnostics: diagnostics.len(),
            dark,
        }
    }

    /// The terminals' share of the config, on the side it was loaded on.
    #[must_use]
    pub fn terminal(&self) -> TerminalConfig {
        TerminalConfig::from_ghostty(&self.config).on_side(self.dark)
    }
}

/// The last `command` line of `loaded`, without Ghostty's `shell:`/`direct:` prefix (a `direct:` command still runs
/// through `/bin/sh -c` here); an empty value resets it.
fn command_of(loaded: &LoadedConfig) -> Option<String> {
    let mut command = None;
    for step in loaded.steps() {
        if let Step::Setting(setting) = step
            && setting.key == "command"
        {
            command = setting
                .value
                .as_deref()
                .map(str::trim)
                .map(|value| {
                    value
                        .strip_prefix("shell:")
                        .or_else(|| value.strip_prefix("direct:"))
                        .unwrap_or(value)
                })
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
        }
    }
    command
}

/// The host a pane launches under for `app`: the config's `command` for a pane without one, else the login shell;
/// the terminal identity from `resources`.
#[must_use]
pub fn launch_host(app: &AppConfig, resources: Option<PathBuf>, env: &dyn Env) -> Host {
    let runtime = Runtime::capture(env);
    let shell = app.command.clone().or(runtime.login_shell);
    let bin_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    Host::new(&app.config, shell, resources, bin_dir)
}

/// Launches panes under the host `host` holds (replaced on a config reload).
#[must_use]
pub fn launcher(host: Rc<RefCell<Host>>) -> Launcher {
    Rc::new(move |launch: &Launch| -> Spawn {
        let (launch, host) = (launch.clone(), host.borrow().clone());
        Box::new(move |config| Pane::launch(&launch, &host, &config))
    })
}

/// The settings' ghostty config path, which the Settings popup writes and the config loads last.
#[must_use]
pub fn settings_config_path(env: &dyn Env) -> PathBuf {
    paths::ghostty_settings_config_path(env)
}

#[cfg(test)]
mod tests {
    use ngterm_core::paths::MapEnv;

    use super::*;

    fn loaded(text: &str) -> LoadedConfig {
        let mut loader = ghostty_config::Loader::new(None);
        loader.load_text("test", text);
        loader.finish()
    }

    #[test]
    fn the_config_command_is_the_last_line_without_its_prefix() {
        assert_eq!(command_of(&loaded("")), None);
        assert_eq!(command_of(&loaded("command = /bin/sh\n")).as_deref(), Some("/bin/sh"));
        assert_eq!(
            command_of(&loaded("command = /bin/sh\ncommand = shell:zsh -l\n")).as_deref(),
            Some("zsh -l")
        );
        assert_eq!(command_of(&loaded("command = direct:nvim\n")).as_deref(), Some("nvim"));
        assert_eq!(
            command_of(&loaded("command = /bin/sh\ncommand =\n")),
            None,
            "an empty value resets it"
        );
    }

    #[test]
    fn the_config_command_runs_in_place_of_the_login_shell() {
        let app = AppConfig {
            config: GhosttyConfig::default(),
            command: Some("/bin/sh".to_owned()),
            diagnostics: 0,
            dark: false,
        };
        let env = MapEnv::new().with("HOME", "/home/u");
        let host = launch_host(&app, None, &env);
        assert_eq!(host.shell, "/bin/sh");
        let prepared = host.prepare(&Launch::default(), &env);
        assert!(
            prepared.command.args.iter().any(|arg| arg == "/bin/sh"),
            "{:?}",
            prepared.command
        );
    }

    #[test]
    fn the_configs_launch_keys_reach_every_pane_the_host_launches() {
        let app = AppConfig {
            config: loaded("env = NGTERM_TEST_VAR=1\ninput = raw:echo hi\\n\nwait-after-command = true\n")
                .finalize(false, &[])
                .config,
            command: None,
            diagnostics: 0,
            dark: false,
        };
        let env = MapEnv::new().with("HOME", "/home/u");
        let prepared = launch_host(&app, None, &env).prepare(&Launch::default(), &env);
        assert!(
            prepared
                .command
                .env
                .iter()
                .any(|(key, value)| key == "NGTERM_TEST_VAR" && value == "1"),
            "{:?}",
            prepared.command
        );
        assert_eq!(prepared.initial_input.as_deref(), Some(&b"echo hi\n"[..]));
        assert!(matches!(
            prepared.exit,
            ngterm_term::ExitPolicy::Launched {
                wait_after_command: true,
                ..
            }
        ));
    }

    #[test]
    fn a_host_without_the_session_server_claims_no_live_capability() {
        let runtime = Runtime {
            executable: PathBuf::from("/nonexistent/ngterm-session"),
            login_shell: Some("/bin/zsh".to_owned()),
            resources_dir: None,
            own_resources_dir: None,
            state_dir: PathBuf::from("/tmp/ngterm-state"),
            inherited: ngterm_core::live::shell_integration::Inherited::default(),
        };
        assert!(claims(&runtime, &MapEnv::new()).is_empty());
    }
}
