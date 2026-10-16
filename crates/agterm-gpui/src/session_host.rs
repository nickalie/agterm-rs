//! What every pane runs under (the GTK build's `zmx_runtime.rs` launch part and `ghostty_app.rs` config part, over
//! `agterm-term` instead of libghostty): the staged zmx as the core's session host, the live capabilities this host
//! claims, the ghostty config the terminals draw with, and the [`Host`] their launches start under.
//!
//! - **Live sessions** (upstream's `restoredRuntime` zmx wiring in `agtermApp.swift`): [`install`] gives the core the
//!   zmx client and this host's wrap inputs before any window mounts, then runs the launch reap and a pending Live
//!   sessions reset; [`live_capabilities`] claims `live-sessions` (a zsh login shell, as upstream) and
//!   `live-any-shell` (zsh, bash or fish) where zmx and the shell's integration are staged.
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

use agterm_core::ghostty_config::{self, FinalConfig, GhosttyConfig, LoadedConfig, Step};
use agterm_core::paths::{self, Env};
use agterm_core::zmx::build_record::launch_cutoff;
use agterm_core::zmx::shell_integration::LoginShell;
use agterm_core::zmx::support::Inputs;
use agterm_core::{Core, LiveInstall};
use agterm_proto::ControlZmxEndpoint;
use agterm_term::{Host, Launch, Pane};
use agterm_zmx::Runtime;
use agterm_zmx::launch::bundled_build_id;

use crate::pane::Launcher;
use crate::terminal::{Spawn, TerminalConfig};

/// The capability a host claims when its panes can run in zmx daemons in a zsh login shell, upstream's only one
/// (`conformance/docs/rust-build.md`).
pub const LIVE_SESSIONS: &str = "live-sessions";
/// The Rust-only capability a host claims when its panes can run in zmx daemons in its login shell, zsh, bash or fish.
pub const LIVE_ANY_SHELL: &str = "live-any-shell";

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
#[must_use]
pub fn wrap_inputs(runtime: &Runtime) -> Inputs {
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
    agterm_term::resolve_resources(&agterm_term::resource_candidates(exe.as_deref()), Path::exists)
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
    use agterm_core::paths::MapEnv;

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
    fn a_host_without_zmx_claims_no_live_capability() {
        let runtime = Runtime {
            zmx_executable: PathBuf::from("/nonexistent/zmx"),
            login_shell: Some("/bin/zsh".to_owned()),
            resources_dir: None,
            state_dir: PathBuf::from("/tmp/agterm-state"),
            inherited: agterm_core::zmx::shell_integration::Inherited::default(),
        };
        assert!(claims(&runtime, &MapEnv::new()).is_empty());
    }
}
