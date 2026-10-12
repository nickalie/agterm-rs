//! Where state lives: the state directory and the control socket, in upstream's precedence
//! (`ControlServer.defaultSocketPath` in `agterm/Control/ControlServer.swift`: `AGTERM_CONTROL_SOCKET`, else
//! `ControlResolve.socketPath` over `AGTERM_STATE_DIR` or `PersistenceStore.defaultDirectory`).
//!
//! The Linux base is glue taken from the Linux fork: `PersistenceStore.defaultDirectory` is Foundation's application
//! support directory, which on Linux is the XDG data home, and an empty variable counts as unset, as the fork's
//! `linuxStateDirectory()` (`agterm-linux/Sources/AgtermLinux/App.swift`) treats `AGTERM_STATE_DIR`; upstream would
//! take an empty value as given.
//!
//! Every function reads the environment through [`Env`], so tests pass a [`MapEnv`] instead of mutating the
//! process environment.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Overrides the whole state directory (test isolation, `run-dev.sh`).
pub const STATE_DIR_ENV: &str = "AGTERM_STATE_DIR";
/// Overrides the app's control socket path. Only the app reads it: `agtermctl` resolves through the state dir.
pub const CONTROL_SOCKET_ENV: &str = "AGTERM_CONTROL_SOCKET";
/// The control socket's file name inside the state directory.
pub const SOCKET_FILE_NAME: &str = "agterm.sock";
/// The state directory's name under the XDG data home.
pub const APP_DIR_NAME: &str = "agterm";
/// The base used when no home directory can be found, as `agtermctl` upstream (`/tmp/agterm`).
pub const FALLBACK_STATE_DIR: &str = "/tmp/agterm";

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

/// The state directory with no override: `<XDG data home>/agterm`, or [`FALLBACK_STATE_DIR`] without a home.
#[must_use]
pub fn default_state_dir(env: &dyn Env) -> PathBuf {
    xdg_data_home(env).map_or_else(|| PathBuf::from(FALLBACK_STATE_DIR), |base| base.join(APP_DIR_NAME))
}

/// The state directory: a non-empty `AGTERM_STATE_DIR`, else [`default_state_dir`].
#[must_use]
pub fn state_dir(env: &dyn Env) -> PathBuf {
    non_empty(env, STATE_DIR_ENV).map_or_else(|| default_state_dir(env), PathBuf::from)
}

/// The control socket inside `state_dir`; the rendezvous the app and `agtermctl` share.
#[must_use]
pub fn socket_in(state_dir: &Path) -> PathBuf {
    state_dir.join(SOCKET_FILE_NAME)
}

/// The socket the app binds: a non-empty `AGTERM_CONTROL_SOCKET`, else `<state dir>/agterm.sock`.
#[must_use]
pub fn app_socket_path(env: &dyn Env) -> PathBuf {
    non_empty(env, CONTROL_SOCKET_ENV).map_or_else(|| socket_in(&state_dir(env)), PathBuf::from)
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
    fn xdg_data_home_unset_falls_back_to_home() {
        assert_eq!(state_dir(&home("/home/u")), Path::new("/home/u/.local/share/agterm"));
    }

    #[test]
    fn xdg_data_home_set_is_used() {
        let env = home("/home/u").with("XDG_DATA_HOME", "/data");
        assert_eq!(state_dir(&env), Path::new("/data/agterm"));
    }

    #[test]
    fn invalid_xdg_data_home_is_ignored() {
        for value in ["", "relative/data", "/"] {
            let env = home("/home/u").with("XDG_DATA_HOME", value);
            assert_eq!(state_dir(&env), Path::new("/home/u/.local/share/agterm"), "{value:?}");
        }
    }

    #[test]
    fn missing_home_uses_the_password_database_then_tmp() {
        let env = MapEnv::new().with("HOME", "").with_passwd_home("/var/home/u");
        assert_eq!(state_dir(&env), Path::new("/var/home/u/.local/share/agterm"));
        assert_eq!(state_dir(&MapEnv::new()), Path::new(FALLBACK_STATE_DIR));
    }

    #[test]
    fn state_dir_override_wins_and_empty_is_unset() {
        let env = home("/home/u").with("XDG_DATA_HOME", "/data");
        assert_eq!(
            state_dir(&env.clone().with(STATE_DIR_ENV, "/tmp/s")),
            Path::new("/tmp/s")
        );
        assert_eq!(state_dir(&env.with(STATE_DIR_ENV, "")), Path::new("/data/agterm"));
    }

    #[test]
    fn socket_path_falls_back_to_the_state_dir() {
        let env = home("/home/u");
        assert_eq!(
            app_socket_path(&env),
            Path::new("/home/u/.local/share/agterm/agterm.sock")
        );
        let isolated = env.with(STATE_DIR_ENV, "/tmp/s");
        assert_eq!(app_socket_path(&isolated), Path::new("/tmp/s/agterm.sock"));
        assert_eq!(
            app_socket_path(&isolated.clone().with(CONTROL_SOCKET_ENV, "")),
            Path::new("/tmp/s/agterm.sock")
        );
        let explicit = isolated.with(CONTROL_SOCKET_ENV, "/tmp/x/ctl.sock");
        assert_eq!(app_socket_path(&explicit), Path::new("/tmp/x/ctl.sock"));
    }

    #[test]
    fn ownership_paths_append_to_the_socket_path() {
        let socket = Path::new("/tmp/s/agterm.sock");
        assert_eq!(ownership_lock_path(socket), Path::new("/tmp/s/agterm.sock.lock"));
        assert_eq!(
            unavailable_socket_path(socket),
            Path::new("/tmp/s/agterm.sock.unavailable")
        );
    }

    #[test]
    fn process_env_reads_the_real_environment() {
        assert_eq!(ProcessEnv.var("PATH"), std::env::var_os("PATH"));
    }
}
