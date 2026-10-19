//! Where state lives: the state directory and the control socket, in upstream's precedence
//! (`ControlServer.defaultSocketPath` in `agterm/Control/ControlServer.swift`: `AGTERM_CONTROL_SOCKET`, else
//! `ControlResolve.socketPath` over `AGTERM_STATE_DIR` or `PersistenceStore.defaultDirectory`).
//!
//! The Linux base is glue taken from the Linux fork: `PersistenceStore.defaultDirectory` is Foundation's application
//! support directory, which on Linux is the XDG data home, and an empty variable counts as unset, as the fork's
//! `linuxStateDirectory()` (`agterm-linux/Sources/AgtermLinux/App.swift`) treats `AGTERM_STATE_DIR`; upstream would
//! take an empty value as given. The directory name is this build's own (`ngterm`), not the fork's, since the two
//! register different application ids and so cannot keep each other off one directory.
//!
//! Every function reads the environment through [`Env`], so tests pass a [`MapEnv`] instead of mutating the
//! process environment.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Overrides the whole state directory (test isolation, `run-gpui-dev.sh`).
pub const STATE_DIR_ENV: &str = "AGTERM_STATE_DIR";
/// Overrides the app's control socket path. Only the app reads it: `ngtermctl` resolves through the state dir.
pub const CONTROL_SOCKET_ENV: &str = "AGTERM_CONTROL_SOCKET";
/// The control socket's file name inside the state directory.
pub const SOCKET_FILE_NAME: &str = "ngterm.sock";
/// The state directory's name under the XDG data home, the config directory's under `~/.config` and the HUD bodies'
/// under the runtime dir.
pub const APP_DIR_NAME: &str = "ngterm";
/// The base used when no home directory can be found, as upstream's `agtermctl` (`/tmp/agterm`) under this build's
/// state directory name.
pub const FALLBACK_STATE_DIR: &str = "/tmp/ngterm";

/// The process environment as the resolvers see it.
pub trait Env {
    /// The variable `key`, unset or not.
    fn var(&self, key: &str) -> Option<OsString>;
    /// The home directory from the password database, the fallback when `HOME` is unset or empty.
    fn passwd_home(&self) -> Option<PathBuf>;
}

/// The real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }

    fn passwd_home(&self) -> Option<PathBuf> {
        std::env::home_dir()
    }
}

/// A fixed environment for tests.
#[derive(Debug, Clone, Default)]
pub struct MapEnv {
    vars: HashMap<String, OsString>,
    passwd_home: Option<PathBuf>,
}

impl MapEnv {
    /// An empty environment with no password-database home.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `key` to `value`.
    #[must_use]
    pub fn with(mut self, key: &str, value: impl Into<OsString>) -> Self {
        self.vars.insert(key.to_owned(), value.into());
        self
    }

    /// Sets the password-database home.
    #[must_use]
    pub fn with_passwd_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.passwd_home = Some(home.into());
        self
    }
}

impl Env for MapEnv {
    fn var(&self, key: &str) -> Option<OsString> {
        self.vars.get(key).cloned()
    }

    fn passwd_home(&self) -> Option<PathBuf> {
        self.passwd_home.clone()
    }
}

/// A variable that is set and non-empty.
fn non_empty(env: &dyn Env, key: &str) -> Option<OsString> {
    env.var(key).filter(|value| !value.is_empty())
}

/// The user's home: `HOME` when set and non-empty, else the password database.
#[must_use]
pub fn home_dir(env: &dyn Env) -> Option<PathBuf> {
    non_empty(env, "HOME").map(PathBuf::from).or_else(|| env.passwd_home())
}

/// The XDG data home as Foundation reads it: `XDG_DATA_HOME` when it is an absolute path longer than `/`, else
/// `<home>/.local/share`.
#[must_use]
pub fn xdg_data_home(env: &dyn Env) -> Option<PathBuf> {
    let explicit = env
        .var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.as_os_str().len() > 1);
    explicit.or_else(|| home_dir(env).map(|home| home.join(".local/share")))
}

/// The state directory with no override: `<XDG data home>/ngterm`, or [`FALLBACK_STATE_DIR`] without a home.
#[must_use]
pub fn default_state_dir(env: &dyn Env) -> PathBuf {
    xdg_data_home(env).map_or_else(|| PathBuf::from(FALLBACK_STATE_DIR), |base| base.join(APP_DIR_NAME))
}

/// The state directory: a non-empty `AGTERM_STATE_DIR`, else [`default_state_dir`].
#[must_use]
pub fn state_dir(env: &dyn Env) -> PathBuf {
    non_empty(env, STATE_DIR_ENV).map_or_else(|| default_state_dir(env), PathBuf::from)
}

/// The control socket inside `state_dir`; the rendezvous the app and `ngtermctl` share.
#[must_use]
pub fn socket_in(state_dir: &Path) -> PathBuf {
    state_dir.join(SOCKET_FILE_NAME)
}

/// The socket the app binds: a non-empty `AGTERM_CONTROL_SOCKET`, else `<state dir>/ngterm.sock`.
#[must_use]
pub fn app_socket_path(env: &dyn Env) -> PathBuf {
    non_empty(env, CONTROL_SOCKET_ENV).map_or_else(|| socket_in(&state_dir(env)), PathBuf::from)
}

/// The directory of the user-editable config files (`keymap.conf`, `hooks.conf`, the agterm-scoped `ghostty.conf`),
/// as upstream's `ConfigPaths.configDirectory`: a non-empty `setting` (`AppSettings.configDirectory`), else
/// `<AGTERM_STATE_DIR>/config` when the state dir is overridden, else `<home>/.config/ngterm`.
#[must_use]
pub fn config_dir(env: &dyn Env, setting: Option<&Path>) -> PathBuf {
    if let Some(setting) = setting.filter(|path| !path.as_os_str().is_empty()) {
        return setting.to_path_buf();
    }
    if let Some(state) = non_empty(env, STATE_DIR_ENV) {
        return PathBuf::from(state).join("config");
    }
    home_dir(env).map_or_else(
        || Path::new(FALLBACK_STATE_DIR).join("config"),
        |home| home.join(".config").join(APP_DIR_NAME),
    )
}

/// What [`config_dir`] reads of the environment, captured once so the core can resolve the config directory again
/// whenever the `configDirectory` setting changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigLocator {
    state_dir_override: Option<OsString>,
    home: Option<PathBuf>,
}

impl ConfigLocator {
    /// The locator of `env`.
    #[must_use]
    pub fn capture(env: &dyn Env) -> Self {
        Self {
            state_dir_override: non_empty(env, STATE_DIR_ENV),
            home: home_dir(env),
        }
    }

    /// [`config_dir`] under the captured environment.
    #[must_use]
    pub fn config_dir(&self, setting: Option<&Path>) -> PathBuf {
        let mut env = MapEnv::new();
        if let Some(state) = &self.state_dir_override {
            env = env.with(STATE_DIR_ENV, state.clone());
        }
        if let Some(home) = &self.home {
            env = env.with("HOME", home.clone());
        }
        config_dir(&env, setting)
    }

    /// The captured home directory.
    #[must_use]
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }
}

/// Where the HUD body files go: a private per-user directory standing in for upstream's `NSTemporaryDirectory()`
/// (a per-user 0700 directory on macOS), never the shared `/tmp`. `<AGTERM_STATE_DIR>/hud` when the state dir is
/// overridden (instances stay apart, as [`config_dir`]), else `<XDG_RUNTIME_DIR>/ngterm/hud` when that is an absolute
/// path (the user's 0700 runtime directory, emptied at logout), else `<state dir>/hud`. The caller makes it 0700.
#[must_use]
pub fn hud_body_dir(env: &dyn Env) -> PathBuf {
    if let Some(state) = non_empty(env, STATE_DIR_ENV) {
        return PathBuf::from(state).join("hud");
    }
    let runtime = env
        .var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.as_os_str().len() > 1);
    runtime.map_or_else(
        || state_dir(env).join("hud"),
        |base| base.join(APP_DIR_NAME).join("hud"),
    )
}

/// The agterm-scoped ghostty config inside a config directory: `<dir>/ghostty.conf`.
#[must_use]
pub fn ghostty_config_path(config_dir: &Path) -> PathBuf {
    config_dir.join("ghostty.conf")
}

/// The user's own ghostty config, which the Settings toggle `inheritGlobalGhosttyConfig` loads below the
/// agterm-scoped one: `<XDG config home>/ghostty/config`, the XDG config home being an absolute `XDG_CONFIG_HOME`,
/// else `<home>/.config` (upstream reads `~/.config/ghostty/config`). `None` without a home.
#[must_use]
pub fn global_ghostty_config_path(env: &dyn Env) -> Option<PathBuf> {
    let explicit = env
        .var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.as_os_str().len() > 1);
    explicit
        .or_else(|| home_dir(env).map(|home| home.join(".config")))
        .map(|base| base.join("ghostty").join("config"))
}

/// The ghostty config the Settings window writes: `<state dir>/ghostty-settings.conf`
/// (`GhosttyApp.settingsConfigURL`).
#[must_use]
pub fn ghostty_settings_config_path(env: &dyn Env) -> PathBuf {
    state_dir(env).join("ghostty-settings.conf")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_owned();
    raw.push(OsStr::new(suffix));
    PathBuf::from(raw)
}

/// `<socket>.lock`: the file whose `flock` owns the socket path. It is never removed: unlinking it would let the next
/// instance lock a fresh inode and exclude nobody.
#[must_use]
pub fn ownership_lock_path(socket: &Path) -> PathBuf {
    with_suffix(socket, ".lock")
}

/// `<socket>.unavailable`: what a refused instance advertises as `AGTERM_SOCKET`, a path nothing creates, so its
/// shells cannot reach the instance that owns the socket.
#[must_use]
pub fn unavailable_socket_path(socket: &Path) -> PathBuf {
    with_suffix(socket, ".unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(path: &str) -> MapEnv {
        MapEnv::new().with("HOME", path)
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the XDG base directories as Unix paths; the Windows locations belong to the GPUI-on-Windows plan"
    )]
    fn hud_bodies_go_to_the_overridden_state_dir_else_the_runtime_dir_else_the_state_dir() {
        let runtime = home("/home/u").with("XDG_RUNTIME_DIR", "/run/user/1000");
        assert_eq!(hud_body_dir(&runtime), Path::new("/run/user/1000/ngterm/hud"));
        let overridden = runtime.clone().with(STATE_DIR_ENV, "/tmp/inst");
        assert_eq!(hud_body_dir(&overridden), Path::new("/tmp/inst/hud"));
        for value in ["", "relative", "/"] {
            let env = home("/home/u").with("XDG_RUNTIME_DIR", value);
            assert_eq!(
                hud_body_dir(&env),
                Path::new("/home/u/.local/share/ngterm/hud"),
                "{value:?}"
            );
        }
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the XDG base directories as Unix paths; the Windows locations belong to the GPUI-on-Windows plan"
    )]
    fn the_global_ghostty_config_follows_the_xdg_config_home() {
        assert_eq!(
            global_ghostty_config_path(&home("/home/u")).as_deref(),
            Some(Path::new("/home/u/.config/ghostty/config"))
        );
        let env = home("/home/u").with("XDG_CONFIG_HOME", "/cfg");
        assert_eq!(
            global_ghostty_config_path(&env).as_deref(),
            Some(Path::new("/cfg/ghostty/config"))
        );
        let relative = home("/home/u").with("XDG_CONFIG_HOME", "rel");
        assert_eq!(
            global_ghostty_config_path(&relative).as_deref(),
            Some(Path::new("/home/u/.config/ghostty/config"))
        );
    }

    #[test]
    fn xdg_data_home_unset_falls_back_to_home() {
        assert_eq!(state_dir(&home("/home/u")), Path::new("/home/u/.local/share/ngterm"));
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the XDG base directories as Unix paths; the Windows locations belong to the GPUI-on-Windows plan"
    )]
    fn xdg_data_home_set_is_used() {
        let env = home("/home/u").with("XDG_DATA_HOME", "/data");
        assert_eq!(state_dir(&env), Path::new("/data/ngterm"));
    }

    #[test]
    fn invalid_xdg_data_home_is_ignored() {
        for value in ["", "relative/data", "/"] {
            let env = home("/home/u").with("XDG_DATA_HOME", value);
            assert_eq!(state_dir(&env), Path::new("/home/u/.local/share/ngterm"), "{value:?}");
        }
    }

    #[test]
    fn missing_home_uses_the_password_database_then_tmp() {
        let env = MapEnv::new().with("HOME", "").with_passwd_home("/var/home/u");
        assert_eq!(state_dir(&env), Path::new("/var/home/u/.local/share/ngterm"));
        assert_eq!(state_dir(&MapEnv::new()), Path::new(FALLBACK_STATE_DIR));
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "the XDG base directories as Unix paths; the Windows locations belong to the GPUI-on-Windows plan"
    )]
    fn state_dir_override_wins_and_empty_is_unset() {
        let env = home("/home/u").with("XDG_DATA_HOME", "/data");
        assert_eq!(
            state_dir(&env.clone().with(STATE_DIR_ENV, "/tmp/s")),
            Path::new("/tmp/s")
        );
        assert_eq!(state_dir(&env.with(STATE_DIR_ENV, "")), Path::new("/data/ngterm"));
    }

    #[test]
    fn socket_path_falls_back_to_the_state_dir() {
        let env = home("/home/u");
        assert_eq!(
            app_socket_path(&env),
            Path::new("/home/u/.local/share/ngterm/ngterm.sock")
        );
        let isolated = env.with(STATE_DIR_ENV, "/tmp/s");
        assert_eq!(app_socket_path(&isolated), Path::new("/tmp/s/ngterm.sock"));
        assert_eq!(
            app_socket_path(&isolated.clone().with(CONTROL_SOCKET_ENV, "")),
            Path::new("/tmp/s/ngterm.sock")
        );
        let explicit = isolated.with(CONTROL_SOCKET_ENV, "/tmp/x/ctl.sock");
        assert_eq!(app_socket_path(&explicit), Path::new("/tmp/x/ctl.sock"));
    }

    #[test]
    fn ownership_paths_append_to_the_socket_path() {
        let socket = Path::new("/tmp/s/ngterm.sock");
        assert_eq!(ownership_lock_path(socket), Path::new("/tmp/s/ngterm.sock.lock"));
        assert_eq!(
            unavailable_socket_path(socket),
            Path::new("/tmp/s/ngterm.sock.unavailable")
        );
    }

    #[test]
    fn config_dir_follows_upstream_precedence() {
        let env = home("/home/u").with("XDG_DATA_HOME", "/data");
        assert_eq!(config_dir(&env, None), Path::new("/home/u/.config/ngterm"));
        let isolated = env.clone().with(STATE_DIR_ENV, "/tmp/s");
        assert_eq!(config_dir(&isolated, None), Path::new("/tmp/s/config"));
        assert_eq!(config_dir(&isolated, Some(Path::new(""))), Path::new("/tmp/s/config"));
        assert_eq!(
            config_dir(&isolated, Some(Path::new("/etc/agt"))),
            Path::new("/etc/agt")
        );
        assert_eq!(
            config_dir(&env.with(STATE_DIR_ENV, ""), None),
            Path::new("/home/u/.config/ngterm")
        );
        assert_eq!(config_dir(&MapEnv::new(), None), Path::new("/tmp/ngterm/config"));
    }

    #[test]
    fn ghostty_config_files_sit_in_the_config_and_state_dirs() {
        assert_eq!(
            ghostty_config_path(Path::new("/tmp/s/config")),
            Path::new("/tmp/s/config/ghostty.conf")
        );
        let env = home("/home/u").with(STATE_DIR_ENV, "/tmp/s");
        assert_eq!(
            ghostty_settings_config_path(&env),
            Path::new("/tmp/s/ghostty-settings.conf")
        );
        assert_eq!(
            ghostty_settings_config_path(&home("/home/u")),
            Path::new("/home/u/.local/share/ngterm/ghostty-settings.conf")
        );
    }

    #[test]
    fn process_env_reads_the_real_environment() {
        assert_eq!(ProcessEnv.var("PATH"), std::env::var_os("PATH"));
    }
}
