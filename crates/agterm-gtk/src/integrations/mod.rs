//! The agent integrations (upstream's `SkillInstaller`, `AgentHooksInstaller` and `CLIInstaller`, `control-api.md` >
//! Installation and agent integrations): the agent skill into each coding agent's skills directory, the
//! agent-status hooks package with its Claude Code, Codex, Pi, `OpenCode` and shell wiring, and `agtermctl` onto the
//! `PATH`. Each installer first plans from what it finds on disk (which agents are present, what is already
//! installed, what belongs to the user), then applies the plan and reports upstream's result text. The Help menu and
//! the first-run welcome run them (`ui.rs`).
//!
//! Linux install paths (the fork's `LinuxIntegrations` as the hint): the hooks package and the agents' configs live
//! where upstream puts them under the home directory; `agtermctl` is linked into `~/.local/bin`, the per-user
//! executables directory, instead of an admin-owned `/usr/local/bin`.
//!
//! The payloads under the repository's `resources/` are upstream v0.36.0's `agterm/Resources/agent-status` and
//! `plugins/agterm/skills/agterm` (MIT, `resources/LICENSE.upstream`), unchanged but for one piece of Linux glue: the
//! Codex adapter reads the hook payload with `python3` or `jq` where macOS has `plutil`.

pub mod cli;
pub mod files;
pub mod hooks;
pub mod hooks_config;
pub mod opencode;
pub mod skill;
pub mod ui;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use agterm_core::paths::{self, ProcessEnv};

/// Where the installers read from and write to: the home they install into, the bundled resources and `agtermctl`,
/// and the environment the `OpenCode` probe runs with.
#[derive(Debug, Clone)]
pub struct Environment {
    pub home: PathBuf,
    /// The bundled agent-status package (`share/agterm/agent-status`).
    pub agent_status: Option<PathBuf>,
    /// The bundled skill directory (`share/agterm/skills/agterm`).
    pub skill: Option<PathBuf>,
    /// The bundled `agtermctl`, the binary's sibling.
    pub bundled_cli: Option<PathBuf>,
    /// The process environment, for the `OpenCode` probe and the `PATH` check.
    pub vars: HashMap<String, String>,
}

/// An installer could not run at all (upstream's `InstallError`), shown as "Install Failed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallError(pub String);

impl Environment {
    /// The running app's: the user's home, the resources staged beside the binary (or this checkout's, in a dev or
    /// test build), its `agtermctl`. `None` without a home directory.
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
            skill: resource("skills/agterm"),
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

/// A bundled resource: `<bin>/../share/agterm/<relative>` in a staged build, else this checkout's `resources/`.
fn resource_dir(exe_dir: Option<&Path>, relative: &str) -> Option<PathBuf> {
    let staged = exe_dir
        .and_then(Path::parent)
        .map(|root| root.join("share/agterm").join(relative));
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../resources")
        .join(relative);
    staged.into_iter().chain([checkout]).find(|dir| dir.is_dir())
}

#[cfg(test)]
mod tests;
