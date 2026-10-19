//! Launching a pane's program as libghostty launches a surface's for an embedder (`termio/Exec.zig`,
//! `apprt/embedded.zig` and `Surface.zig` at `LIBGHOSTTY_VT_REV`; the GTK build's `surface.rs` for the seed): the
//! terminal's identity, the shell integration and the exit handling every pane shares.
//!
//! - **Program**: a [`Launch`]'s command line, or else the host's login shell, runs through `/bin/sh -c` (libghostty
//!   ran a surface's command that way on Linux, and core's foreground reader looks past exactly that wrapper). A live
//!   pane's command is its `zmx attach` line ([`agterm_core::restore::spawn_plan::resolve_live_seed`]), whose
//!   environment carries the wrap configuration from [`Runtime`]; a plain pane runs its seed's command, or
//!   the login shell with its `initial_input` typed in.
//! - **Environment**, later steps winning: the app's own; `GHOSTTY_LOG` and `VTE_VERSION` removed; the terminal
//!   identity ([`Host`]); `GHOSTTY_SHELL_FEATURES`; the shell integration of a zsh, bash or fish command line
//!   ([`agterm_core::live::shell_integration::setup`], as `shell-integration` asks); the config's `env`; the launch's
//!   own variables; `PWD` for a working directory.
//! - **Config** (`Surface.zig` and `termio/Exec.zig`, the embedder's options winning as `apprt/embedded.zig` lets
//!   them): the launch's working directory, else (none, or not a directory) `working-directory`'s; the launch's initial input, else `input`'s;
//!   `wait-after-command` for every launch; `abnormal-command-exit-runtime`; `shell-integration`'s forced shell for the
//!   login shell (a command line keeps its own program's scheme: a live pane's `zmx attach` is no shell).
//! - **Identity** (Technical Details > Terminal identity): with Ghostty's resources and the compiled `xterm-ghostty`
//!   beside them, `TERM` is the config's `term`, `TERMINFO` that terminfo directory and `GHOSTTY_RESOURCES_DIR` the
//!   resources; without them `TERM=xterm-256color` and neither variable. `COLORTERM=truecolor`,
//!   `TERM_PROGRAM=ghostty` and the libghostty-vt version as `TERM_PROGRAM_VERSION` either way; agterm's executable
//!   directory as `GHOSTTY_BIN_DIR`, appended to `PATH`.
//! - **Exit**: [`ExitPolicy::Launched`] (see [`crate::exit`]).
//!
//! Not carried over from libghostty: the elvish and nushell integrations (`shell-integration = elvish` or `nushell`
//! injects nothing), Flatpak's host spawning and `GHOSTTY_SURFACE_ID`; the config's `command` is the frontend's
//! (what a pane without one runs).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agterm_core::ghostty_config::{
    GhosttyConfig, InputSource, ShellIntegration, ShellIntegrationFeatures, WorkingDirectory,
};
use agterm_core::live::shell_integration::{self, Inherited, LoginShell};
use agterm_core::paths::{Env, ProcessEnv};
use agterm_core::restore::spawn_plan::LaunchSeed;
use agterm_session::launch::Runtime;

use crate::exit::ExitPolicy;
use crate::{Pane, PaneCommand, PaneConfig, PaneError};

/// The shell a command line runs through, as libghostty ran a surface's command on Linux.
pub const COMMAND_SHELL: &str = "/bin/sh";
/// What the terminal claims without its own terminfo.
pub const FALLBACK_TERM: &str = "xterm-256color";
/// The terminfo entry beside Ghostty's resources that lets the terminal claim `xterm-ghostty`, relative to them, in
/// either layout `tic` writes: a directory per first letter (`x/`, ncurses on Linux) or per its hex code (`78/`,
/// where the file system ignores case: macOS, Git Bash on Windows).
const TERMINFO_ENTRIES: [&str; 2] = ["../terminfo/x/xterm-ghostty", "../terminfo/78/xterm-ghostty"];
/// The staged resources relative to the staged root (`scripts/stage.sh`).
const STAGED_RESOURCES: &str = "share/ghostty";
/// A system ghostty package's resources.
const SYSTEM_RESOURCES: &str = "/usr/share/ghostty";
/// Variables a pane never inherits from the app (libghostty's own log switch, and VTE's, which would make programs
/// take the terminal for a VTE one).
const NOT_INHERITED: [&str; 2] = ["GHOSTTY_LOG", "VTE_VERSION"];
/// The largest `input = path:` file libghostty sends (10 MB).
const INPUT_FILE_LIMIT: u64 = 10 * 1024 * 1024;

/// What every launch shares: the terminal's identity and the shell integration, what libghostty's app gave each
/// surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// The shell a pane without a command runs: the password database's login shell, `sh` without one.
    pub shell: String,
    /// Ghostty's resources (shell integration, themes) with the compiled `xterm-ghostty` beside them
    /// ([`resolve_resources`]); `None` claims `xterm-256color` and integrates no shell.
    pub resources_dir: Option<PathBuf>,
    /// The config's `term`, claimed while the terminfo resolves.
    pub term: String,
    /// The directory of agterm's executable: `GHOSTTY_BIN_DIR`, and the last entry of `PATH`.
    pub bin_dir: Option<PathBuf>,
    /// `shell-integration-features`, the ssh wrappers agterm cannot serve dropped.
    pub shell_features: ShellIntegrationFeatures,
    /// `cursor-style-blink`, unset counting as on.
    pub cursor_blink: bool,
    /// What `TERM_PROGRAM_VERSION` names.
    pub version: String,
    /// `abnormal-command-exit-runtime`.
    pub abnormal_exit_runtime: Duration,
    /// `wait-after-command`: every program's end holds its pane, the shell's included.
    pub wait_after_command: bool,
    /// `env`: variables over the terminal's own, under the launch's.
    pub env: Vec<(String, String)>,
    /// `input`: what a launch without an initial input of its own types first.
    pub input: Vec<InputSource>,
    /// `working-directory`: where a launch without a directory of its own starts.
    pub working_directory: Option<WorkingDirectory>,
    /// `shell-integration`.
    pub shell_integration: ShellIntegration,
}

impl Host {
    /// The host of `config`, running `shell` (the login shell; `None` falls back to `sh`) with the `resources_dir`
    /// [`resolve_resources`] found, from agterm's executable directory `bin_dir`.
    #[must_use]
    pub fn new(
        config: &GhosttyConfig,
        shell: Option<String>,
        resources_dir: Option<PathBuf>,
        bin_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            shell: shell
                .filter(|shell| !shell.is_empty())
                .unwrap_or_else(|| "sh".to_owned()),
            resources_dir,
            term: config.term.clone(),
            bin_dir,
            shell_features: config.shell_integration_features - ShellIntegrationFeatures::UNSUPPORTED,
            cursor_blink: config.cursor.style_blink.unwrap_or(true),
            version: crate::vt::version(),
            abnormal_exit_runtime: Duration::from_millis(u64::from(config.launch.abnormal_command_exit_runtime)),
            wait_after_command: config.launch.wait_after_command,
            env: config
                .launch
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            input: config.launch.input.clone(),
            working_directory: config.launch.working_directory.clone(),
            shell_integration: config.launch.shell_integration,
        }
    }

    /// The host of a live launch's inputs: their login shell, and their resources while the terminfo is beside them.
    #[must_use]
    pub fn from_runtime(config: &GhosttyConfig, runtime: &Runtime, bin_dir: Option<PathBuf>) -> Self {
        let resources = runtime
            .resources_dir
            .clone()
            .and_then(|dir| resolve_resources(&[dir], Path::exists));
        Self::new(config, runtime.login_shell.clone(), resources, bin_dir)
    }

    /// The program, environment, typed input and exit handling of `launch` under this host, over the app's
    /// environment `env`.
    #[must_use]
    pub fn prepare(&self, launch: &Launch, env: &dyn Env) -> Prepared {
        let mut vars = Vars::default();
        for name in NOT_INHERITED {
            vars.remove(name);
        }
        self.identity(env, &mut vars);
        match self.shell_features.env_value(self.cursor_blink) {
            Some(features) => vars.set("GHOSTTY_SHELL_FEATURES", features),
            None => vars.remove("GHOSTTY_SHELL_FEATURES"),
        }
        let mut line = launch.command.clone().unwrap_or_else(|| self.shell.clone());
        let force = match self.integration(launch) {
            Integration::Off => None,
            Integration::Detect => Some(None),
            Integration::As(shell) => Some(Some(shell)),
        };
        if let (Some(resources), Some(force)) = (&self.resources_dir, force) {
            let mut integration = BTreeMap::new();
            if let Some(integrated) =
                shell_integration::setup_as(force, &line, resources, &Inherited::read(env), &mut integration)
            {
                line = integrated;
                for (key, value) in integration {
                    vars.set(&key, value);
                }
            }
        }
        for (key, value) in &self.env {
            vars.set(key, value.clone());
        }
        for name in &launch.env_remove {
            vars.remove(name);
        }
        for (key, value) in &launch.env {
            vars.set(key, value.clone());
        }
        // `apprt/embedded.zig`: the launch's own directory when it is one, else `working-directory`'s
        let cwd = launch
            .working_directory
            .clone()
            .filter(|dir| dir.is_dir())
            .or_else(|| self.default_directory(env).filter(|dir| dir.is_dir()));
        if let Some(cwd) = &cwd {
            vars.set("PWD", cwd.to_string_lossy().into_owned());
        }
        let mut command = PaneCommand::new(COMMAND_SHELL).arg("-c").arg(&line);
        command.cwd = cwd;
        command.env_remove = vars.removed.into_iter().map(Into::into).collect();
        command.env = vars
            .set
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        Prepared {
            command,
            initial_input: launch
                .initial_input
                .clone()
                .filter(|input| !input.is_empty())
                .map(String::into_bytes)
                .or_else(|| self.config_input()),
            exit: ExitPolicy::Launched {
                argv: format!("{COMMAND_SHELL} -c {line}"),
                command: launch.command.is_some(),
                wait_after_command: launch.wait_after_command || self.wait_after_command,
                abnormal_runtime: self.abnormal_exit_runtime,
            },
        }
    }

    /// Whether and how the shell integration is injected into `launch`.
    fn integration(&self, launch: &Launch) -> Integration {
        let forced = match self.shell_integration {
            ShellIntegration::None | ShellIntegration::Elvish | ShellIntegration::Nushell => return Integration::Off,
            ShellIntegration::Detect => return Integration::Detect,
            ShellIntegration::Bash => LoginShell::Bash,
            ShellIntegration::Fish => LoginShell::Fish,
            ShellIntegration::Zsh => LoginShell::Zsh,
        };
        if launch.command.is_none() {
            Integration::As(forced)
        } else {
            Integration::Detect
        }
    }

    /// `working-directory`'s directory: `home` the home directory, a path with a leading `~/` under it, `inherit` (or
    /// none) the app's own (the child inherits it). A path that is not absolute is ignored, as Ghostty requires one.
    fn default_directory(&self, env: &dyn Env) -> Option<PathBuf> {
        let home = || env.var("HOME").map(PathBuf::from);
        match self.working_directory.as_ref()? {
            WorkingDirectory::Inherit => None,
            WorkingDirectory::Home => home(),
            WorkingDirectory::Path(path) => match path.strip_prefix("~/") {
                Some(rest) => home().map(|home| home.join(rest)),
                None => Some(PathBuf::from(path)).filter(|path| path.is_absolute()),
            },
        }
    }

    /// `input`'s bytes, its sources concatenated; `None` when there are none, or when one cannot be read (a missing
    /// file, one over 10 MB, one that is not a regular file), as libghostty then sends none of them.
    fn config_input(&self) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        for source in &self.input {
            match source {
                InputSource::Raw(raw) => bytes.extend_from_slice(raw),
                InputSource::Path(path) => match read_input_file(path) {
                    Ok(contents) => bytes.extend_from_slice(&contents),
                    Err(error) => {
                        tracing::warn!(
                            path = %path.display(),
                            %error,
                            "input: the file cannot be read; none of the input is sent"
                        );
                        return None;
                    }
                },
            }
        }
        (!bytes.is_empty()).then_some(bytes)
    }

    /// The terminal identity's variables.
    fn identity(&self, env: &dyn Env, vars: &mut Vars) {
        if let Some(resources) = &self.resources_dir {
            vars.set("GHOSTTY_RESOURCES_DIR", resources.to_string_lossy().into_owned());
            vars.set("TERM", self.term.clone());
            let terminfo = resources.parent().unwrap_or(resources).join("terminfo");
            vars.set("TERMINFO", terminfo.to_string_lossy().into_owned());
        } else {
            vars.remove("GHOSTTY_RESOURCES_DIR");
            vars.remove("TERMINFO");
            vars.set("TERM", FALLBACK_TERM.to_owned());
        }
        vars.set("COLORTERM", "truecolor".to_owned());
        if let Some(bin) = &self.bin_dir {
            let bin = bin.to_string_lossy().into_owned();
            let path = env.var("PATH").map(|path| path.to_string_lossy().into_owned());
            match path.filter(|path| !path.is_empty()) {
                Some(path) if path.split(':').any(|entry| entry == bin) => {}
                Some(path) => vars.set("PATH", format!("{path}:{bin}")),
                None => vars.set("PATH", bin.clone()),
            }
            vars.set("GHOSTTY_BIN_DIR", bin);
        }
        vars.set("TERM_PROGRAM", "ghostty".to_owned());
        vars.set("TERM_PROGRAM_VERSION", self.version.clone());
    }
}

/// How a launch's shell integration is injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Integration {
    /// Not at all (`shell-integration = none`, or a scheme agterm does not port).
    Off,
    /// By the program's name.
    Detect,
    /// As this shell's, whatever the program's name.
    As(LoginShell),
}

/// Variables set over the app's environment, and removed from it.
#[derive(Debug, Default)]
struct Vars {
    set: BTreeMap<String, String>,
    removed: Vec<String>,
}

impl Vars {
    fn set(&mut self, key: &str, value: String) {
        self.removed.retain(|removed| removed != key);
        self.set.insert(key.to_owned(), value);
    }

    fn remove(&mut self, key: &str) {
        self.set.remove(key);
        if !self.removed.iter().any(|removed| removed == key) {
            self.removed.push(key.to_owned());
        }
    }
}

/// An `input = path:` file's contents, read through one handle: `termio/Termio.zig` opens the file and reads at most
/// 10 MB of it (`readToEndAlloc`, an error past that), on its own IO thread. A pane is prepared on the UI thread, so
/// only a regular file is read (opened without blocking, so a FIFO cannot stall the open); a device or a FIFO, which
/// libghostty would read there, is refused.
fn read_input_file(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    read_bounded(file, INPUT_FILE_LIMIT)
}

/// At most `limit` bytes of `reader`, an error when it holds more (the read itself is bounded, so a file that grows
/// while it is read cannot run past the limit).
fn read_bounded(reader: impl std::io::Read, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;

    let mut contents = Vec::new();
    reader.take(limit.saturating_add(1)).read_to_end(&mut contents)?;
    if u64::try_from(contents.len()).unwrap_or(u64::MAX) > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            format!("more than {limit} bytes"),
        ));
    }
    Ok(contents)
}

/// What a pane runs (upstream's `LaunchSeed` on the pane's own surface spec).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Launch {
    /// A shell line run through `/bin/sh -c` (a seed's command, a live pane's attach); `None` runs the login shell.
    pub command: Option<String>,
    /// Typed into the program as soon as it starts (a captured command replayed into the login shell).
    pub initial_input: Option<String>,
    /// Where the program starts; one that is not a directory is ignored, as libghostty ignored it.
    pub working_directory: Option<PathBuf>,
    /// A command's exit holds the pane on "Process exited" until a key.
    pub wait_after_command: bool,
    /// Variables over everything else: the pane's environment, then a live pane's wrap configuration.
    pub env: Vec<(String, String)>,
    /// Variables of the app's own environment the program does not inherit.
    pub env_remove: Vec<String>,
}

impl Launch {
    /// The launch of `seed` in `working_directory`, its variables over the pane's own `env` (the GTK build's
    /// `merged_env`).
    #[must_use]
    pub fn from_seed(seed: &LaunchSeed, working_directory: Option<PathBuf>, env: &[(String, String)]) -> Self {
        let mut merged = env.to_vec();
        for (key, value) in &seed.env {
            match merged.iter_mut().find(|(known, _)| known == key) {
                Some(entry) => entry.1.clone_from(value),
                None => merged.push((key.clone(), value.clone())),
            }
        }
        Self {
            command: seed.command.clone(),
            initial_input: seed.initial_input.clone(),
            working_directory,
            wait_after_command: seed.wait_after_command,
            env: merged,
            env_remove: Vec::new(),
        }
    }
}

/// A launch made concrete: what [`Pane::spawn`] runs, the input typed after it starts and the exit handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub command: PaneCommand,
    pub initial_input: Option<Vec<u8>>,
    pub exit: ExitPolicy,
}

/// The first of `candidates` (absolute paths only) holding `shell-integration` with the compiled `xterm-ghostty`
/// beside it (`../terminfo/x/xterm-ghostty`, or `78/` as `tic` writes it on macOS and Windows): libghostty derived
/// `TERMINFO` from the resources, so a half-present set would claim a terminal no terminfo describes.
#[must_use]
pub fn resolve_resources(candidates: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    candidates
        .iter()
        .filter(|dir| dir.is_absolute())
        .find(|dir| {
            exists(&dir.join("shell-integration")) && TERMINFO_ENTRIES.iter().any(|entry| exists(&dir.join(entry)))
        })
        .cloned()
}

/// Where an agterm executable at `exe` looks for Ghostty's resources, first first: the staged layout beside it
/// (`bin/../share/ghostty`, `scripts/stage.sh`), then a system ghostty package.
#[must_use]
pub fn resource_candidates(exe: Option<&Path>) -> Vec<PathBuf> {
    let staged = exe
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|root| root.join(STAGED_RESOURCES));
    staged.into_iter().chain([PathBuf::from(SYSTEM_RESOURCES)]).collect()
}

impl Pane {
    /// Start `launch` under `host` on a new PTY of `config`'s size, with the exit handling of a launched pane; the
    /// launch's input is queued for the program at once.
    ///
    /// # Errors
    ///
    /// As [`Pane::spawn`] (`/bin/sh` itself could not start; a command that is not found ends with status 127
    /// instead, reported through [`crate::PaneEvent::Exited`]).
    pub fn launch(launch: &Launch, host: &Host, config: &PaneConfig) -> Result<Self, PaneError> {
        let prepared = host.prepare(launch, &ProcessEnv);
        let input = prepared.initial_input.unwrap_or_default();
        let config = PaneConfig {
            exit: prepared.exit,
            // the queue is empty: it takes the input whole, even a 10 MB `input = path:` file
            write_limit: config.write_limit.max(input.len()),
            ..config.clone()
        };
        let pane = Self::spawn(&prepared.command, &config)?;
        if !input.is_empty() {
            pane.write(&input)?;
        }
        Ok(pane)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use agterm_core::ghostty_config::Loader;
    use agterm_core::paths::MapEnv;

    use super::*;
    use crate::ABNORMAL_EXIT_RUNTIME;

    fn host(resources: Option<&str>) -> Host {
        Host {
            shell: "/bin/zsh".to_owned(),
            resources_dir: resources.map(PathBuf::from),
            term: "xterm-ghostty".to_owned(),
            bin_dir: Some(PathBuf::from("/opt/agterm/bin")),
            shell_features: ShellIntegrationFeatures::PATH,
            cursor_blink: true,
            version: "1.2.3".to_owned(),
            abnormal_exit_runtime: ABNORMAL_EXIT_RUNTIME,
            wait_after_command: false,
            env: Vec::new(),
            input: Vec::new(),
            working_directory: None,
            shell_integration: ShellIntegration::Detect,
        }
    }

    /// The host of a config made of `text`'s lines.
    fn host_of(text: &str, resources: Option<&Path>) -> Host {
        let mut loader = Loader::new(None);
        loader.load_text("test.conf", text);
        let config = loader.finish().finalize(false, &[]);
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        Host::new(
            &config.config,
            Some("/usr/local/bin/mysh".to_owned()),
            resources.map(Path::to_path_buf),
            None,
        )
    }

    /// A resources directory bundling the bash integration (and so `ENV` for a bash launch).
    fn bash_resources() -> tempfile::TempDir {
        let resources = tempfile::tempdir().unwrap();
        let bash = resources.path().join("shell-integration/bash/ghostty.bash");
        std::fs::create_dir_all(bash.parent().unwrap()).unwrap();
        std::fs::write(&bash, "").unwrap();
        resources
    }

    #[test]
    fn the_configs_env_goes_over_the_terminals_own_and_under_the_launchs() {
        let host = host_of("env = TERM=vt100\nenv = A=1\nenv = B=2\nenv = B=\n", None);
        let launch = Launch {
            env: vec![("A".to_owned(), "launch".to_owned())],
            ..Launch::default()
        };
        let vars = set(&host.prepare(&launch, &MapEnv::new()));
        expect(&vars, &[("TERM", "vt100"), ("A", "launch")]);
        assert!(!vars.contains_key("B"), "`B=` drops the key");
        // an empty value clears the map
        let cleared = host_of("env = A=1\nenv =\n", None);
        assert!(cleared.env.is_empty());
    }

    #[test]
    fn the_configs_input_is_typed_unless_the_launch_types_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("input.txt");
        std::fs::write(&file, b"from a file\n").unwrap();
        let host = host_of(
            &format!(
                "input = raw:echo \\x41\\n\ninput = path:{}\ninput = plain\n",
                file.display()
            ),
            None,
        );
        let typed = host.prepare(&Launch::default(), &MapEnv::new()).initial_input;
        assert_eq!(typed.as_deref(), Some(&b"echo A\nfrom a file\nplain"[..]));
        let own = Launch {
            initial_input: Some("ls\n".to_owned()),
            ..Launch::default()
        };
        assert_eq!(
            host.prepare(&own, &MapEnv::new()).initial_input.as_deref(),
            Some(&b"ls\n"[..])
        );
        // one source missing: none of the input
        let missing = host_of(
            &format!("input = raw:x\ninput = path:{}\n", dir.path().join("gone").display()),
            None,
        );
        assert_eq!(missing.prepare(&Launch::default(), &MapEnv::new()).initial_input, None);
        // an empty value clears the list
        assert!(host_of("input = raw:x\ninput =\n", None).input.is_empty());
    }

    #[test]
    fn the_configs_working_directory_serves_a_launch_without_its_own() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("work")).unwrap();
        let env = MapEnv::new().with("HOME", home.path().to_str().unwrap());
        let cwd = |text: &str, launch: &Launch| host_of(text, None).prepare(launch, &env).command.cwd;
        let plain = Launch::default();
        assert_eq!(cwd("working-directory = home\n", &plain).as_deref(), Some(home.path()));
        assert_eq!(
            cwd("working-directory = ~/work\n", &plain),
            Some(home.path().join("work"))
        );
        let absolute = format!("working-directory = \"{}\"\n", home.path().join("work").display());
        assert_eq!(cwd(&absolute, &plain), Some(home.path().join("work")));
        assert_eq!(
            cwd("working-directory = inherit\n", &plain),
            None,
            "the app's own, inherited"
        );
        assert_eq!(
            cwd("working-directory = relative/dir\n", &plain),
            None,
            "not absolute: ignored"
        );
        assert_eq!(cwd("", &plain), None);
        let own = Launch {
            working_directory: Some(home.path().join("work")),
            ..Launch::default()
        };
        assert_eq!(cwd("working-directory = home\n", &own), Some(home.path().join("work")));
        // a launch's own directory that is gone falls back to the config's, as `apprt/embedded.zig` keeps
        // `working-directory` when the embedder's cannot be opened
        let gone = Launch {
            working_directory: Some(home.path().join("deleted")),
            ..Launch::default()
        };
        assert_eq!(cwd("working-directory = home\n", &gone).as_deref(), Some(home.path()));
        assert_eq!(
            cwd("working-directory = ~/work\n", &gone),
            Some(home.path().join("work"))
        );
        assert_eq!(cwd("working-directory = inherit\n", &gone), None);
        assert_eq!(cwd("", &gone), None);
        let empty = Launch {
            working_directory: Some(PathBuf::new()),
            ..Launch::default()
        };
        assert_eq!(cwd("working-directory = home\n", &empty).as_deref(), Some(home.path()));
    }

    #[test]
    fn an_input_file_is_read_through_one_bounded_read() {
        let dir = tempfile::tempdir().unwrap();
        let at_limit = dir.path().join("at-limit");
        let limit = usize::try_from(INPUT_FILE_LIMIT).unwrap();
        std::fs::write(&at_limit, vec![b'a'; limit]).unwrap();
        assert_eq!(read_input_file(&at_limit).unwrap().len(), limit);
        let over = dir.path().join("over");
        std::fs::write(&over, vec![b'a'; limit + 1]).unwrap();
        assert!(read_input_file(&over).is_err(), "past 10 MB: refused");
        // a device and a FIFO are refused, without blocking the open or the read
        let fifo = dir.path().join("fifo");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
        for special in [Path::new("/dev/zero"), fifo.as_path()] {
            let path = special.to_owned();
            let (done, result) = std::sync::mpsc::channel();
            std::thread::spawn(move || done.send(read_input_file(&path).map(|bytes| bytes.len())));
            let read = result.recv_timeout(Duration::from_secs(10));
            assert!(matches!(read, Ok(Err(_))), "{}: {read:?}", special.display());
        }
        let host = host_of(
            &format!("input = raw:x\ninput = path:{}\n", Path::new("/dev/zero").display()),
            None,
        );
        assert_eq!(host.prepare(&Launch::default(), &MapEnv::new()).initial_input, None);
        // the read itself is bounded, so a source that keeps growing stops at the limit
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(read_bounded(std::io::repeat(b'x'), 1024).map(|bytes| bytes.len())));
        assert!(matches!(
            result.recv_timeout(Duration::from_secs(10)),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::FileTooLarge
        ));
        assert_eq!(read_bounded(&b"abc"[..], 3).unwrap(), b"abc");
    }

    #[test]
    fn an_input_path_with_escapes_reads_the_decoded_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("start up"), b"hi\n").unwrap();
        let host = host_of(&format!("input = path:{}/start\\x20up\n", dir.path().display()), None);
        assert_eq!(
            host.prepare(&Launch::default(), &MapEnv::new())
                .initial_input
                .as_deref(),
            Some(&b"hi\n"[..])
        );
    }

    #[test]
    fn wait_after_command_and_the_abnormal_runtime_come_from_the_config() {
        let host = host_of(
            "wait-after-command = true\nabnormal-command-exit-runtime = 1000\n",
            None,
        );
        assert_eq!(
            host.prepare(&Launch::default(), &MapEnv::new()).exit,
            ExitPolicy::Launched {
                argv: "/bin/sh -c /usr/local/bin/mysh".to_owned(),
                command: false,
                wait_after_command: true,
                abnormal_runtime: Duration::from_secs(1),
            }
        );
        let defaults = host_of("", None);
        assert!(!defaults.wait_after_command);
        assert_eq!(defaults.abnormal_exit_runtime, ABNORMAL_EXIT_RUNTIME);
    }

    #[test]
    fn shell_integration_none_injects_nothing_and_a_forced_shell_takes_the_login_shell() {
        let resources = bash_resources();
        let env = MapEnv::new().with("HOME", "/home/u");
        let bash = Launch {
            command: Some("bash".to_owned()),
            ..Launch::default()
        };
        // detect: a bash command line is bash's, the unknown login shell nobody's
        let detect = host_of("", Some(resources.path()));
        assert!(set(&detect.prepare(&bash, &env)).contains_key("ENV"));
        assert!(!set(&detect.prepare(&Launch::default(), &env)).contains_key("ENV"));
        // none: nothing injected, the features still set
        let none = host_of("shell-integration = none\n", Some(resources.path()));
        let prepared = none.prepare(&bash, &env);
        assert_eq!(prepared.command.args[1], OsString::from("bash"));
        let vars = set(&prepared);
        assert!(!vars.contains_key("ENV") && vars.contains_key("GHOSTTY_SHELL_FEATURES"));
        // forced bash: the login shell, whatever its name, takes bash's scheme; a command line keeps its own
        let forced = host_of("shell-integration = bash\n", Some(resources.path()));
        let shell = forced.prepare(&Launch::default(), &env);
        assert_eq!(shell.command.args[1], OsString::from("/usr/local/bin/mysh --posix"));
        assert!(set(&shell).contains_key("ENV"));
        let command = Launch {
            command: Some("zmx attach s1".to_owned()),
            ..Launch::default()
        };
        assert_eq!(
            forced.prepare(&command, &env).command.args[1],
            OsString::from("zmx attach s1")
        );
        // the integrations agterm does not port inject nothing
        for unported in ["elvish", "nushell"] {
            let host = host_of(&format!("shell-integration = {unported}\n"), Some(resources.path()));
            assert!(!set(&host.prepare(&bash, &env)).contains_key("ENV"), "{unported}");
        }
    }

    fn set(prepared: &Prepared) -> BTreeMap<String, String> {
        prepared
            .command
            .env
            .iter()
            .map(|(key, value)| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
            .collect()
    }

    fn removed(prepared: &Prepared) -> Vec<String> {
        prepared
            .command
            .env_remove
            .iter()
            .map(|key| key.to_string_lossy().into_owned())
            .collect()
    }

    /// Every `(key, value)` of `want` is set in `vars`.
    fn expect(vars: &BTreeMap<String, String>, want: &[(&str, &str)]) {
        for (key, value) in want {
            assert_eq!(vars.get(*key).map(String::as_str), Some(*value), "{key}");
        }
    }

    #[test]
    fn the_identity_follows_the_resolved_resources() {
        let env = MapEnv::new().with("PATH", "/usr/bin:/bin");
        let prepared = host(Some("/stage/share/ghostty")).prepare(&Launch::default(), &env);
        expect(
            &set(&prepared),
            &[
                ("TERM", "xterm-ghostty"),
                ("TERMINFO", "/stage/share/terminfo"),
                ("GHOSTTY_RESOURCES_DIR", "/stage/share/ghostty"),
                ("COLORTERM", "truecolor"),
                ("TERM_PROGRAM", "ghostty"),
                ("TERM_PROGRAM_VERSION", "1.2.3"),
                ("GHOSTTY_BIN_DIR", "/opt/agterm/bin"),
                ("PATH", "/usr/bin:/bin:/opt/agterm/bin"),
                ("GHOSTTY_SHELL_FEATURES", "path"),
            ],
        );
        assert_eq!(removed(&prepared), ["GHOSTTY_LOG", "VTE_VERSION"]);
        // the config's `term` is what a resolved terminal claims
        let kitty = Host {
            term: "xterm-kitty".to_owned(),
            ..host(Some("/stage/share/ghostty"))
        };
        expect(
            &set(&kitty.prepare(&Launch::default(), &env)),
            &[("TERM", "xterm-kitty")],
        );
    }

    #[test]
    fn without_the_terminfo_the_terminal_claims_xterm_256color() {
        // whatever the config's `term`, and with no inherited TERMINFO or resources pointing elsewhere
        let fallback = Host {
            term: "xterm-kitty".to_owned(),
            ..host(None)
        };
        let prepared = fallback.prepare(&Launch::default(), &MapEnv::new());
        let vars = set(&prepared);
        expect(&vars, &[("TERM", FALLBACK_TERM), ("COLORTERM", "truecolor")]);
        assert!(!vars.contains_key("TERMINFO") && !vars.contains_key("GHOSTTY_RESOURCES_DIR"));
        let removed = removed(&prepared);
        assert!(
            ["TERMINFO", "GHOSTTY_RESOURCES_DIR"]
                .iter()
                .all(|key| removed.iter().any(|gone| gone == key))
        );
    }

    #[test]
    fn the_bin_dir_is_appended_to_path_once() {
        let launch = Launch::default();
        let present = MapEnv::new().with("PATH", "/opt/agterm/bin:/usr/bin");
        assert!(!set(&host(None).prepare(&launch, &present)).contains_key("PATH"));
        assert_eq!(
            set(&host(None).prepare(&launch, &MapEnv::new()))["PATH"],
            "/opt/agterm/bin"
        );
        let no_bin = Host {
            bin_dir: None,
            ..host(None)
        };
        let vars = set(&no_bin.prepare(&launch, &present));
        assert!(!vars.contains_key("PATH") && !vars.contains_key("GHOSTTY_BIN_DIR"));
    }

    #[test]
    fn a_command_runs_through_bin_sh_and_the_launch_wins_over_everything() {
        let launch = Launch {
            command: Some("htop -d 5".to_owned()),
            initial_input: Some(String::new()),
            wait_after_command: true,
            env: vec![
                ("TERM".to_owned(), "dumb".to_owned()),
                ("AGTERM_PANE_ID".to_owned(), "X".to_owned()),
            ],
            env_remove: vec!["ZMX_DIR".to_owned(), "GHOSTTY_SHELL_FEATURES".to_owned()],
            working_directory: Some(PathBuf::from("/nonexistent/dir")),
        };
        let prepared = host(None).prepare(&launch, &MapEnv::new());
        assert_eq!(prepared.command.program, OsString::from("/bin/sh"));
        assert_eq!(
            prepared.command.args,
            [OsString::from("-c"), OsString::from("htop -d 5")]
        );
        let vars = set(&prepared);
        expect(&vars, &[("TERM", "dumb"), ("AGTERM_PANE_ID", "X")]);
        assert!(!vars.contains_key("GHOSTTY_SHELL_FEATURES") && !vars.contains_key("PWD"));
        assert!(removed(&prepared).contains(&"ZMX_DIR".to_owned()));
        // a working directory that is not one is ignored, as libghostty ignored it; empty input types nothing
        assert_eq!((&prepared.command.cwd, &prepared.initial_input), (&None, &None));
        assert_eq!(
            prepared.exit,
            ExitPolicy::Launched {
                argv: "/bin/sh -c htop -d 5".to_owned(),
                command: true,
                wait_after_command: true,
                abnormal_runtime: ABNORMAL_EXIT_RUNTIME,
            }
        );
    }

    #[test]
    fn a_working_directory_is_entered_and_the_login_shell_runs_as_a_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let launch = Launch {
            working_directory: Some(dir.path().to_path_buf()),
            ..Launch::default()
        };
        let prepared = host(None).prepare(&launch, &MapEnv::new());
        assert_eq!(prepared.command.cwd.as_deref(), Some(dir.path()));
        assert_eq!(set(&prepared)["PWD"], dir.path().to_string_lossy());
        // the login shell, run as a command line too
        assert_eq!(prepared.command.args[1], OsString::from("/bin/zsh"));
        assert!(matches!(prepared.exit, ExitPolicy::Launched { command: false, .. }));
    }

    #[test]
    fn the_login_shell_gets_its_integration_and_features() {
        let resources = tempfile::tempdir().unwrap();
        let bash = resources.path().join("shell-integration/bash/ghostty.bash");
        std::fs::create_dir_all(bash.parent().unwrap()).unwrap();
        std::fs::write(&bash, "").unwrap();
        let bash_host = Host {
            shell: "/usr/bin/bash".to_owned(),
            shell_features: ShellIntegrationFeatures::CURSOR | ShellIntegrationFeatures::SUDO,
            cursor_blink: false,
            ..host(Some(resources.path().to_str().unwrap()))
        };
        let env = MapEnv::new().with("HOME", "/home/u");
        let prepared = bash_host.prepare(&Launch::default(), &env);
        assert_eq!(prepared.command.args[1], OsString::from("/usr/bin/bash --posix"));
        let vars = set(&prepared);
        assert_eq!(vars["ENV"], bash.to_string_lossy());
        assert_eq!(vars["GHOSTTY_BASH_INJECT"], "1");
        assert_eq!(vars["HISTFILE"], "/home/u/.bash_history");
        assert_eq!(vars["GHOSTTY_SHELL_FEATURES"], "cursor:steady,sudo");
        // a command that is not a shell runs as given; no resources, no integration
        let other = Launch {
            command: Some("bash -c true".to_owned()),
            ..Launch::default()
        };
        assert_eq!(
            bash_host.prepare(&other, &env).command.args[1],
            OsString::from("bash -c true")
        );
        let bare = Host {
            resources_dir: None,
            ..bash_host
        };
        let prepared = bare.prepare(&Launch::default(), &env);
        assert_eq!(prepared.command.args[1], OsString::from("/usr/bin/bash"));
        assert!(!set(&prepared).contains_key("ENV"));
    }

    #[test]
    fn a_seed_runs_with_its_variables_over_the_panes() {
        let seed = LaunchSeed {
            command: Some("zmx attach x".to_owned()),
            initial_input: Some("ls\n".to_owned()),
            wait_after_command: false,
            env: vec![
                ("SHELL".to_owned(), "/bin/zsh".to_owned()),
                ("ZMX_DIR".to_owned(), "/d".to_owned()),
            ],
            live: true,
        };
        let pane_env = vec![
            ("AGTERM_PANE_ID".to_owned(), "P".to_owned()),
            ("SHELL".to_owned(), "/bin/sh".to_owned()),
        ];
        let launch = Launch::from_seed(&seed, Some(PathBuf::from("/w")), &pane_env);
        assert_eq!(launch.command.as_deref(), Some("zmx attach x"));
        assert_eq!(launch.initial_input.as_deref(), Some("ls\n"));
        assert_eq!(launch.working_directory, Some(PathBuf::from("/w")));
        assert_eq!(
            launch.env,
            [
                ("AGTERM_PANE_ID".to_owned(), "P".to_owned()),
                ("SHELL".to_owned(), "/bin/zsh".to_owned()),
                ("ZMX_DIR".to_owned(), "/d".to_owned()),
            ]
        );
    }

    #[test]
    fn resources_need_shell_integration_and_terminfo_beside_them() {
        let candidates = [
            PathBuf::from("relative/ghostty"),
            PathBuf::from("/stage/share/ghostty"),
            PathBuf::from("/usr/share/ghostty"),
        ];
        let complete = |path: &Path| {
            [
                "relative/ghostty/shell-integration",
                "relative/ghostty/../terminfo/x/xterm-ghostty",
                "/stage/share/ghostty/shell-integration",
                "/usr/share/ghostty/shell-integration",
                "/usr/share/ghostty/../terminfo/x/xterm-ghostty",
            ]
            .contains(&path.to_str().unwrap())
        };
        // relative candidates never count; the stage lacks its terminfo, the system package has both
        assert_eq!(
            resolve_resources(&candidates, complete),
            Some(PathBuf::from("/usr/share/ghostty"))
        );
        assert_eq!(resolve_resources(&candidates, |_| false), None);
        // macOS's tic writes the hashed layout, which counts as well
        let hashed = |path: &Path| {
            [
                "/stage/share/ghostty/shell-integration",
                "/stage/share/ghostty/../terminfo/78/xterm-ghostty",
            ]
            .contains(&path.to_str().unwrap())
        };
        assert_eq!(
            resolve_resources(&candidates, hashed),
            Some(PathBuf::from("/stage/share/ghostty"))
        );
        assert_eq!(
            resource_candidates(Some(Path::new("/opt/agterm/bin/agterm"))),
            [
                PathBuf::from("/opt/agterm/share/ghostty"),
                PathBuf::from("/usr/share/ghostty")
            ]
        );
        assert_eq!(resource_candidates(None), [PathBuf::from("/usr/share/ghostty")]);
    }

    #[test]
    fn the_host_reads_the_config_and_a_runtime() {
        let mut config = GhosttyConfig {
            term: "xterm-kitty".to_owned(),
            ..GhosttyConfig::default()
        };
        config.cursor.style_blink = Some(false);
        let host = Host::new(&config, None, None, None);
        assert_eq!(host.shell, "sh");
        assert_eq!(host.term, "xterm-kitty");
        assert!(!host.cursor_blink);
        // ghostty's defaults are cursor, title and path; the ssh wrappers never reach the shell
        config.shell_integration_features = ShellIntegrationFeatures::all();
        assert!(
            !Host::new(&config, None, None, None)
                .shell_features
                .intersects(ShellIntegrationFeatures::UNSUPPORTED)
        );
        assert_eq!(host.version, crate::vt::version());
        let runtime = Runtime {
            login_shell: Some("/bin/fish".to_owned()),
            resources_dir: Some(PathBuf::from("/nonexistent/share/ghostty")),
            ..Runtime::default()
        };
        let live = Host::from_runtime(&config, &runtime, None);
        assert_eq!(live.shell, "/bin/fish");
        assert_eq!(live.resources_dir, None, "no terminfo beside them");
    }
}
