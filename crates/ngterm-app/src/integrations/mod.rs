//! The agent integrations (upstream's `SkillInstaller`, `AgentHooksInstaller` and `CLIInstaller`, `control-api.md` >
//! Installation and agent integrations): the agent skill into each coding agent's skills directory, the
//! agent-status hooks package with its Claude Code, Codex, Pi, `OpenCode` and shell wiring, and `ngtermctl` onto the
//! `PATH`. Each installer first plans from what it finds on disk (which agents are present, what is already
//! installed, what belongs to the user), then applies the plan and reports upstream's result text. The Help menu and
//! the first-run welcome run them from each frontend's own UI glue.
//!
//! Linux install paths, under this build's own names so that nothing lands on an agterm install's: the hooks package
//! goes to `~/.config/ngterm/agent-status` (an install from before the rename used `~/.config/agterm/agent-status`,
//! which a reinstall migrates away from: `hooks::legacy_script_dir`), the skill to the agents' `skills/ngterm`, and
//! the bundled `ngtermctl` (upstream's `agtermctl`) is linked into `~/.local/bin`, the per-user executables directory
//! of the XDG Base Directory spec, instead of an admin-owned `/usr/local/bin`. The
//! agents' own configs live where upstream puts them.
//!
//! The payloads under the repository's `resources/` are upstream v0.36.0's `agterm/Resources/agent-status` and
//! `plugins/agterm/skills/agterm` (MIT, `resources/LICENSE.upstream`), at `resources/agent-status` and
//! `resources/skills/ngterm` (the skill's directory renamed with the skill), with these deviations:
//!
//! - the Codex adapter reads the hook payload with `python3` or `jq` where macOS has `plutil`;
//! - the Claude adapter reads a `completed` call's payload and reports a `Stop` whose `background_tasks` lists work in
//!   flight as `active --blink` (upstream's goes `completed` at once), and the installer adds a sixth Claude hook,
//!   `SessionEnd` → `completed --auto-reset`, so that claim cannot outlive the agent ([`hooks_config`]);
//! - the package adds a restore adapter, `ngterm-agent-restore.sh`, which upstream has no counterpart for (it ships the
//!   idea as its `claude-session-resume` cookbook recipe and the `SessionStart`-pin pattern in its skill): the
//!   installer wires it to Claude Code's `SessionStart` and `SessionEnd` only (Codex gets no restore hooks), and it
//!   keeps the pane's `session restore` pin on the live conversation (`claude --resume <id>`), clearing it on a
//!   deliberate exit, so a rerun launch reattaches each pane to its latest Claude Code conversation; the script's
//!   header holds what it pins, what it skips and why (`build/patches/agent-status/0003-restore-adapter.patch`), and
//!   the skill tells agents not to hand-write that hook for Claude Code
//!   (`build/patches/skill-ngterm/0005-restore-adapter.patch`);
//! - every script and plugin calls `ngtermctl` where upstream's calls `agtermctl` (the wrappers' default, their
//!   comments and the shell integration's);
//! - the agent-status package is under this build's names: its wrappers, Pi extension and `OpenCode` plugins are
//!   `ngterm-*` (`ngterm-agent-status.sh`, `ngterm-claude-status.sh`, `ngterm-codex-status.sh`, `ngterm-status.ts`,
//!   `ngterm-status.js`, `ngterm-v2/tui.js`, whose plugin id is `ngterm.status`), their ownership markers and the rc
//!   and Codex block markers name ngterm (`// ngterm-pi-status-extension`, `# >>> ngterm agent-status >>>`, …), and
//!   the variables they read and set are `NGTERM_*` (`NGTERM_SESSION_ID`, `NGTERM_PANE_ID`, `NGTERM_SOCKET`,
//!   `NGTERMCTL`, `NGTERM_AGENT_BIN`, `NGTERM_AGENT_RE`, …); a reinstall moves an install under the old names onto
//!   these ([`hooks::Leftovers`]);
//! - the Pi extension and both `OpenCode` plugins call the wrapper in `~/.config/ngterm/agent-status`;
//! - the skill speaks of the `live` group and the attach contract's `NGTERM_LIVE_*` variables where upstream's names
//!   `zmx`, and it is named `ngterm` (frontmatter `name`, a description that also triggers on ngterm and `ngtermctl`),
//!   its text, examples and `show-image.sh` running `ngtermctl` wherever upstream runs `agtermctl`, the far side of a
//!   remote attach included (`remote::tree::REMOTE_CLI`), and the rest of it is under this build's identity too (its
//!   prose, `NGTERM_*` variables, HTML bridge, installed file names, docs and bug links: `0004-ngterm-identity.patch`):
//!   all are patches over upstream's tree (`build/patches/skill-ngterm`).

pub mod cli;
pub mod files;
pub mod hooks;
pub mod hooks_config;
pub mod opencode;
pub mod skill;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ngterm_core::paths::{self, ProcessEnv};

/// Where the installers read from and write to: the home they install into, the bundled resources and `ngtermctl`,
/// and the environment the `OpenCode` probe runs with.
#[derive(Debug, Clone)]
pub struct Environment {
    pub home: PathBuf,
    /// The bundled agent-status package (`share/ngterm/agent-status`).
    pub agent_status: Option<PathBuf>,
    /// The bundled skill directory (`share/ngterm/skills/ngterm`).
    pub skill: Option<PathBuf>,
    /// The bundled `ngtermctl`, the binary's sibling.
    pub bundled_cli: Option<PathBuf>,
    /// The process environment, for the `OpenCode` probe and the `PATH` check.
    pub vars: HashMap<String, String>,
}

/// An installer could not run at all (upstream's `InstallError`), shown as "Install Failed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallError(pub String);

impl Environment {
    /// The running app's: the user's home, the resources staged beside the binary (or this checkout's, in a dev or
    /// test build), its `ngtermctl`. `None` without a home directory.
    #[must_use]
    pub fn process() -> Option<Self> {
        let home = paths::home_dir(&ProcessEnv)?;
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf));
        let resource = |relative: &str| resource_dir(exe_dir.as_deref(), relative);
        Some(Self {
            home,
            agent_status: resource("agent-status"),
            skill: resource("skills/ngterm"),
            bundled_cli: exe_dir
                .map(|dir| dir.join(cli::TOOL_NAME))
                .filter(|tool| tool.is_file()),
            vars: std::env::vars().collect(),
        })
    }

    /// The home as text, for the paths the transforms build.
    #[must_use]
    pub fn home_str(&self) -> String {
        self.home.to_string_lossy().into_owned()
    }
}

/// A bundled resource: `<bin>/../share/ngterm/<relative>` in a staged build, else this checkout's `resources/`.
fn resource_dir(exe_dir: Option<&Path>, relative: &str) -> Option<PathBuf> {
    let staged = exe_dir
        .and_then(Path::parent)
        .map(|root| root.join("share/ngterm").join(relative));
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../resources")
        .join(relative);
    staged.into_iter().chain([checkout]).find(|dir| dir.is_dir())
}

#[cfg(test)]
mod adapter_tests;
#[cfg(test)]
mod restore_tests;
#[cfg(test)]
mod tests;
