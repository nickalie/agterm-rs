//! Launching a pane's program as libghostty launches a surface's for an embedder (`termio/Exec.zig`,
//! `apprt/embedded.zig` and `Surface.zig` at `LIBGHOSTTY_VT_REV`; the GTK build's `surface.rs` for the seed): the
//! terminal's identity, the shell integration and the exit handling every pane shares.
//!
//! - **Program**: a [`Launch`]'s command line, or else the host's login shell, runs through `/bin/sh -c` (libghostty
//!   ran a surface's command that way on Linux, and core's foreground reader looks past exactly that wrapper). A live
//!   pane runs no program: it attaches to its session in process ([`crate::session_io`]) with what its
//!   `ngterm-session attach` line would read ([`Launch::live`], from
//!   [`ngterm_core::restore::spawn_plan::resolve_live_seed`]): the line's session and creation argv, the environment
//!   below (carrying the wrap configuration from [`Runtime`]) and the working directory; a plain pane runs its seed's
//!   command, or the login shell with its `initial_input` typed in.
//! - **Windows**: a command line runs as `cmd.exe`'s command text, `%ComSpec% /D /S /C "<line>"` (no `AutoRun`, as
//!   `sh -c` reads no rc file; `/S` with the outer quotes keeps the line's own quotes as written, which a quoted
//!   program path needs), its tail passed as written ([`PaneCommand::raw_args`]). The login shell runs directly, as
//!   Ghostty runs its shell on Windows (`termio/Exec.zig`): a value naming a file runs whole (a relative one found in
//!   the launch's directory first, where the child runs; the login shell from
//!   [`ngterm_session::launch::login_shell`] is a full path, often under `C:\Program Files`), anything else (the
//!   config's `command`) is split on whitespace as Ghostty splits it, quotes and all. No shell
//!   integration is injected there (the zsh, bash and fish ones are POSIX shells'); `PATH` takes `;`.
//! - **Environment**, later steps winning: the app's own; `GHOSTTY_LOG` and `VTE_VERSION` removed; the terminal
//!   identity ([`Host`]); `GHOSTTY_SHELL_FEATURES`; the shell integration of a zsh, bash or fish command line
//!   ([`ngterm_core::live::shell_integration::setup`], as `shell-integration` asks); the config's `env`; the launch's
//!   own variables; `PWD` for a working directory. On Windows, whose names ignore case, a later step's key replaces
//!   every spelling of it (`PATH` over `Path`).
//! - **Config** (`Surface.zig` and `termio/Exec.zig`, the embedder's options winning as `apprt/embedded.zig` lets
//!   them): the launch's working directory, else (none, or not a directory) `working-directory`'s; the launch's initial input, else `input`'s;
//!   `wait-after-command` for every launch; `abnormal-command-exit-runtime`; `shell-integration`'s forced shell for the
//!   login shell (a command line keeps its own program's scheme: a live pane's `ngterm-session attach` is no shell).
//! - **Identity** (Technical Details > Terminal identity): with Ghostty's resources and the compiled `xterm-ghostty`
//!   beside them, `TERM` is the config's `term`, `TERMINFO` that terminfo directory and `GHOSTTY_RESOURCES_DIR` the
//!   resources; without them `TERM=xterm-256color` and neither variable. `COLORTERM=truecolor`,
//!   `TERM_PROGRAM=ghostty` and the libghostty-vt version as `TERM_PROGRAM_VERSION` either way; ngterm's executable
//!   directory as `GHOSTTY_BIN_DIR`, appended to `PATH`.
//! - **Exit**: [`ExitPolicy::Launched`] (see [`crate::exit`]).
//!
//! Not carried over from libghostty: the elvish and nushell integrations (`shell-integration = elvish` or `nushell`
//! injects nothing), Flatpak's host spawning and `GHOSTTY_SURFACE_ID`; the config's `command` is the frontend's
//! (what a pane without one runs).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ngterm_core::ghostty_config::{
    GhosttyConfig, InputSource, ShellIntegration, ShellIntegrationFeatures, WorkingDirectory,
};
use ngterm_core::live::shell_integration::{self, Inherited, LoginShell};
use ngterm_core::live::support::LiveAttach;
use ngterm_core::paths::{self, Env, ProcessEnv};
use ngterm_core::restore::spawn_plan::LaunchSeed;
use ngterm_session::launch::Runtime;

use crate::exit::ExitPolicy;
use crate::{Pane, PaneCommand, PaneConfig, PaneError};

/// The shell a command line runs through, as libghostty ran a surface's command on Linux: `/bin/sh -c <line>`; on
/// Windows `cmd.exe` (`%ComSpec%` when set) `/D /S /C "<line>"`.
pub const COMMAND_SHELL: &str = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };
/// `cmd`'s switches before a command line: no `AutoRun`, and exactly the outer quotes stripped.
#[cfg(any(windows, test))]
const WINDOWS_SHELL_ARGS: &[&str] = &["/D", "/S", "/C"];
/// The login shell a host without one runs: Ghostty's default (`sh`, `cmd.exe` on Windows).
const DEFAULT_SHELL: &str = if cfg!(windows) { "cmd.exe" } else { "sh" };
/// What separates `PATH`'s entries.
const PATH_SEPARATOR: char = if cfg!(windows) { ';' } else { ':' };
/// What the terminal claims without its own terminfo.
pub const FALLBACK_TERM: &str = "xterm-256color";
/// The terminfo entry beside Ghostty's resources that lets the terminal claim `xterm-ghostty`, relative to them, in
/// either layout `tic` writes: a directory per first letter (`x/`, ncurses on Linux) or per its hex code (`78/`,
/// where the file system ignores case: macOS, Git Bash on Windows).
const TERMINFO_ENTRIES: [&str; 2] = ["../terminfo/x/xterm-ghostty", "../terminfo/78/xterm-ghostty"];
/// The staged resources relative to the staged root (`scripts/stage.sh`).
const STAGED_RESOURCES: &str = "share/ghostty";
/// A system ghostty package's resources (none on Windows).
#[cfg(unix)]
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
    /// The shell a pane without a command runs: the password database's login shell (on Windows the PowerShell
    /// `ngterm_session::launch::login_shell` finds), `sh` without one (`cmd.exe` on Windows).
    pub shell: String,
    /// Ghostty's resources (shell integration, themes) with the compiled `xterm-ghostty` beside them
    /// ([`resolve_resources`]); `None` claims `xterm-256color` and integrates no shell.
    pub resources_dir: Option<PathBuf>,
    /// The config's `term`, claimed while the terminfo resolves.
    pub term: String,
    /// The directory of ngterm's executable: `GHOSTTY_BIN_DIR`, and the last entry of `PATH`.
    pub bin_dir: Option<PathBuf>,
    /// `shell-integration-features`, the ssh wrappers ngterm cannot serve dropped.
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
    /// [`resolve_resources`] found, from ngterm's executable directory `bin_dir`.
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
                .unwrap_or_else(|| DEFAULT_SHELL.to_owned()),
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
        let (mut command, argv) = program(&line, launch.command.is_none(), cwd.as_deref(), env);
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
                argv,
                command: launch.command.is_some(),
                wait_after_command: launch.wait_after_command || self.wait_after_command,
                abnormal_runtime: self.abnormal_exit_runtime,
            },
        }
    }

    /// Whether and how the shell integration is injected into `launch` (never on Windows: the zsh, bash and fish
    /// integrations are POSIX shells').
    fn integration(&self, launch: &Launch) -> Integration {
        if cfg!(windows) {
            return Integration::Off;
        }
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
    /// The home is [`paths::home_dir`]'s: a non-empty `HOME`, else the user's own (the password database's on Unix;
    /// on Windows, where `HOME` is seldom set, the profile directory, `USERPROFILE`).
    fn default_directory(&self, env: &dyn Env) -> Option<PathBuf> {
        let home = || paths::home_dir(env);
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
                Some(path) if path.split(PATH_SEPARATOR).any(|entry| entry == bin) => {}
                Some(path) => vars.set("PATH", format!("{path}{PATH_SEPARATOR}{bin}")),
                None => vars.set("PATH", bin.clone()),
            }
            vars.set("GHOSTTY_BIN_DIR", bin);
        }
        vars.set("TERM_PROGRAM", "ghostty".to_owned());
        vars.set("TERM_PROGRAM_VERSION", self.version.clone());
    }
}

/// What runs `line` in `cwd` (the login shell when `shell`), and the program line the failure report names.
#[cfg(unix)]
fn program(line: &str, _shell: bool, _cwd: Option<&Path>, _env: &dyn Env) -> (PaneCommand, String) {
    let command = PaneCommand::new(COMMAND_SHELL).arg("-c").arg(line);
    (command, format!("{COMMAND_SHELL} -c {line}"))
}

/// What runs `line` in `cwd` (the login shell when `shell`), and the program line the failure report names.
#[cfg(windows)]
fn program(line: &str, shell: bool, cwd: Option<&Path>, env: &dyn Env) -> (PaneCommand, String) {
    if shell {
        return windows_shell(line, cwd, Path::is_file);
    }
    let comspec = env.var("ComSpec").filter(|comspec| !comspec.is_empty()).map_or_else(
        || COMMAND_SHELL.to_owned(),
        |comspec| comspec.to_string_lossy().into_owned(),
    );
    windows_command(line, &comspec)
}

/// `cmd`'s run of the command line `line`: `/D /S /C`, then the line in quotes, passed as written.
#[cfg(any(windows, test))]
fn windows_command(line: &str, cmd: &str) -> (PaneCommand, String) {
    let tail = format!("\"{line}\"");
    let argv = format!("{cmd} {} {tail}", WINDOWS_SHELL_ARGS.join(" "));
    let command = PaneCommand::new(cmd).args(WINDOWS_SHELL_ARGS).raw_arg(tail);
    (command, argv)
}

/// The Windows login shell `value` run directly: whole when it names a file, else split on whitespace as Ghostty
/// splits its shell on Windows. A relative value with a directory or a space in it is looked up under `cwd` first,
/// where the child runs (as `sh -c` finds `./shell` in the child's directory; `CreateProcessW` would resolve a
/// relative program against the app's); a bare name without either is left whole for the `PATH` search.
#[cfg(any(windows, test))]
fn windows_shell(value: &str, cwd: Option<&Path>, is_file: impl Fn(&Path) -> bool) -> (PaneCommand, String) {
    let path = Path::new(value);
    let spaced = value.contains([' ', '\t']);
    let bare = !value.contains(['/', '\\']);
    let under_cwd = cwd
        .filter(|_| path.is_relative() && (spaced || !bare))
        .map(|cwd| cwd.join(path))
        .filter(|file| is_file(file));
    let command = if let Some(file) = under_cwd {
        PaneCommand::new(file)
    } else if !spaced || is_file(path) {
        PaneCommand::new(value)
    } else {
        let mut words = value.split([' ', '\t']).filter(|word| !word.is_empty());
        PaneCommand::new(words.next().unwrap_or(value)).args(words)
    };
    (command, value.to_owned())
}

/// How a launch's shell integration is injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Integration {
    /// Not at all (`shell-integration = none`, or a scheme ngterm does not port).
    Off,
    /// By the program's name.
    Detect,
    /// As this shell's, whatever the program's name.
    As(LoginShell),
}

/// Variables set over the app's environment, and removed from it. Where names ignore case (Windows), a key replaces
/// every spelling of it, so the last layer wins there as well: portable-pty folds the keys it is given in its own
/// order, which would let an earlier layer's `Path` override a later `PATH`, or a staged `PATH` outlive a removed
/// `path`.
#[derive(Debug)]
struct Vars {
    set: BTreeMap<String, String>,
    removed: Vec<String>,
    /// Whether names ignore case (folded as portable-pty folds them: lowercase).
    fold_case: bool,
}

impl Default for Vars {
    fn default() -> Self {
        Self::folding(cfg!(windows))
    }
}

impl Vars {
    fn folding(fold_case: bool) -> Self {
        Self {
            set: BTreeMap::new(),
            removed: Vec::new(),
            fold_case,
        }
    }

    fn set(&mut self, key: &str, value: String) {
        self.forget(key);
        self.set.insert(key.to_owned(), value);
    }

    fn remove(&mut self, key: &str) {
        self.forget(key);
        self.removed.push(key.to_owned());
    }

    /// Drop every spelling of `key` from both lists.
    fn forget(&mut self, key: &str) {
        let folded = self.fold_case.then(|| key.to_lowercase());
        let same = |other: &str| match &folded {
            Some(folded) => other.to_lowercase() == *folded,
            None => other == key,
        };
        self.removed.retain(|removed| !same(removed));
        self.set.retain(|set, _| !same(set));
    }
}

/// An `input = path:` file's contents, read through one handle: `termio/Termio.zig` opens the file and reads at most
/// 10 MB of it (`readToEndAlloc`, an error past that), on its own IO thread. A pane is prepared on the UI thread, so
/// only a regular file is read (opened without blocking, so a FIFO cannot stall the open); a device or a FIFO, which
/// libghostty would read there, is refused.
fn read_input_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    let file = options.open(path)?;
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
    /// A shell line run through `/bin/sh -c` (`cmd /D /S /C` on Windows: a seed's command, a live pane's attach);
    /// `None` runs the login shell.
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
    /// A live pane's attach to its session, run in process in place of `command`, its shell line (which the exit
    /// report then names).
    pub live: Option<LiveAttach>,
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
            live: seed.attach.clone(),
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

/// Where an ngterm executable at `exe` looks for Ghostty's resources, first first: the staged layout beside it
/// (`bin/../share/ghostty`, `scripts/stage.sh`), then a system ghostty package (not on Windows).
#[must_use]
pub fn resource_candidates(exe: Option<&Path>) -> Vec<PathBuf> {
    let staged = exe
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|root| root.join(STAGED_RESOURCES));
    #[cfg(unix)]
    let system = Some(PathBuf::from(SYSTEM_RESOURCES));
    #[cfg(windows)]
    let system = None;
    staged.into_iter().chain(system).collect()
}

impl Pane {
    /// Start `launch` under `host` on a new PTY of `config`'s size, with the exit handling of a launched pane; the
    /// launch's input is queued for the program at once. A live launch attaches to its session in process instead
    /// (module docs); an attach that fails ends the pane with the CLI's status, reported through
    /// [`crate::PaneEvent::Exited`].
    ///
    /// # Errors
    ///
    /// As [`Pane::spawn`] (`/bin/sh` itself could not start; a command that is not found ends with status 127
    /// instead, 1 under `cmd.exe`, reported through [`crate::PaneEvent::Exited`]).
    pub fn launch(launch: &Launch, host: &Host, config: &PaneConfig) -> Result<Self, PaneError> {
        let prepared = host.prepare(launch, &ProcessEnv);
        let input = prepared.initial_input.unwrap_or_default();
        let config = PaneConfig {
            exit: prepared.exit,
            // the queue is empty: it takes the input whole, even a 10 MB `input = path:` file
            write_limit: config.write_limit.max(input.len()),
            ..config.clone()
        };
        let pane = match &launch.live {
            Some(attach) => {
                let (env, current_dir) = crate::session_io::app_environment();
                let session = crate::session_io::resolve(attach, &prepared.command, env, current_dir);
                Self::attach_live(session, &config)?
            }
            None => Self::spawn(&prepared.command, &config)?,
        };
        if !input.is_empty() {
            pane.write(&input)?;
        }
        Ok(pane)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use ngterm_core::ghostty_config::Loader;
    use ngterm_core::paths::MapEnv;

    use super::*;
    use crate::ABNORMAL_EXIT_RUNTIME;

    fn host(resources: Option<&str>) -> Host {
        Host {
            shell: "/bin/zsh".to_owned(),
            resources_dir: resources.map(PathBuf::from),
            term: "xterm-ghostty".to_owned(),
            bin_dir: Some(PathBuf::from("/opt/ngterm/bin")),
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

    /// `path` as a config value names it: with `/` for Windows' `\`, which would start an escape there (`input`'s
    /// values are Zig string literals' bodies), and which Windows takes as a separator too.
    fn config_path(path: &Path) -> String {
        path.display().to_string().replace('\\', "/")
    }

    #[test]
    fn the_configs_input_is_typed_unless_the_launch_types_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("input.txt");
        std::fs::write(&file, b"from a file\n").unwrap();
        let host = host_of(
            &format!(
                "input = raw:echo \\x41\\n\ninput = path:{}\ninput = plain\n",
                config_path(&file)
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
            &format!(
                "input = raw:x\ninput = path:{}\n",
                config_path(&dir.path().join("gone"))
            ),
            None,
        );
        assert_eq!(missing.prepare(&Launch::default(), &MapEnv::new()).initial_input, None);
        // an empty value clears the list
        assert!(host_of("input = raw:x\ninput =\n", None).input.is_empty());
    }

    /// Without `HOME` (as on Windows, mostly), or with it empty, `home` is the user's own home: the password
    /// database's, the profile directory on Windows.
    #[test]
    fn the_home_directory_without_home_is_the_users_own() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("work")).unwrap();
        let plain = Launch::default();
        for env in [
            MapEnv::new().with_passwd_home(home.path()),
            MapEnv::new().with("HOME", "").with_passwd_home(home.path()),
        ] {
            let cwd = |text: &str| host_of(text, None).prepare(&plain, &env).command.cwd;
            assert_eq!(cwd("working-directory = home\n").as_deref(), Some(home.path()));
            assert_eq!(cwd("working-directory = ~/work\n"), Some(home.path().join("work")));
        }
        assert_eq!(
            host_of("working-directory = home\n", None)
                .prepare(&plain, &MapEnv::new())
                .command
                .cwd,
            None,
            "no home at all: the app's own"
        );
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
        let absolute = format!("working-directory = \"{}\"\n", config_path(&home.path().join("work")));
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
        assert!(read_input_file(dir.path()).is_err(), "a directory: refused");
        // the read itself is bounded, so a source that keeps growing stops at the limit
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(read_bounded(std::io::repeat(b'x'), 1024).map(|bytes| bytes.len())));
        assert!(matches!(
            result.recv_timeout(Duration::from_secs(10)),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::FileTooLarge
        ));
        assert_eq!(read_bounded(&b"abc"[..], 3).unwrap(), b"abc");
    }

    // Unix only: it needs `mkfifo` and `/dev/zero` (the open without blocking is `O_NONBLOCK`, Unix's)
    #[cfg(unix)]
    #[test]
    fn a_device_or_a_fifo_is_refused_as_an_input_file() {
        let dir = tempfile::tempdir().unwrap();
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
    }

    #[test]
    fn an_input_path_with_escapes_reads_the_decoded_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("start up"), b"hi\n").unwrap();
        let host = host_of(
            &format!("input = path:{}/start\\x20up\n", config_path(dir.path())),
            None,
        );
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
        // the login shell through `sh -c`; on Windows run directly
        let argv = if cfg!(windows) {
            "/usr/local/bin/mysh"
        } else {
            "/bin/sh -c /usr/local/bin/mysh"
        };
        assert_eq!(
            host.prepare(&Launch::default(), &MapEnv::new()).exit,
            ExitPolicy::Launched {
                argv: argv.to_owned(),
                command: false,
                wait_after_command: true,
                abnormal_runtime: Duration::from_secs(1),
            }
        );
        let defaults = host_of("", None);
        assert!(!defaults.wait_after_command);
        assert_eq!(defaults.abnormal_exit_runtime, ABNORMAL_EXIT_RUNTIME);
    }

    // Unix only: the POSIX shells' integration, which Windows never injects (that is
    // `windows_runs_commands_through_cmd_and_the_login_shell_directly`'s), and `/bin/sh -c`'s argv
    #[cfg(unix)]
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
            command: Some("ngterm-session attach s1".to_owned()),
            ..Launch::default()
        };
        assert_eq!(
            forced.prepare(&command, &env).command.args[1],
            OsString::from("ngterm-session attach s1")
        );
        // the integrations ngterm does not port inject nothing
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
        let env = MapEnv::new().with("PATH", format!("/usr/bin{PATH_SEPARATOR}/bin"));
        let prepared = host(Some("/stage/share/ghostty")).prepare(&Launch::default(), &env);
        // the terminfo beside the resources, joined with the OS's separator
        let terminfo = Path::new("/stage/share").join("terminfo");
        let path = format!("/usr/bin{PATH_SEPARATOR}/bin{PATH_SEPARATOR}/opt/ngterm/bin");
        expect(
            &set(&prepared),
            &[
                ("TERM", "xterm-ghostty"),
                ("TERMINFO", &terminfo.to_string_lossy()),
                ("GHOSTTY_RESOURCES_DIR", "/stage/share/ghostty"),
                ("COLORTERM", "truecolor"),
                ("TERM_PROGRAM", "ghostty"),
                ("TERM_PROGRAM_VERSION", "1.2.3"),
                ("GHOSTTY_BIN_DIR", "/opt/ngterm/bin"),
                ("PATH", &path),
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
        let present = MapEnv::new().with("PATH", format!("/opt/ngterm/bin{PATH_SEPARATOR}/usr/bin"));
        assert!(!set(&host(None).prepare(&launch, &present)).contains_key("PATH"));
        assert_eq!(
            set(&host(None).prepare(&launch, &MapEnv::new()))["PATH"],
            "/opt/ngterm/bin"
        );
        let no_bin = Host {
            bin_dir: None,
            ..host(None)
        };
        let vars = set(&no_bin.prepare(&launch, &present));
        assert!(!vars.contains_key("PATH") && !vars.contains_key("GHOSTTY_BIN_DIR"));
    }

    #[test]
    fn a_command_runs_through_the_command_shell_and_the_launch_wins_over_everything() {
        let launch = Launch {
            command: Some("htop -d 5".to_owned()),
            initial_input: Some(String::new()),
            wait_after_command: true,
            env: vec![
                ("TERM".to_owned(), "dumb".to_owned()),
                ("NGTERM_PANE_ID".to_owned(), "X".to_owned()),
            ],
            env_remove: vec!["NGTERM_LIVE_DIR".to_owned(), "GHOSTTY_SHELL_FEATURES".to_owned()],
            working_directory: Some(PathBuf::from("/nonexistent/dir")),
            live: None,
        };
        let prepared = host(None).prepare(&launch, &MapEnv::new());
        // `/bin/sh -c <line>`; on Windows (no `ComSpec` here) `cmd.exe /D /S /C "<line>"`
        let argv = if cfg!(windows) {
            assert_eq!(prepared.command.program, OsString::from("cmd.exe"));
            assert_eq!(prepared.command.args, ["/D", "/S", "/C"].map(OsString::from));
            assert_eq!(prepared.command.raw_args, [OsString::from("\"htop -d 5\"")]);
            r#"cmd.exe /D /S /C "htop -d 5""#
        } else {
            assert_eq!(prepared.command.program, OsString::from("/bin/sh"));
            assert_eq!(
                prepared.command.args,
                [OsString::from("-c"), OsString::from("htop -d 5")]
            );
            "/bin/sh -c htop -d 5"
        };
        let vars = set(&prepared);
        expect(&vars, &[("TERM", "dumb"), ("NGTERM_PANE_ID", "X")]);
        assert!(!vars.contains_key("GHOSTTY_SHELL_FEATURES") && !vars.contains_key("PWD"));
        assert!(removed(&prepared).contains(&"NGTERM_LIVE_DIR".to_owned()));
        // a working directory that is not one is ignored, as libghostty ignored it; empty input types nothing
        assert_eq!((&prepared.command.cwd, &prepared.initial_input), (&None, &None));
        assert_eq!(
            prepared.exit,
            ExitPolicy::Launched {
                argv: argv.to_owned(),
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
        // the login shell, run as a command line too (on Windows run directly, as Ghostty runs it there)
        if cfg!(windows) {
            assert_eq!(prepared.command.program, OsString::from("/bin/zsh"));
        } else {
            assert_eq!(prepared.command.args[1], OsString::from("/bin/zsh"));
        }
        assert!(matches!(prepared.exit, ExitPolicy::Launched { command: false, .. }));
    }

    // Unix only: the POSIX shells' integration, which Windows never injects (that is
    // `windows_runs_commands_through_cmd_and_the_login_shell_directly`'s), and `/bin/sh -c`'s argv
    #[cfg(unix)]
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
    fn a_windows_command_line_runs_as_cmds_command_text_with_its_quotes_kept() {
        let line = r#""C:\Program Files\ngterm\ngterm-session.exe" attach "my session""#;
        let (command, argv) = windows_command(line, r"C:\Windows\System32\cmd.exe");
        assert_eq!(command.program, OsString::from(r"C:\Windows\System32\cmd.exe"));
        assert_eq!(command.args, ["/D", "/S", "/C"].map(OsString::from));
        // the whole line in one more pair of quotes, which `/S` strips: the line's own reach cmd as written
        assert_eq!(command.raw_args, [OsString::from(format!("\"{line}\""))]);
        assert_eq!(argv, format!(r#"C:\Windows\System32\cmd.exe /D /S /C "{line}""#));
    }

    #[test]
    fn a_windows_login_shell_runs_whole_when_it_is_a_file_else_split_on_whitespace() {
        let pwsh = r"C:\Program Files\PowerShell\7\pwsh.exe";
        let cwd = Path::new(r"C:\work");
        let relative = cwd.join("my shell.exe");
        let dotted = cwd.join(r".\shell.exe");
        let nested = cwd.join(r"bin\shell.exe");
        let bare = cwd.join("shell.exe");
        let files = [
            Path::new(pwsh),
            relative.as_path(),
            dotted.as_path(),
            nested.as_path(),
            bare.as_path(),
        ];
        let is_file = |path: &Path| files.contains(&path);
        let (command, argv) = windows_shell(pwsh, None, is_file);
        assert_eq!(command.program, OsString::from(pwsh));
        assert!(command.args.is_empty() && command.raw_args.is_empty());
        assert_eq!(argv, pwsh);
        // a bare name is a program for the PATH search, whole
        assert_eq!(
            windows_shell("cmd.exe", None, is_file).0.program,
            OsString::from("cmd.exe")
        );
        // the config's `command` with arguments is split as Ghostty splits it
        let (command, argv) = windows_shell("pwsh  -NoLogo\t-NoProfile", None, is_file);
        assert_eq!(command.program, OsString::from("pwsh"));
        assert_eq!(command.args, ["-NoLogo", "-NoProfile"].map(OsString::from));
        assert_eq!(argv, "pwsh  -NoLogo\t-NoProfile");
        // a relative file with a space or a directory resolves under the launch's directory, where the child runs
        let (command, _) = windows_shell("my shell.exe", Some(cwd), is_file);
        assert_eq!(command.program.as_os_str(), relative.as_os_str());
        let (command, argv) = windows_shell(r".\shell.exe", Some(cwd), is_file);
        assert_eq!(command.program.as_os_str(), dotted.as_os_str());
        assert_eq!(argv, r".\shell.exe");
        let (command, _) = windows_shell(r"bin\shell.exe", Some(cwd), is_file);
        assert_eq!(command.program.as_os_str(), nested.as_os_str());
        // not there, or no launch directory: as given
        let (command, _) = windows_shell(r"tools\shell.exe", Some(cwd), is_file);
        assert_eq!(command.program, OsString::from(r"tools\shell.exe"));
        let (command, _) = windows_shell(r".\shell.exe", None, is_file);
        assert_eq!(command.program, OsString::from(r".\shell.exe"));
        // a bare name stays the PATH search's, even with a file of that name in the directory
        let (command, _) = windows_shell("shell.exe", Some(cwd), is_file);
        assert_eq!(command.program, OsString::from("shell.exe"));
    }

    #[test]
    fn variables_replace_every_spelling_where_names_ignore_case() {
        let mut folded = Vars::folding(true);
        folded.set("Path", r"C:\host-tools".to_owned());
        folded.set("PATH", r"C:\launch-tools".to_owned());
        assert_eq!(
            folded.set.iter().collect::<Vec<_>>(),
            [(&"PATH".to_owned(), &r"C:\launch-tools".to_owned())],
            "the later layer wins, in its own spelling"
        );
        folded.remove("path");
        assert!(folded.set.is_empty());
        assert_eq!(folded.removed, ["path"]);
        folded.set("PATH", "again".to_owned());
        assert!(folded.removed.is_empty(), "a later set undoes the removal");
        folded.remove("Path");
        folded.remove("PATH");
        assert_eq!(folded.removed, ["PATH"], "one removal per name");
        // where names keep their case (Unix), spellings are distinct variables
        let mut exact = Vars::folding(false);
        exact.set("Path", "a".to_owned());
        exact.set("PATH", "b".to_owned());
        exact.remove("path");
        assert_eq!(exact.set.len(), 2);
        assert_eq!(exact.removed, ["path"]);
    }

    /// Windows' forms through `prepare`: `cmd /D /S /C` for a command line, the login shell run directly, no POSIX
    /// shell integration, `;` in `PATH`.
    #[cfg(windows)]
    #[test]
    fn windows_runs_commands_through_cmd_and_the_login_shell_directly() {
        let resources = bash_resources();
        let shell_path = std::env::current_exe().unwrap();
        let host = Host {
            shell: shell_path.to_string_lossy().into_owned(),
            shell_integration: ShellIntegration::Bash,
            ..host(Some(resources.path().to_str().unwrap()))
        };
        let env = MapEnv::new()
            .with("ComSpec", r"C:\Windows\system32\cmd.exe")
            .with("PATH", r"C:\Windows");
        let command = Launch {
            command: Some("echo hi".to_owned()),
            ..Launch::default()
        };
        let prepared = host.prepare(&command, &env);
        assert_eq!(prepared.command.program, OsString::from(r"C:\Windows\system32\cmd.exe"));
        assert_eq!(prepared.command.raw_args, [OsString::from("\"echo hi\"")]);
        let ExitPolicy::Launched { argv, .. } = &prepared.exit else {
            panic!("{:?}", prepared.exit);
        };
        assert_eq!(argv, r#"C:\Windows\system32\cmd.exe /D /S /C "echo hi""#);
        assert_eq!(set(&prepared)["PATH"], r"C:\Windows;/opt/ngterm/bin");
        // without `ComSpec`, plain `cmd.exe` from `PATH`
        let plain = host.prepare(&command, &MapEnv::new());
        assert_eq!(plain.command.program, OsString::from("cmd.exe"));
        // the login shell: the file itself, nothing injected
        let shell = host.prepare(&Launch::default(), &env);
        assert_eq!(shell.command.program, shell_path.into_os_string());
        assert!(shell.command.args.is_empty());
        assert!(
            !set(&shell).contains_key("ENV"),
            "no POSIX shell integration on Windows"
        );
    }

    /// Windows' environment and home through `prepare`: names ignore case, so the launch's `PATH` wins over the
    /// config's `Path` and a removed `path` takes `PATH` along; `home` without `HOME` is the profile directory; no
    /// system resources directory.
    #[cfg(windows)]
    #[test]
    fn windows_variables_ignore_case_and_the_home_is_the_profile_directory() {
        let env = MapEnv::new().with("PATH", r"C:\Windows");
        let layered = Host {
            env: vec![("Path".to_owned(), r"C:\host-tools".to_owned())],
            ..host(None)
        };
        let launch = Launch {
            env: vec![("PATH".to_owned(), r"C:\launch-tools".to_owned())],
            ..Launch::default()
        };
        let vars = set(&layered.prepare(&launch, &env));
        assert_eq!(vars["PATH"], r"C:\launch-tools");
        assert!(!vars.contains_key("Path"));
        let removing = Launch {
            env_remove: vec!["path".to_owned()],
            ..Launch::default()
        };
        let prepared = layered.prepare(&removing, &env);
        assert!(!set(&prepared).keys().any(|key| key.eq_ignore_ascii_case("path")));
        assert!(removed(&prepared).contains(&"path".to_owned()));
        // `working-directory = home` without `HOME`: the user's own home, which is the profile directory
        let profile = Env::passwd_home(&ProcessEnv).expect("the profile directory");
        if let Some(user_profile) = std::env::var_os("USERPROFILE") {
            assert_eq!(profile, PathBuf::from(user_profile));
        }
        let home = host_of("working-directory = home\n", None)
            .prepare(&Launch::default(), &MapEnv::new().with_passwd_home(&profile));
        assert_eq!(home.command.cwd, Some(profile));
        // and no system resources directory
        assert!(resource_candidates(None).is_empty());
    }

    #[test]
    fn a_seed_runs_with_its_variables_over_the_panes() {
        let seed = LaunchSeed {
            command: Some("ngterm-session attach x".to_owned()),
            initial_input: Some("ls\n".to_owned()),
            wait_after_command: false,
            env: vec![
                ("SHELL".to_owned(), "/bin/zsh".to_owned()),
                ("NGTERM_LIVE_DIR".to_owned(), "/d".to_owned()),
            ],
            live: true,
            attach: Some(LiveAttach {
                executable: "ngterm-session".to_owned(),
                session: "x".to_owned(),
                argv: Vec::new(),
            }),
        };
        let pane_env = vec![
            ("NGTERM_PANE_ID".to_owned(), "P".to_owned()),
            ("SHELL".to_owned(), "/bin/sh".to_owned()),
        ];
        let launch = Launch::from_seed(&seed, Some(PathBuf::from("/w")), &pane_env);
        assert_eq!(launch.command.as_deref(), Some("ngterm-session attach x"));
        assert_eq!(launch.initial_input.as_deref(), Some("ls\n"));
        assert_eq!(launch.working_directory, Some(PathBuf::from("/w")));
        assert_eq!(launch.live, seed.attach, "the attach as data, run in process");
        assert_eq!(
            launch.env,
            [
                ("NGTERM_PANE_ID".to_owned(), "P".to_owned()),
                ("SHELL".to_owned(), "/bin/zsh".to_owned()),
                ("NGTERM_LIVE_DIR".to_owned(), "/d".to_owned()),
            ]
        );
    }

    // Unix only: POSIX absolute paths (Windows' need a drive) and the system package's `/usr/share/ghostty`, which
    // Windows has no counterpart of (`windows_runs_commands_through_cmd_and_the_login_shell_directly` checks there is
    // none)
    #[cfg(unix)]
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
            resource_candidates(Some(Path::new("/opt/ngterm/bin/ngterm"))),
            [
                PathBuf::from("/opt/ngterm/share/ghostty"),
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
        assert_eq!(host.shell, DEFAULT_SHELL);
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
