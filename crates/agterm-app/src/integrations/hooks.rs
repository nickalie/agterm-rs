//! The agent-status hooks installer (upstream's `AgentHooksInstaller.swift`): the bundled package into
//! `~/.config/agterm/agent-status` with the bundled `agtermctl` baked into its wrappers, a marker-guarded `source` line
//! in `~/.zshrc`, `~/.bashrc` and (when fish is set up) `config.fish`, the Claude Code hooks merged into
//! `~/.claude/settings.json`, the Codex hooks into `~/.codex/config.toml`, and, where each agent is set up, Pi's
//! extension and `OpenCode`'s plugin. Claude and Codex configs get a `.bak` first; every rewrite keeps a symlinked file's
//! link and its target's mode. Idempotent: a second run refreshes the baked path and changes nothing else.
//!
//! [`plan`] reads the home and decides every step (what each agent has, what is current, what is the user's) without
//! writing; [`apply`] carries the plan out and reports upstream's outcomes.

use std::fs;
use std::path::{Path, PathBuf};

use super::files::{self, Unreadable};
use super::hooks_config::{self as config, CodexMerge};
use super::opencode::{Probe, Version};
use super::{Environment, InstallError};

/// The package's destination under `home`.
#[must_use]
pub fn script_dir(home: &Path) -> PathBuf {
    home.join(".config/agterm/agent-status")
}

/// A file the install writes, with the previous contents its backup keeps, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWrite {
    pub path: PathBuf,
    pub contents: String,
    pub backup: Option<String>,
}

/// The Claude Code step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudePlan {
    Merge(FileWrite),
    Current,
    /// `settings.json` is unreadable, not valid JSON or has hooks of a shape the merge does not know: untouched.
    Skipped,
}

/// The Codex step (upstream's `CodexResult` before the write).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexPlan {
    Merge(FileWrite),
    Current,
    HooksExist,
    Unparseable,
    Unreadable,
    NoCodex,
}

/// The Pi step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PiPlan {
    Install(FileWrite),
    Current,
    UserOwned,
    Unreadable,
    NoPi,
}

/// The `OpenCode` step: a decided outcome, or the plugin to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCodePlan {
    Decided(OpenCodeResult),
    Install {
        version: Version,
        destination: PathBuf,
        /// `None` when the installed plugin is already current.
        contents: Option<String>,
        configuration_directory: PathBuf,
    },
}

/// Everything the install will do, decided before any write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HooksPlan {
    pub package: PathBuf,
    pub script_dir: PathBuf,
    /// The `agtermctl` baked into the wrappers; `None` keeps their `PATH` fallback.
    pub bake: Option<String>,
    pub claude: ClaudePlan,
    /// The rc files that gain the `source` block, with their new contents.
    pub shell_rcs: Vec<FileWrite>,
    /// The rc files that could not be read, left untouched.
    pub rcs_skipped: Vec<PathBuf>,
    pub codex: CodexPlan,
    pub pi: PiPlan,
    pub opencode: OpenCodePlan,
}

/// Upstream's `CodexResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexResult {
    Merged,
    AlreadyConfigured,
    HooksExist,
    Unparseable,
    Unreadable,
    NoCodex,
}

impl CodexResult {
    /// The user must act: add the block by hand or fix the file.
    #[must_use]
    pub fn is_warning(self) -> bool {
        matches!(self, Self::HooksExist | Self::Unparseable | Self::Unreadable)
    }

    /// The outcomes whose text sends the user to the docs for the block to add by hand.
    #[must_use]
    pub fn needs_manual_merge(self) -> bool {
        matches!(self, Self::HooksExist | Self::Unparseable)
    }
}

/// Upstream's `PiResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiResult {
    Installed,
    AlreadyConfigured,
    UserOwned,
    Unreadable,
    WriteFailed,
    NoPi,
}

impl PiResult {
    #[must_use]
    pub fn is_warning(self) -> bool {
        matches!(self, Self::UserOwned | Self::Unreadable | Self::WriteFailed)
    }
}

/// Upstream's `OpenCodeResult`; only the version-specific outcomes carry a version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCodeResult {
    Installed(Version, PathBuf),
    AlreadyConfigured(Version, PathBuf),
    UserOwned(Version, PathBuf),
    Unreadable(Version, PathBuf),
    WriteFailed(Version, PathBuf),
    NoOpenCode(PathBuf),
    Skipped,
    V2ConfigUnavailable,
    V1CleanupUserOwned(PathBuf),
    V1CleanupUnreadable(PathBuf),
    V1CleanupFailed(PathBuf),
}

impl OpenCodeResult {
    #[must_use]
    pub fn is_warning(&self) -> bool {
        !matches!(
            self,
            Self::Installed(..) | Self::AlreadyConfigured(..) | Self::NoOpenCode(_) | Self::Skipped
        )
    }
}

/// What the install did, for the result alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HooksOutcome {
    pub script_dir: PathBuf,
    pub settings_skipped: bool,
    /// The rc files that could not be read and did not get the `source` line.
    pub rcs_skipped: Vec<PathBuf>,
    pub codex: CodexResult,
    pub pi: PiResult,
    pub opencode: OpenCodeResult,
}

impl HooksOutcome {
    #[must_use]
    pub fn is_warning(&self) -> bool {
        self.settings_skipped
            || !self.rcs_skipped.is_empty()
            || self.codex.is_warning()
            || self.pi.is_warning()
            || self.opencode.is_warning()
    }

    /// The shell line: upstream's, plus the rc files left untouched because they could not be read.
    fn shell_text(&self) -> String {
        let added = "The source line was added to ~/.zshrc, ~/.bashrc (and ~/.config/fish/config.fish if fish is \
                     installed).";
        if self.rcs_skipped.is_empty() {
            return added.to_owned();
        }
        let skipped: Vec<String> = self.rcs_skipped.iter().map(|path| path.display().to_string()).collect();
        format!(
            "{added} Except {}: it couldn't be read, so agterm left it untouched; add the source line there by hand.",
            skipped.join(", ")
        )
    }

    #[must_use]
    pub fn title(&self) -> &'static str {
        if self.is_warning() {
            "Agent Status Hooks Installed — with a warning"
        } else {
            "Agent Status Hooks Installed"
        }
    }

    /// The alert text, calling out anything an integration could not safely update and left alone (upstream's
    /// `successText`).
    #[must_use]
    pub fn text(&self) -> String {
        let claude = if self.settings_skipped {
            "Your ~/.claude/settings.json couldn't be safely read or merged, so the Claude Code hooks were NOT added \
             (the file was left untouched). It is unreadable, not valid JSON, or has a hooks entry in a shape we don't \
             recognize. Fix it and run this again, or add the hooks manually."
        } else {
            "Claude Code hooks merged into ~/.claude/settings.json."
        };
        format!(
            "Scripts installed to {}.\n{claude}\n{}\n{}\n{}\n{}\n\nOpen a new terminal for the shell integration to take \
             effect.",
            self.script_dir.display(),
            codex_text(self.codex),
            pi_text(self.pi),
            opencode_text(&self.opencode),
            self.shell_text(),
        )
    }
}

/// The docs anchor for a manual Codex merge, opened by the alert's second button.
pub const CODEX_MANUAL_DOCS_URL: &str = "https://agterm.com/docs#codex-hooks-manual";

/// The Codex line of the alert: one line each, never the generated block (upstream's `codexText`).
#[must_use]
pub fn codex_text(codex: CodexResult) -> String {
    let approve = "Run /hooks in Codex to review and approve them before they take effect.";
    let manual =
        "See the Add Codex hooks by hand section of the agterm docs for the block to add, then run /hooks in Codex.";
    match codex {
        CodexResult::Merged => format!(
            "Codex lifecycle hooks merged into ~/.codex/config.toml (any old codex-notify.sh notify line was \
             removed). {approve}"
        ),
        CodexResult::AlreadyConfigured => {
            format!("Codex lifecycle hooks are already present in ~/.codex/config.toml. {approve}")
        }
        CodexResult::HooksExist => {
            format!("Your ~/.codex/config.toml already defines its own hooks, so agterm left it untouched. {manual}")
        }
        CodexResult::Unparseable => format!(
            "Your ~/.codex/config.toml isn't valid TOML, so agterm left it untouched. Fix it and run this again. \
             {manual}"
        ),
        CodexResult::Unreadable => {
            "Your ~/.codex/config.toml exists but couldn't be read, so agterm left it untouched.".to_owned()
        }
        CodexResult::NoCodex => {
            "No ~/.codex found, so Codex hooks were skipped. Install Codex, then run this again.".to_owned()
        }
    }
}

/// The Pi line of the alert (upstream's `piText`).
#[must_use]
pub fn pi_text(pi: PiResult) -> &'static str {
    match pi {
        PiResult::Installed => {
            "Pi lifecycle extension installed to ~/.pi/agent/extensions/agterm-status.ts. Restart Pi or run /reload."
        }
        PiResult::AlreadyConfigured => {
            "Pi lifecycle extension is already current at ~/.pi/agent/extensions/agterm-status.ts."
        }
        PiResult::UserOwned => "~/.pi/agent/extensions/agterm-status.ts is user-owned, so agterm left it untouched.",
        PiResult::Unreadable => {
            "~/.pi/agent/extensions/agterm-status.ts exists but could not be read, so agterm left it untouched."
        }
        PiResult::WriteFailed => {
            "Pi's lifecycle extension couldn't be written to ~/.pi/agent/extensions/ (check that directory's \
             permissions), so it was skipped."
        }
        PiResult::NoPi => {
            "No ~/.pi/agent found, so Pi's lifecycle extension was skipped. Start Pi once, then run this again."
        }
    }
}

/// The `OpenCode` line of the alert (upstream's `opencodeText`).
#[must_use]
pub fn opencode_text(opencode: &OpenCodeResult) -> String {
    use OpenCodeResult as R;
    match opencode {
        R::Installed(version, path) => format!(
            "OpenCode {} status plugin installed to {}. Restart OpenCode.",
            version.raw(),
            path.display()
        ),
        R::AlreadyConfigured(version, path) => format!(
            "OpenCode {} status plugin is already current at {}.",
            version.raw(),
            path.display()
        ),
        R::UserOwned(version, path) => format!(
            "{} is user-owned, so the OpenCode {} plugin was left untouched.",
            path.display(),
            version.raw()
        ),
        R::Unreadable(version, path) => format!(
            "{} could not be read, so the OpenCode {} plugin was left untouched.",
            path.display(),
            version.raw()
        ),
        R::WriteFailed(version, path) => format!(
            "OpenCode {} plugin couldn't be written to {} (check permissions), so it was skipped.",
            version.raw(),
            path.display()
        ),
        R::NoOpenCode(directory) => format!(
            "No {} found, so the OpenCode plugin was skipped. Start OpenCode once, then run this again. Coarse shell \
             detection for opencode is off by default.",
            directory.display()
        ),
        R::Skipped => "OpenCode status plugin installation was skipped.".to_owned(),
        R::V2ConfigUnavailable => "OpenCode v2 installation was skipped because its configuration directory could not \
                                   be determined from your login shell. Check shell startup and OPENCODE_CONFIG_DIR, \
                                   then run this again."
            .to_owned(),
        R::V1CleanupUserOwned(path) => format!(
            "OpenCode v2 status plugin is installed, but {} is user-owned and was left untouched.",
            path.display()
        ),
        R::V1CleanupUnreadable(path) => format!(
            "OpenCode v2 status plugin is installed, but {} could not be read and was left untouched.",
            path.display()
        ),
        R::V1CleanupFailed(path) => format!(
            "OpenCode v2 status plugin is installed, but the v1 plugin at {} could not be safely removed. Check \
             permissions and run this again.",
            path.display()
        ),
    }
}

/// The chooser shown when the probe could not tell the `OpenCode` version (upstream's `makeOpenCodeVersionAlert`).
pub const OPENCODE_CHOOSER_TITLE: &str = "Choose OpenCode Version";
pub const OPENCODE_CHOOSER_TEXT: &str = "Could not determine the OpenCode version in your login shell (opencode \
    --version). Choose the version you use, or skip OpenCode and install the other hooks.";

/// Where the `OpenCode` step stands after the probe: decided, a version to install, or the user must pick one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCodeStart {
    Decided(OpenCodeResult),
    Install(Version),
    Ask,
}

/// The default `OpenCode` 1 configuration directory under `home`.
fn legacy_opencode_directory(home: &Path) -> PathBuf {
    home.join(".config/opencode")
}

/// Upstream's decision before the chooser: no probe and no default config, or no version and neither config, skips
/// without asking; a detected version installs; otherwise the user picks.
#[must_use]
pub fn opencode_start(home: &Path, probe: Option<&Probe>) -> OpenCodeStart {
    let legacy = legacy_opencode_directory(home);
    let Some(probe) = probe else {
        return if legacy.exists() {
            OpenCodeStart::Ask
        } else {
            OpenCodeStart::Decided(OpenCodeResult::NoOpenCode(legacy))
        };
    };
    if let Some(version) = probe.version {
        return OpenCodeStart::Install(version);
    }
    if !legacy.exists()
        && let Some(v2) = probe.v2_configuration_directory.as_deref().map(PathBuf::from)
        && !v2.exists()
    {
        return OpenCodeStart::Decided(OpenCodeResult::NoOpenCode(v2));
    }
    OpenCodeStart::Ask
}

/// The `OpenCode` step once the version is known (`None`: the user skipped `OpenCode`).
fn plan_opencode(
    home: &Path,
    package: &Path,
    version: Option<Version>,
    probe: Option<&Probe>,
) -> Result<OpenCodePlan, InstallError> {
    let Some(version) = version else {
        return Ok(OpenCodePlan::Decided(OpenCodeResult::Skipped));
    };
    let configuration_directory = match version {
        Version::V1 => legacy_opencode_directory(home),
        Version::V2 => match probe.and_then(|probe| probe.v2_configuration_directory.as_deref()) {
            Some(directory) => PathBuf::from(directory),
            None => return Ok(OpenCodePlan::Decided(OpenCodeResult::V2ConfigUnavailable)),
        },
    };
    if !configuration_directory.exists() {
        return Ok(OpenCodePlan::Decided(OpenCodeResult::NoOpenCode(
            configuration_directory,
        )));
    }
    let source = bundled_text(
        &package.join(version.relative_path()),
        version.marker(),
        &format!("The OpenCode {} status plugin", version.raw()),
    )?;
    let destination = version.plugin_path(&configuration_directory);
    let existing = match files::read_existing(&destination) {
        Ok(existing) => existing,
        Err(Unreadable) => return Ok(OpenCodePlan::Decided(OpenCodeResult::Unreadable(version, destination))),
    };
    if !version.may_overwrite(existing.is_some(), existing.as_deref()) {
        return Ok(OpenCodePlan::Decided(OpenCodeResult::UserOwned(version, destination)));
    }
    let contents = (existing.as_deref() != Some(source.as_str())).then_some(source);
    Ok(OpenCodePlan::Install {
        version,
        destination,
        contents,
        configuration_directory,
    })
}

/// A bundled text resource that must carry its ownership marker.
fn bundled_text(path: &Path, marker: &str, what: &str) -> Result<String, InstallError> {
    let text = fs::read_to_string(path).map_err(|_| InstallError(format!("{what} is not bundled in this build.")))?;
    if !text.contains(marker) {
        let noun = what.strip_prefix("The ").unwrap_or(what);
        return Err(InstallError(format!(
            "The bundled {noun} is missing its ownership marker."
        )));
    }
    Ok(text)
}

/// Plans the whole install over `env`'s home, without writing. `opencode` is where the `OpenCode` step stands after the
/// probe and the chooser: an [`OpenCodeStart::Ask`] nobody answered counts as skipped.
///
/// # Errors
///
/// [`InstallError`] when the bundled package is missing or a step cannot be planned.
pub fn plan(env: &Environment, opencode: &OpenCodeStart, probe: Option<&Probe>) -> Result<HooksPlan, InstallError> {
    let package = env
        .agent_status
        .clone()
        .filter(|package| package.is_dir())
        .ok_or_else(|| InstallError("The agent-status scripts are not bundled in this build.".to_owned()))?;
    let script_dir = script_dir(&env.home);
    let dir = script_dir.to_string_lossy().into_owned();
    let opencode = match opencode {
        OpenCodeStart::Decided(result) => OpenCodePlan::Decided(result.clone()),
        OpenCodeStart::Install(version) => plan_opencode(&env.home, &package, Some(*version), probe)?,
        OpenCodeStart::Ask => plan_opencode(&env.home, &package, None, probe)?,
    };
    let (shell_rcs, rcs_skipped) = plan_shell_rcs(&env.home, &dir);
    Ok(HooksPlan {
        bake: env.bundled_cli.as_ref().map(|tool| tool.to_string_lossy().into_owned()),
        claude: plan_claude(&env.home, &dir),
        shell_rcs,
        rcs_skipped,
        codex: plan_codex(&env.home, &dir),
        pi: plan_pi(&env.home, &package)?,
        opencode,
        package,
        script_dir,
    })
}

fn plan_claude(home: &Path, script_dir: &str) -> ClaudePlan {
    let path = home.join(".claude/settings.json");
    let Ok(existing) = files::read_existing(&path) else {
        return ClaudePlan::Skipped;
    };
    match config::merge_claude_settings(existing.as_deref(), script_dir) {
        Err(config::MalformedSettings) => ClaudePlan::Skipped,
        Ok((_, false)) => ClaudePlan::Current,
        Ok((contents, true)) => ClaudePlan::Merge(FileWrite {
            path,
            contents,
            backup: existing,
        }),
    }
}

/// The rc files gaining the block: `~/.zshrc` and `~/.bashrc` always, `config.fish` only with `~/.config/fish`
/// present. An unreadable rc file is left untouched rather than replaced, and returned second for the alert.
fn plan_shell_rcs(home: &Path, script_dir: &str) -> (Vec<FileWrite>, Vec<PathBuf>) {
    let mut writes = Vec::new();
    let mut skipped = Vec::new();
    for (name, script) in [
        (".zshrc", config::INTEGRATION_RELATIVE_PATH),
        (".bashrc", config::INTEGRATION_RELATIVE_PATH),
        (".config/fish/config.fish", config::FISH_INTEGRATION_RELATIVE_PATH),
    ] {
        let path = home.join(name);
        if script == config::FISH_INTEGRATION_RELATIVE_PATH && !path.parent().is_some_and(Path::exists) {
            continue;
        }
        let Ok(existing) = files::read_existing(&path) else {
            tracing::warn!(path = %path.display(), "agent hooks: the rc file is unreadable, left untouched");
            skipped.push(path);
            continue;
        };
        let (contents, changed) = config::append_shell_rc(existing.as_deref().unwrap_or_default(), script_dir, script);
        if changed {
            writes.push(FileWrite {
                path,
                contents,
                backup: None,
            });
        }
    }
    (writes, skipped)
}

fn plan_codex(home: &Path, script_dir: &str) -> CodexPlan {
    let codex = home.join(".codex");
    if !codex.exists() {
        return CodexPlan::NoCodex;
    }
    let path = codex.join("config.toml");
    let Ok(existing) = files::read_existing(&path) else {
        return CodexPlan::Unreadable;
    };
    match config::merge_codex_config(existing.as_deref().unwrap_or_default(), script_dir) {
        CodexMerge::Unchanged => CodexPlan::Current,
        CodexMerge::HooksExist => CodexPlan::HooksExist,
        CodexMerge::Unparseable => CodexPlan::Unparseable,
        CodexMerge::Merged(contents) => CodexPlan::Merge(FileWrite {
            path,
            contents,
            backup: existing.filter(|text| !text.is_empty()),
        }),
    }
}

fn plan_pi(home: &Path, package: &Path) -> Result<PiPlan, InstallError> {
    if !home.join(".pi/agent").exists() {
        return Ok(PiPlan::NoPi);
    }
    let source = bundled_text(
        &package.join(config::PI_EXTENSION_RELATIVE_PATH),
        config::PI_EXTENSION_MARKER,
        "The Pi status extension",
    )?;
    let path = PathBuf::from(config::pi_extension_path(&home.to_string_lossy()));
    let Ok(existing) = files::read_existing(&path) else {
        return Ok(PiPlan::Unreadable);
    };
    if !config::may_overwrite_pi_extension(existing.is_some(), existing.as_deref()) {
        return Ok(PiPlan::UserOwned);
    }
    if existing.as_deref() == Some(source.as_str()) {
        return Ok(PiPlan::Current);
    }
    Ok(PiPlan::Install(FileWrite {
        path,
        contents: source,
        backup: None,
    }))
}

/// Carries `plan` out, every step regardless of an earlier one's outcome; a failed copy, bake or config write stops
/// the install as upstream's thrown error does.
///
/// # Errors
///
/// [`InstallError`] naming the copy, bake or config write that failed.
pub fn apply(plan: &HooksPlan) -> Result<HooksOutcome, InstallError> {
    install_package(plan)?;
    let settings_skipped = match &plan.claude {
        ClaudePlan::Merge(write) => {
            write_with_backup(write, true)?;
            false
        }
        ClaudePlan::Current => false,
        ClaudePlan::Skipped => true,
    };
    for rc in &plan.shell_rcs {
        write_with_backup(rc, true)?;
    }
    let codex = apply_codex(&plan.codex)?;
    Ok(HooksOutcome {
        script_dir: plan.script_dir.clone(),
        settings_skipped,
        rcs_skipped: plan.rcs_skipped.clone(),
        codex,
        pi: apply_pi(&plan.pi),
        opencode: apply_opencode(&plan.opencode),
    })
}

/// Copies the package over the script directory and bakes `agtermctl` into the generic and Codex wrappers (the
/// Claude adapter execs the generic one, so it needs none).
fn install_package(plan: &HooksPlan) -> Result<(), InstallError> {
    files::copy_tree(&plan.package, &plan.script_dir).map_err(failed("install the scripts into", &plan.script_dir))?;
    let Some(tool) = &plan.bake else {
        return Ok(());
    };
    for name in [config::WRAPPER_NAME, config::CODEX_WRAPPER_NAME] {
        let wrapper = plan.script_dir.join(name);
        let original = fs::read_to_string(&wrapper).map_err(failed("read", &wrapper))?;
        files::write_preserving_symlink(&config::bake_agtermctl_path(&original, tool), &wrapper)
            .map_err(failed("write", &wrapper))?;
    }
    Ok(())
}

/// The failure of `action` on `path`, naming both: the Install Failed alert says which file stopped the install.
fn failed(action: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> InstallError {
    let path = path.display().to_string();
    move |err| InstallError(format!("Could not {action} {path}: {err}"))
}

/// Writes `write` through a symlink with the target's mode, backing the previous contents up beside the link (never
/// into a dotfiles directory the link resolves into) with that same mode.
fn write_with_backup(write: &FileWrite, create_parent: bool) -> Result<(), InstallError> {
    if create_parent && let Some(parent) = write.path.parent() {
        fs::create_dir_all(parent).map_err(failed("create", parent))?;
    }
    if let Some(previous) = &write.backup {
        let mode = files::posix_mode(&files::write_target(&write.path));
        let backup = PathBuf::from(config::backup_path(&write.path.to_string_lossy()));
        files::write_file(previous, &backup, mode).map_err(failed("write", &backup))?;
    }
    files::write_preserving_symlink(&write.contents, &write.path).map_err(failed("write", &write.path))
}

fn apply_codex(codex: &CodexPlan) -> Result<CodexResult, InstallError> {
    Ok(match codex {
        CodexPlan::Merge(write) => {
            write_with_backup(write, false)?;
            CodexResult::Merged
        }
        CodexPlan::Current => CodexResult::AlreadyConfigured,
        CodexPlan::HooksExist => CodexResult::HooksExist,
        CodexPlan::Unparseable => CodexResult::Unparseable,
        CodexPlan::Unreadable => CodexResult::Unreadable,
        CodexPlan::NoCodex => CodexResult::NoCodex,
    })
}

/// A failed write degrades to a warning, as upstream's: the other steps already ran.
fn apply_pi(pi: &PiPlan) -> PiResult {
    match pi {
        PiPlan::Install(write) => match write_with_backup(write, true) {
            Ok(()) => PiResult::Installed,
            Err(_) => PiResult::WriteFailed,
        },
        PiPlan::Current => PiResult::AlreadyConfigured,
        PiPlan::UserOwned => PiResult::UserOwned,
        PiPlan::Unreadable => PiResult::Unreadable,
        PiPlan::NoPi => PiResult::NoPi,
    }
}

fn apply_opencode(opencode: &OpenCodePlan) -> OpenCodeResult {
    let (version, destination, contents, configuration_directory) = match opencode {
        OpenCodePlan::Decided(result) => return result.clone(),
        OpenCodePlan::Install {
            version,
            destination,
            contents,
            configuration_directory,
        } => (*version, destination, contents, configuration_directory),
    };
    if let Some(contents) = contents {
        let write = FileWrite {
            path: destination.clone(),
            contents: contents.clone(),
            backup: None,
        };
        if write_with_backup(&write, true).is_err() {
            return OpenCodeResult::WriteFailed(version, destination.clone());
        }
    }
    if version == Version::V2
        && let Some(warning) = remove_managed_v1_plugin(configuration_directory)
    {
        return warning;
    }
    if contents.is_some() {
        OpenCodeResult::Installed(version, destination.clone())
    } else {
        OpenCodeResult::AlreadyConfigured(version, destination.clone())
    }
}

/// `OpenCode` 2 would load both plugins, so a v2 install removes the managed v1 plugin beside it: the entry itself
/// only, never a symlink's target; an unmarked or unreadable one is left with a warning.
fn remove_managed_v1_plugin(configuration_directory: &Path) -> Option<OpenCodeResult> {
    let destination = Version::V1.plugin_path(configuration_directory);
    match fs::symlink_metadata(&destination) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(OpenCodeResult::V1CleanupUnreadable(destination)),
        Ok(_) => {}
    }
    let Ok(existing) = fs::read_to_string(&destination) else {
        return Some(OpenCodeResult::V1CleanupUnreadable(destination));
    };
    if !Version::V1.may_overwrite(true, Some(&existing)) {
        return Some(OpenCodeResult::V1CleanupUserOwned(destination));
    }
    fs::remove_file(&destination)
        .err()
        .map(|_| OpenCodeResult::V1CleanupFailed(destination))
}
