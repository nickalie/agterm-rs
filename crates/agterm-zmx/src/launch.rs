//! What a live launch reads of this host (upstream's `agterm/Ghostty/ZmxLaunch.swift`): the staged zmx (or a debug
//! build's `AGTERM_ZMX_PATH`), the password-database login shell, the Ghostty resources holding its shell integration,
//! the app's own values of the variables that integration hands on, the state directory naming the socket namespace
//! and the bundled zmx build id. Linux has no supervising session host
//! (macOS's `agterm-session-host`), so no configuration names one.

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs;
use std::path::{Path, PathBuf};

use agterm_core::paths::{self, Env};
use agterm_core::restore::mode::{LiveProbe, LiveRejection, UI_TEST_OPT_IN};
use agterm_core::zmx::lead::LeadAttachment;
use agterm_core::zmx::shell_integration::Inherited;
use agterm_core::zmx::socket_directory;
use agterm_core::zmx::support::{self, Configuration, Inputs};
use uuid::Uuid;

use crate::client::ZmxClient;

/// The variable a debug build reads for another zmx executable (upstream honors it in DEBUG only).
pub const ZMX_PATH_OVERRIDE: &str = "AGTERM_ZMX_PATH";
/// The staged zmx build id, relative to the staged root (`scripts/stage.sh`).
pub const BUILD_ID_PATH: &str = "share/agterm/zmx-BUILD";
/// The staged Ghostty resources, relative to the staged root: the fallback without `GHOSTTY_RESOURCES_DIR`.
const BUNDLED_RESOURCES: &str = "share/ghostty";

/// The host's live-launch inputs, read once.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Runtime {
    pub zmx_executable: PathBuf,
    pub login_shell: Option<String>,
    pub resources_dir: Option<PathBuf>,
    pub state_dir: PathBuf,
    /// The app's own `ZDOTDIR`, `ENV`, `HISTFILE`, home and `XDG_DATA_DIRS`, handed on to the shell integration.
    pub inherited: Inherited,
}

impl Runtime {
    /// The inputs of this process: the zmx beside the running executable, the password database's login shell.
    #[must_use]
    pub fn capture(env: &dyn Env) -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf));
        Self::resolve(env, exe_dir.as_deref(), login_shell(), cfg!(debug_assertions))
    }

    /// The inputs for an executable in `exe_dir` (`<root>/bin` of the staged layout) under `env`; the debug override
    /// counts only when `allow_debug_override`.
    #[must_use]
    pub fn resolve(
        env: &dyn Env,
        exe_dir: Option<&Path>,
        login_shell: Option<String>,
        allow_debug_override: bool,
    ) -> Self {
        let var = |key: &str| env.var(key).filter(|value| !value.is_empty()).map(PathBuf::from);
        let staged_root = exe_dir.and_then(Path::parent);
        let zmx_executable = var(ZMX_PATH_OVERRIDE)
            .filter(|_| allow_debug_override)
            .or_else(|| exe_dir.map(|dir| dir.join("zmx")))
            .unwrap_or_default();
        Self {
            zmx_executable,
            login_shell,
            resources_dir: var("GHOSTTY_RESOURCES_DIR")
                .or_else(|| staged_root.map(|root| root.join(BUNDLED_RESOURCES))),
            state_dir: paths::state_dir(env),
            inherited: Inherited::read(env),
        }
    }

    /// The launch probe over these inputs, with the UI-test bypass read from `env`.
    #[must_use]
    pub fn live_probe(&self, env: &dyn Env) -> LiveProbe {
        LiveProbe {
            ui_test_launch: agterm_core::is_ui_test_launch(env),
            ui_test_opt_in: env.var(UI_TEST_OPT_IN).is_some_and(|value| value == "1"),
            zmx_executable: self.zmx_executable.clone(),
            login_shell: self.login_shell.clone(),
            resources_dir: self.resources_dir.clone(),
        }
    }

    /// The wrap configuration of pane `pane_identity`, extending its `base_environment`.
    ///
    /// # Errors
    ///
    /// This host cannot wrap a pane.
    pub fn configuration(
        &self,
        pane_identity: Uuid,
        base_environment: BTreeMap<String, String>,
        lead: Option<LeadAttachment>,
    ) -> Result<Configuration, LiveRejection> {
        support::configuration(&Inputs {
            zmx_executable: self.zmx_executable.clone(),
            login_shell: self.login_shell.clone(),
            resources_dir: self.resources_dir.clone(),
            state_dir: self.state_dir.clone(),
            pane_identity,
            base_environment,
            inherited: self.inherited.clone(),
            session_host_executable: None,
            lead,
        })
    }

    /// A client of this instance's daemons, hanging up what a confirmed kill leaves running.
    #[must_use]
    pub fn client(&self) -> ZmxClient {
        ZmxClient::new(self.zmx_executable.clone(), socket_directory(&self.state_dir))
            .with_sweeper(crate::sweep::Sweeper::live())
    }
}

/// The staged zmx's build id beside an executable in `exe_dir`, trimmed; `None` when absent or blank, which turns the
/// outdated reset reason off.
#[must_use]
pub fn bundled_build_id(exe_dir: &Path) -> Option<String> {
    let root = exe_dir.parent()?;
    let id = fs::read_to_string(root.join(BUILD_ID_PATH)).ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// The login shell the password database names for this user; `None` when unreadable or empty.
#[must_use]
pub fn login_shell() -> Option<String> {
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
    let shell = unsafe { CStr::from_ptr(entry.pw_shell) }.to_string_lossy().into_owned();
    (!shell.is_empty()).then_some(shell)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use agterm_core::paths::MapEnv;
    use agterm_core::zmx::daemon_name;

    use super::*;

    /// A staged layout: `bin/zmx`, the zsh, bash and fish integrations under `share/ghostty` and the build id.
    fn staged() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("bin")).unwrap();
        let zmx = root.path().join("bin/zmx");
        fs::write(&zmx, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&zmx, fs::Permissions::from_mode(0o755)).unwrap();
        let integration = root.path().join("share/ghostty/shell-integration/zsh");
        fs::create_dir_all(&integration).unwrap();
        fs::write(integration.join(".zshenv"), "").unwrap();
        let bash = root.path().join("share/ghostty/shell-integration/bash");
        fs::create_dir_all(&bash).unwrap();
        fs::write(bash.join("ghostty.bash"), "").unwrap();
        let fish = root.path().join("share/ghostty/shell-integration/fish/vendor_conf.d");
        fs::create_dir_all(&fish).unwrap();
        fs::write(fish.join("ghostty-shell-integration.fish"), "").unwrap();
        fs::create_dir_all(root.path().join("share/agterm")).unwrap();
        fs::write(root.path().join(BUILD_ID_PATH), "rev x86_64-linux-gnu.2.39 digest\n").unwrap();
        root
    }

    #[test]
    fn the_staged_zmx_and_resources_wrap_a_pane_for_a_zsh_login_shell() {
        let root = staged();
        let bin = root.path().join("bin");
        let env = MapEnv::new()
            .with("AGTERM_STATE_DIR", "/tmp/agterm-state")
            .with("ZDOTDIR", "/home/u/.zsh");
        let runtime = Runtime::resolve(&env, Some(&bin), Some("/usr/bin/zsh".to_owned()), false);
        assert_eq!(runtime.zmx_executable, bin.join("zmx"));
        assert_eq!(runtime.resources_dir, Some(root.path().join("share/ghostty")));
        assert_eq!(runtime.live_probe(&env).unavailable_reason(), None);
        let pane = Uuid::new_v4();
        let configuration = runtime.configuration(pane, BTreeMap::new(), None).unwrap();
        assert_eq!(configuration.daemon_name, daemon_name(pane));
        assert_eq!(configuration.session_host_executable, None, "Linux has no session host");
        assert_eq!(
            configuration.socket_directory,
            socket_directory(Path::new("/tmp/agterm-state"))
        );
        assert_eq!(
            configuration.environment.get("GHOSTTY_ZSH_ZDOTDIR").map(String::as_str),
            Some("/home/u/.zsh")
        );
        assert_eq!(runtime.client().socket_directory(), configuration.socket_directory);
        assert_eq!(
            bundled_build_id(&bin).as_deref(),
            Some("rev x86_64-linux-gnu.2.39 digest")
        );
    }

    #[test]
    fn bash_and_fish_login_shells_wrap_with_their_own_integration_and_the_apps_environment() {
        let root = staged();
        let bin = root.path().join("bin");
        let resources = root.path().join("share/ghostty/shell-integration");
        let env = MapEnv::new()
            .with("AGTERM_STATE_DIR", "/tmp/agterm-state")
            .with("HOME", "/home/u")
            .with("ENV", "/home/u/.shrc")
            .with("XDG_DATA_DIRS", "/opt/share:/usr/share");
        let pane = Uuid::new_v4();
        let bash = Runtime::resolve(&env, Some(&bin), Some("/bin/bash".to_owned()), false);
        let bash = bash.configuration(pane, BTreeMap::new(), None).unwrap().environment;
        assert_eq!(
            bash.get("ENV").map(PathBuf::from),
            Some(resources.join("bash/ghostty.bash"))
        );
        assert_eq!(bash.get("GHOSTTY_BASH_ENV").map(String::as_str), Some("/home/u/.shrc"));
        assert_eq!(bash.get("HISTFILE").map(String::as_str), Some("/home/u/.bash_history"));
        let fish = Runtime::resolve(&env, Some(&bin), Some("/usr/bin/fish".to_owned()), false);
        let fish = fish.configuration(pane, BTreeMap::new(), None).unwrap().environment;
        assert_eq!(
            fish.get("XDG_DATA_DIRS").cloned(),
            Some(format!("{}:/opt/share:/usr/share", resources.display()))
        );
        assert!(!fish.contains_key("ENV") && !fish.contains_key("ZDOTDIR"));
    }

    #[test]
    fn the_resources_dir_comes_from_the_environment_first() {
        let root = staged();
        let env = MapEnv::new().with("GHOSTTY_RESOURCES_DIR", "/opt/ghostty");
        let runtime = Runtime::resolve(&env, Some(&root.path().join("bin")), None, false);
        assert_eq!(runtime.resources_dir, Some(PathBuf::from("/opt/ghostty")));
        let empty = MapEnv::new().with("GHOSTTY_RESOURCES_DIR", "");
        let runtime = Runtime::resolve(&empty, Some(&root.path().join("bin")), None, false);
        assert_eq!(runtime.resources_dir, Some(root.path().join("share/ghostty")));
    }

    #[test]
    fn the_executable_override_counts_only_in_a_debug_build() {
        let env = MapEnv::new().with(ZMX_PATH_OVERRIDE, "/custom/zmx");
        let bin = Path::new("/staged/bin");
        assert_eq!(
            Runtime::resolve(&env, Some(bin), None, true).zmx_executable,
            PathBuf::from("/custom/zmx")
        );
        assert_eq!(
            Runtime::resolve(&env, Some(bin), None, false).zmx_executable,
            bin.join("zmx")
        );
        let empty = MapEnv::new().with(ZMX_PATH_OVERRIDE, "");
        assert_eq!(
            Runtime::resolve(&empty, Some(bin), None, true).zmx_executable,
            bin.join("zmx")
        );
    }

    #[test]
    fn the_probe_names_the_first_rejection_and_honors_the_ui_test_bypass() {
        let root = staged();
        let bin = root.path().join("bin");
        let tcsh = Runtime::resolve(&MapEnv::new(), Some(&bin), Some("/bin/tcsh".to_owned()), false);
        assert_eq!(
            tcsh.live_probe(&MapEnv::new()).unavailable_reason().as_deref(),
            Some("the password-database login shell is not zsh, bash or fish")
        );
        for shell in ["/bin/bash", "/usr/bin/fish"] {
            let runtime = Runtime::resolve(&MapEnv::new(), Some(&bin), Some(shell.to_owned()), false);
            assert_eq!(runtime.live_probe(&MapEnv::new()).unavailable_reason(), None, "{shell}");
        }
        let unstaged = Runtime::resolve(&MapEnv::new(), Some(Path::new("/nonexistent/bin")), None, false);
        assert_eq!(
            unstaged.live_probe(&MapEnv::new()).unavailable_reason().as_deref(),
            Some("the zmx executable is unavailable")
        );
        let nowhere = Runtime::resolve(&MapEnv::new(), None, None, false);
        assert_eq!(
            nowhere.live_probe(&MapEnv::new()).unavailable_reason().as_deref(),
            Some("the zmx executable path is not absolute")
        );
        assert!(bundled_build_id(Path::new("/nonexistent/bin")).is_none());
        let zsh = Runtime::resolve(&MapEnv::new(), Some(&bin), Some("/bin/zsh".to_owned()), false);
        let ui_test = MapEnv::new()
            .with("AGTERM_STATE_DIR", "/tmp/s")
            .with("AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE", "1");
        assert_eq!(
            zsh.live_probe(&ui_test).unavailable_reason().as_deref(),
            Some("Live sessions are disabled for default UI tests.")
        );
        assert_eq!(
            zsh.live_probe(&ui_test.with(UI_TEST_OPT_IN, "1")).unavailable_reason(),
            None
        );
    }

    #[test]
    fn the_password_database_names_a_login_shell() {
        assert!(login_shell().is_some_and(|shell| shell.starts_with('/')));
    }
}
