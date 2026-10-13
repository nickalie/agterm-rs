//! What the core needs to start hooks and custom commands: the frontend's [`ProcessLauncher`], a way back onto the
//! main loop for their exits, the socket they are told about, and the `PATH` they run with (upstream's
//! `CommandPath.swift`, the socket provider the runners take).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agterm_host::ProcessLauncher;

use crate::Core;
use crate::paths::ConfigLocator;

/// A step to run on the main loop with the core.
pub type CoreTask = Box<dyn FnOnce(&mut Core) + Send + 'static>;

/// Queues a [`CoreTask`] on the main loop, which settles after it as after any mutation.
pub type CorePoster = Arc<dyn Fn(CoreTask) + Send + Sync>;

/// The frontend's process seam, installed once with [`Core::set_process_host`].
#[derive(Clone)]
pub struct ProcessHost {
    pub launcher: Arc<dyn ProcessLauncher>,
    pub poster: CorePoster,
    /// What children get as `AGT_SOCKET`: the owned socket, or `<socket>.unavailable`.
    pub socket: String,
    /// The directory of the `agtermctl` beside the app binary, put first on `PATH`.
    pub cli_dir: Option<PathBuf>,
    /// The app's own `PATH`, widened for children.
    pub path: Option<String>,
}

impl fmt::Debug for ProcessHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessHost")
            .field("socket", &self.socket)
            .field("cli_dir", &self.cli_dir)
            .finish_non_exhaustive()
    }
}

/// The process seam and where the config files live.
#[derive(Debug, Default)]
pub(crate) struct ProcessRuntime {
    pub(crate) host: Option<ProcessHost>,
    pub(crate) config: ConfigLocator,
}

/// The `PATH` used when the app's environment carries none.
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The `PATH` a hook or custom command runs with (upstream's `CommandPath.widened`): the bundled CLI's directory first,
/// so a bare `agtermctl` reaches this build, then the app's own entries, then `~/.local/bin` (where a user-installed
/// CLI lands on Linux) when absent; duplicates and empty entries dropped.
#[must_use]
pub fn widened_path(path: Option<&str>, cli_dir: Option<&Path>, home: Option<&Path>) -> String {
    let base = path.filter(|path| !path.is_empty()).unwrap_or(DEFAULT_PATH);
    let mut entries: Vec<String> = Vec::new();
    let mut add = |dir: String| {
        if !dir.is_empty() && !entries.contains(&dir) {
            entries.push(dir);
        }
    };
    if let Some(dir) = cli_dir {
        add(dir.to_string_lossy().into_owned());
    }
    for entry in base.split(':') {
        add(entry.to_owned());
    }
    if let Some(home) = home {
        add(home.join(".local/bin").to_string_lossy().into_owned());
    }
    entries.join(":")
}

impl Core {
    /// Installs the frontend's process seam; until then hooks and custom commands fail to start.
    pub fn set_process_host(&mut self, host: ProcessHost) {
        self.processes.host = Some(host);
    }

    /// Captures where `keymap.conf` and `hooks.conf` live (with the `configDirectory` setting on top).
    pub fn set_config_locator(&mut self, locator: ConfigLocator) {
        self.processes.config = locator;
    }

    /// The config directory: the `configDirectory` setting, else the environment's default.
    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        let setting = self.settings().config_directory.as_deref().map(Path::new);
        self.processes.config.config_dir(setting)
    }

    /// The `PATH` children run with.
    pub(crate) fn child_path(&self) -> String {
        let host = self.processes.host.as_ref();
        widened_path(
            host.and_then(|host| host.path.as_deref()),
            host.and_then(|host| host.cli_dir.as_deref()),
            self.processes.config.home(),
        )
    }

    /// What children get as `AGT_SOCKET`.
    pub(crate) fn child_socket(&self) -> String {
        self.processes
            .host
            .as_ref()
            .map(|host| host.socket.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widening_puts_the_bundled_cli_first_and_appends_the_user_bin() {
        assert_eq!(
            widened_path(
                Some("/usr/bin:/bin:/usr/bin"),
                Some(Path::new("/opt/agterm/bin")),
                Some(Path::new("/home/u"))
            ),
            "/opt/agterm/bin:/usr/bin:/bin:/home/u/.local/bin"
        );
        assert_eq!(widened_path(Some(""), None, None), DEFAULT_PATH);
        assert_eq!(
            widened_path(Some("/home/u/.local/bin:/bin"), None, Some(Path::new("/home/u"))),
            "/home/u/.local/bin:/bin"
        );
    }
}
