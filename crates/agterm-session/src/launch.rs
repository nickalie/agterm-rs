//! What a live launch reads of this host (upstream's `agterm/Ghostty/ZmxLaunch.swift`): the staged session host
//! executable (or a debug build's override), the login shell, the Ghostty resources holding its shell integration, the
//! app's own values of the variables that integration hands on, the state directory naming the socket namespace and
//! the staged host's build id. Linux and Windows have no supervising session host (macOS's `agterm-session-host`),
//! so no configuration names one.
//!
//! The staged session host is still zmx, so the executable, its override and its build id keep zmx's names
//! ([`EXECUTABLE_FILE`], [`EXECUTABLE_OVERRIDE`], [`BUILD_ID_PATH`]) until the session server takes its place.
//!
//! The login shell is the password database's on Unix ([`login_shell`]); Windows has none, so there it is PowerShell:
//! the first launchable `pwsh.exe` on `PATH`, else `powershell.exe` there or in the system directory (`windows_shell`,
//! after herdr), none when there is neither: the live launch then refuses with the unsupported-shell reason.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use agterm_core::live::lead::LeadAttachment;
use agterm_core::live::shell_integration::Inherited;
use agterm_core::live::support::{self, Configuration, Inputs};
use agterm_core::paths::{self, Env};
use agterm_core::restore::mode::{LiveProbe, LiveRejection, UI_TEST_OPT_IN};
use uuid::Uuid;

#[cfg(any(windows, test))]
mod windows_shell;

/// The variable a debug build reads for another session host executable (upstream honors it in DEBUG only).
pub const EXECUTABLE_OVERRIDE: &str = "AGTERM_ZMX_PATH";
/// The staged session host's file name in the staged `bin` directory.
pub const EXECUTABLE_FILE: &str = if cfg!(windows) { "zmx.exe" } else { "zmx" };
/// The staged session host's build id, relative to the staged root (`scripts/stage.sh`).
pub const BUILD_ID_PATH: &str = "share/ngterm/zmx-BUILD";
/// The staged Ghostty resources, relative to the staged root: the fallback without `GHOSTTY_RESOURCES_DIR`.
const BUNDLED_RESOURCES: &str = "share/ghostty";

/// The host's live-launch inputs, read once.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Runtime {
    /// The session host executable: the override in a debug build, else [`EXECUTABLE_FILE`] beside the app.
    pub executable: PathBuf,
    /// The shell a pane runs: the password database's login shell on Unix, PowerShell on Windows.
    pub login_shell: Option<String>,
    pub resources_dir: Option<PathBuf>,
    pub state_dir: PathBuf,
    /// The app's own `ZDOTDIR`, `ENV`, `HISTFILE`, home and `XDG_DATA_DIRS`, handed on to the shell integration.
    pub inherited: Inherited,
}

impl Runtime {
    /// The inputs of this process: the session host beside the running executable, this host's login shell.
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
        let executable = var(EXECUTABLE_OVERRIDE)
            .filter(|_| allow_debug_override)
            .or_else(|| exe_dir.map(|dir| dir.join(EXECUTABLE_FILE)))
            .unwrap_or_default();
        Self {
            executable,
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
            executable: self.executable.clone(),
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
            executable: self.executable.clone(),
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
}

/// The staged session host's build id beside an executable in `exe_dir`, trimmed; `None` when absent or blank, which
/// turns the outdated reset reason off.
#[must_use]
pub fn bundled_build_id(exe_dir: &Path) -> Option<String> {
    let root = exe_dir.parent()?;
    let id = fs::read_to_string(root.join(BUILD_ID_PATH)).ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// The login shell the password database names for this user; `None` when unreadable or empty.
#[cfg(unix)]
#[must_use]
pub fn login_shell() -> Option<String> {
    use std::ffi::CStr;

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

/// The shell a Windows pane runs, there being no password database: the first launchable `pwsh.exe` on `PATH`, else
/// `powershell.exe` there or in the system directory, as a full path; `None` without any (`windows_shell`).
#[cfg(windows)]
#[must_use]
pub fn login_shell() -> Option<String> {
    windows_shell::login_shell()
}

#[cfg(test)]
mod tests {
    use agterm_core::live::{daemon_name, socket_directory};
    use agterm_core::paths::MapEnv;

    use super::*;

    /// A staged layout: the session host in `bin`, the zsh, bash and fish integrations under `share/ghostty` and the
    /// build id.
    fn staged() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("bin")).unwrap();
        let executable = root.path().join("bin").join(EXECUTABLE_FILE);
        fs::write(&executable, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let integration = root.path().join("share/ghostty/shell-integration/zsh");
        fs::create_dir_all(&integration).unwrap();
        fs::write(integration.join(".zshenv"), "").unwrap();
        let bash = root.path().join("share/ghostty/shell-integration/bash");
        fs::create_dir_all(&bash).unwrap();
        fs::write(bash.join("ghostty.bash"), "").unwrap();
        let fish = root.path().join("share/ghostty/shell-integration/fish/vendor_conf.d");
        fs::create_dir_all(&fish).unwrap();
        fs::write(fish.join("ghostty-shell-integration.fish"), "").unwrap();
        // the PowerShell integration as `scripts/stage.sh` stages it
        let pwsh = root.path().join("share/ghostty/shell-integration/pwsh");
        fs::create_dir_all(&pwsh).unwrap();
        fs::copy(pwsh_integration_source(), pwsh.join("ngterm.ps1")).unwrap();
        fs::create_dir_all(root.path().join("share/ngterm")).unwrap();
        fs::write(root.path().join(BUILD_ID_PATH), "rev x86_64-linux-gnu.2.39 digest\n").unwrap();
        root
    }

    #[test]
    fn the_staged_host_and_resources_wrap_a_pane_for_a_zsh_login_shell() {
        let root = staged();
        let bin = root.path().join("bin");
        let state = root.path().join("state");
        let env = MapEnv::new()
            .with("AGTERM_STATE_DIR", &state)
            .with("ZDOTDIR", "/home/u/.zsh");
        let runtime = Runtime::resolve(&env, Some(&bin), Some("/usr/bin/zsh".to_owned()), false);
        assert_eq!(runtime.executable, bin.join(EXECUTABLE_FILE));
        assert_eq!(runtime.resources_dir, Some(root.path().join("share/ghostty")));
        assert_eq!(runtime.state_dir, state);
        assert_eq!(runtime.live_probe(&env).unavailable_reason(), None);
        let pane = Uuid::new_v4();
        let configuration = runtime.configuration(pane, BTreeMap::new(), None).unwrap();
        assert_eq!(configuration.daemon_name, daemon_name(pane));
        assert_eq!(configuration.session_host_executable, None, "no session host here");
        // the namespace is the state directory's
        assert_eq!(configuration.socket_directory, socket_directory(&state));
        assert_eq!(
            configuration.environment.get("GHOSTTY_ZSH_ZDOTDIR").map(String::as_str),
            Some("/home/u/.zsh")
        );
        assert_eq!(
            bundled_build_id(&bin).as_deref(),
            Some("rev x86_64-linux-gnu.2.39 digest")
        );
    }

    #[test]
    fn the_namespace_follows_the_state_directory() {
        let root = staged();
        let bin = root.path().join("bin");
        let pane = Uuid::new_v4();
        let namespace = |state: &Path| {
            let env = MapEnv::new().with("AGTERM_STATE_DIR", state);
            let runtime = Runtime::resolve(&env, Some(&bin), Some("/bin/zsh".to_owned()), false);
            runtime
                .configuration(pane, BTreeMap::new(), None)
                .unwrap()
                .socket_directory
        };
        let (one, two) = (root.path().join("one"), root.path().join("two"));
        assert_eq!(namespace(&one), socket_directory(&one));
        assert_ne!(namespace(&one), namespace(&two));
    }

    // bash and fish are Unix login shells: their integration's `ENV`, home and `XDG_DATA_DIRS` forms are POSIX ones
    #[cfg(unix)]
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

    /// The repository's PowerShell integration, which `scripts/stage.sh` stages.
    fn pwsh_integration_source() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("resources")
            .join("shell-integration")
            .join("pwsh")
            .join("ngterm.ps1")
    }

    #[test]
    fn a_powershell_login_shell_wraps_with_the_staged_integration() {
        let root = staged();
        let bin = root.path().join("bin");
        let script = root
            .path()
            .join("share/ghostty/shell-integration")
            .join("pwsh")
            .join("ngterm.ps1");
        let env = MapEnv::new().with("AGTERM_STATE_DIR", root.path().join("state"));
        for shell in [
            "/usr/bin/pwsh",
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            "powershell.exe",
        ] {
            let runtime = Runtime::resolve(&env, Some(&bin), Some(shell.to_owned()), false);
            assert_eq!(runtime.live_probe(&env).unavailable_reason(), None, "{shell}");
            let environment = runtime
                .configuration(Uuid::new_v4(), BTreeMap::new(), None)
                .unwrap()
                .environment;
            assert_eq!(
                environment
                    .get(agterm_core::live::shell_integration::PWSH_INTEGRATION_VARIABLE)
                    .map(PathBuf::from),
                Some(script.clone()),
                "{shell}"
            );
            assert_eq!(environment.get("SHELL").map(String::as_str), Some(shell));
        }
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
        assert_eq!(Runtime::resolve(&MapEnv::new(), None, None, false).resources_dir, None);
    }

    #[test]
    fn the_executable_override_counts_only_in_a_debug_build() {
        let env = MapEnv::new().with(EXECUTABLE_OVERRIDE, "/custom/zmx");
        let bin = Path::new("/staged/bin");
        assert_eq!(
            Runtime::resolve(&env, Some(bin), None, true).executable,
            PathBuf::from("/custom/zmx")
        );
        assert_eq!(
            Runtime::resolve(&env, Some(bin), None, false).executable,
            bin.join(EXECUTABLE_FILE)
        );
        let empty = MapEnv::new().with(EXECUTABLE_OVERRIDE, "");
        assert_eq!(
            Runtime::resolve(&empty, Some(bin), None, true).executable,
            bin.join(EXECUTABLE_FILE)
        );
    }

    #[test]
    fn the_probe_names_the_first_rejection_and_honors_the_ui_test_bypass() {
        let root = staged();
        let bin = root.path().join("bin");
        let tcsh = Runtime::resolve(&MapEnv::new(), Some(&bin), Some("/bin/tcsh".to_owned()), false);
        assert_eq!(
            tcsh.live_probe(&MapEnv::new()).unavailable_reason().as_deref(),
            Some(agterm_core::restore::mode::UNSUPPORTED_LOGIN_SHELL)
        );
        for shell in ["/bin/bash", "/usr/bin/fish"] {
            let runtime = Runtime::resolve(&MapEnv::new(), Some(&bin), Some(shell.to_owned()), false);
            assert_eq!(runtime.live_probe(&MapEnv::new()).unavailable_reason(), None, "{shell}");
        }
        let missing = root.path().join("missing/bin");
        let unstaged = Runtime::resolve(&MapEnv::new(), Some(&missing), None, false);
        assert_eq!(
            unstaged.live_probe(&MapEnv::new()).unavailable_reason().as_deref(),
            Some("the live-session executable is unavailable")
        );
        let nowhere = Runtime::resolve(&MapEnv::new(), None, None, false);
        assert_eq!(
            nowhere.live_probe(&MapEnv::new()).unavailable_reason().as_deref(),
            Some("the live-session executable path is not absolute")
        );
        assert!(bundled_build_id(&missing).is_none());
        let zsh = Runtime::resolve(&MapEnv::new(), Some(&bin), Some("/bin/zsh".to_owned()), false);
        let ui_test = MapEnv::new()
            .with("AGTERM_STATE_DIR", root.path().join("state"))
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
    fn a_blank_build_id_turns_the_outdated_reason_off() {
        let root = staged();
        let bin = root.path().join("bin");
        fs::write(root.path().join(BUILD_ID_PATH), " \n").unwrap();
        assert_eq!(bundled_build_id(&bin), None);
        assert_eq!(bundled_build_id(Path::new("/")), None, "no staged root above");
    }

    #[cfg(unix)]
    #[test]
    fn the_password_database_names_a_login_shell() {
        assert!(login_shell().is_some_and(|shell| shell.starts_with('/')));
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_pane_runs_powershell_by_its_full_path() {
        let shell = login_shell().unwrap();
        let path = Path::new(&shell);
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        assert!(
            path.is_absolute()
                && (name.eq_ignore_ascii_case(windows_shell::PWSH)
                    || name.eq_ignore_ascii_case(windows_shell::INBOX_POWERSHELL)),
            "{shell}"
        );
        assert_eq!(
            agterm_core::live::shell_integration::LoginShell::of(&shell),
            Some(agterm_core::live::shell_integration::LoginShell::Pwsh)
        );
    }
}
