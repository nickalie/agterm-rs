//! The agent skill installer (upstream's `SkillInstall.swift` and `SkillInstaller.swift`): the bundled skill into the
//! skills directory of every coding agent present, Claude Code (`~/.claude/skills/ngterm`) and Codex
//! (`~/.codex/skills/ngterm`), Claude Code's when neither is. Idempotent, and a same-named skill the user wrote (its
//! `SKILL.md` absent, unreadable or unmarked) is never overwritten.
//!
//! The skill is named `ngterm`, not upstream's `agterm`: the GTK fork installs its own `agterm` skill into the same
//! agents' directories, and that one drives the fork through its `agtermctl`. This build's skill drives it through
//! `ngtermctl` (`cli::TOOL_NAME`) and leaves the fork's alone.

use std::fs;
use std::path::{Path, PathBuf};

use super::files;
use super::{Environment, InstallError};

/// The skill's directory name under each agent's `skills/`, and its frontmatter `name`.
pub const SKILL_NAME: &str = "ngterm";

/// The sentinel in the bundled `SKILL.md` telling an ngterm-installed skill from the user's own.
pub const MARKER: &str = "<!-- ngterm-skill -->";

/// The sentinel this build's skill carried before the build took its own name, the one the fork's `agterm` skill
/// carries too: in a `SKILL.md` whose frontmatter names [`SKILL_NAME`], it marks an install of ours. A one-time
/// migration: remove it once no install from before the rename is left to migrate (the first reinstall past it has
/// run).
pub const LEGACY_MARKER: &str = "<!-- agterm-skill -->";

/// One destination: the agent's label and its `…/skills/ngterm` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub agent: &'static str,
    pub skill_directory: PathBuf,
}

/// The targets for the agents present, falling back to Claude Code's (the primary) when neither is, so the install
/// is never a no-op (upstream's `installTargets`).
#[must_use]
pub fn install_targets(home: &Path, claude_exists: bool, codex_exists: bool) -> Vec<Target> {
    let target = |agent, base: &str| Target {
        agent,
        skill_directory: home.join(base).join("skills").join(SKILL_NAME),
    };
    let mut targets = Vec::new();
    if claude_exists {
        targets.push(target("Claude Code", ".claude"));
    }
    if codex_exists {
        targets.push(target("Codex", ".codex"));
    }
    if targets.is_empty() {
        targets.push(target("Claude Code", ".claude"));
    }
    targets
}

/// Whether a destination may be replaced: absent, or holding a `SKILL.md` with the marker, or with the marker from
/// before the rename ([`LEGACY_MARKER`]) under a frontmatter naming this skill.
#[must_use]
pub fn may_overwrite(directory_exists: bool, existing_skill: Option<&str>) -> bool {
    !directory_exists
        || existing_skill
            .is_some_and(|text| text.contains(MARKER) || (text.contains(LEGACY_MARKER) && names_this_skill(text)))
}

/// Whether `text` opens with a frontmatter whose `name` is [`SKILL_NAME`].
fn names_this_skill(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("---\n") else {
        return false;
    };
    rest.lines()
        .take_while(|line| *line != "---")
        .any(|line| line.strip_prefix("name:").is_some_and(|name| name.trim() == SKILL_NAME))
}

/// What the install will do at one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetPlan {
    /// Copy the skill there; `fresh_skills_dir` when the agent's `skills/` does not exist yet.
    Install { target: Target, fresh_skills_dir: bool },
    /// A different `ngterm` skill is there: left untouched.
    UserOwned(Target),
}

/// The plan: the bundled source and one step per target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPlan {
    pub source: PathBuf,
    pub targets: Vec<TargetPlan>,
}

/// Plans the install over `env`'s home, without writing.
///
/// # Errors
///
/// [`InstallError`] when the build bundles no skill.
pub fn plan(env: &Environment) -> Result<SkillPlan, InstallError> {
    let source = env
        .skill
        .clone()
        .filter(|source| source.is_dir())
        .ok_or_else(|| InstallError("The agent skill is not bundled in this build.".to_owned()))?;
    let home = &env.home;
    let targets = install_targets(home, home.join(".claude").exists(), home.join(".codex").exists())
        .into_iter()
        .map(|target| {
            // lstat: a symlink there, dangling or not, counts as present, so the wipe never deletes the user's link
            let directory_exists = files::entry_exists(&target.skill_directory);
            let existing = fs::read_to_string(target.skill_directory.join("SKILL.md")).ok();
            if !may_overwrite(directory_exists, existing.as_deref()) {
                return TargetPlan::UserOwned(target);
            }
            let fresh_skills_dir = !target.skill_directory.parent().is_some_and(Path::exists);
            TargetPlan::Install {
                target,
                fresh_skills_dir,
            }
        })
        .collect();
    Ok(SkillPlan { source, targets })
}

/// What the install did (upstream's `Report`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillReport {
    pub installed: Vec<(&'static str, PathBuf)>,
    pub skipped: Vec<(&'static str, String)>,
    pub fresh_skills_dir: bool,
}

/// Carries `plan` out: each target's prior ngterm install is replaced by a fresh copy; a failure skips that target.
#[must_use]
pub fn apply(plan: &SkillPlan) -> SkillReport {
    let mut report = SkillReport::default();
    for step in &plan.targets {
        match step {
            TargetPlan::UserOwned(target) => report.skipped.push((
                target.agent,
                format!("a different '{SKILL_NAME}' skill is already there — left untouched"),
            )),
            TargetPlan::Install {
                target,
                fresh_skills_dir,
            } => match install_one(&plan.source, &target.skill_directory) {
                Ok(()) => {
                    report.fresh_skills_dir |= fresh_skills_dir;
                    report.installed.push((target.agent, target.skill_directory.clone()));
                }
                Err(err) => report.skipped.push((target.agent, err.to_string())),
            },
        }
    }
    report
}

fn install_one(source: &Path, destination: &Path) -> std::io::Result<()> {
    if let Some(skills) = destination.parent() {
        fs::create_dir_all(skills)?;
    }
    files::remove_entry(destination)?;
    files::copy_tree(source, destination)
}

impl SkillReport {
    /// Nothing installed, or a target skipped.
    #[must_use]
    pub fn is_warning(&self) -> bool {
        self.installed.is_empty() || !self.skipped.is_empty()
    }

    #[must_use]
    pub fn title(&self) -> &'static str {
        if self.installed.is_empty() {
            "Skill Not Installed"
        } else {
            "Agent Skill Installed"
        }
    }

    /// The alert text (upstream's `message`).
    #[must_use]
    pub fn text(&self) -> String {
        let mut lines = Vec::new();
        if self.installed.is_empty() {
            lines.push("The skill was not installed.".to_owned());
        } else {
            lines.push("Installed for:".to_owned());
            for (agent, path) in &self.installed {
                lines.push(format!("  • {agent}: {}", path.display()));
            }
            lines.push(String::new());
            lines.push(
                "The agent uses it automatically when you ask it to control ngterm. It drives the app via the \
                 ngtermctl CLI, so install that too if you haven't: Help ▸ Install Command Line Tool…"
                    .to_owned(),
            );
        }
        if !self.skipped.is_empty() {
            lines.push(String::new());
            lines.push("Skipped:".to_owned());
            for (agent, reason) in &self.skipped {
                lines.push(format!("  • {agent}: {reason}"));
            }
        }
        if self.fresh_skills_dir {
            lines.push(String::new());
            lines.push(
                "A skills directory was created for the first time — restart any running agent session so it picks \
                 up the new skill."
                    .to_owned(),
            );
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_cover_the_agents_present_and_fall_back_to_claude() {
        let home = Path::new("/h");
        let claude = Target {
            agent: "Claude Code",
            skill_directory: "/h/.claude/skills/ngterm".into(),
        };
        let codex = Target {
            agent: "Codex",
            skill_directory: "/h/.codex/skills/ngterm".into(),
        };
        assert_eq!(install_targets(home, true, true), [claude.clone(), codex.clone()]);
        assert_eq!(install_targets(home, false, true), [codex]);
        assert_eq!(install_targets(home, false, false), [claude]);
    }

    #[test]
    fn only_an_absent_or_marked_skill_is_overwritten() {
        assert!(may_overwrite(false, None));
        assert!(!may_overwrite(true, None), "a directory without SKILL.md is the user's");
        assert!(!may_overwrite(true, Some("---\nname: ngterm\n---\n")));
        assert!(may_overwrite(
            true,
            Some("---\nname: ngterm\n---\n<!-- ngterm-skill -->\n")
        ));
    }

    #[test]
    fn an_install_from_before_the_rename_is_ours_and_the_forks_skill_is_not() {
        assert!(may_overwrite(
            true,
            Some("---\nname: ngterm\ndescription: x\n---\n<!-- agterm-skill -->\n")
        ));
        assert!(!may_overwrite(
            true,
            Some("---\nname: agterm\n---\n<!-- agterm-skill -->\n")
        ));
        assert!(!may_overwrite(true, Some("<!-- agterm-skill -->\nname: ngterm\n")));
    }

    #[test]
    fn the_report_names_what_was_installed_and_skipped() {
        let report = SkillReport {
            installed: vec![("Claude Code", "/h/.claude/skills/ngterm".into())],
            skipped: vec![(
                "Codex",
                "a different 'ngterm' skill is already there — left untouched".to_owned(),
            )],
            fresh_skills_dir: true,
        };
        assert!(report.is_warning());
        assert_eq!(report.title(), "Agent Skill Installed");
        let text = report.text();
        assert!(text.starts_with("Installed for:\n  • Claude Code: /h/.claude/skills/ngterm\n"));
        assert!(text.contains("Help ▸ Install Command Line Tool…"));
        assert!(text.contains("ngtermctl CLI, so install that too"));
        assert!(text.contains("Skipped:\n  • Codex: a different 'ngterm' skill"));
        assert!(text.ends_with("so it picks up the new skill."));
        let none = SkillReport::default();
        assert_eq!(none.title(), "Skill Not Installed");
        assert_eq!(none.text(), "The skill was not installed.");
    }
}
