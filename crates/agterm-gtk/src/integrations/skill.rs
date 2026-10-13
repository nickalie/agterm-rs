//! The agent skill installer (upstream's `SkillInstall.swift` and `SkillInstaller.swift`): the bundled `agterm` skill
//! into the skills directory of every coding agent present, Claude Code (`~/.claude/skills/agterm`) and Codex
//! (`~/.codex/skills/agterm`), Claude Code's when neither is. Idempotent, and a same-named skill the user wrote (its
//! `SKILL.md` absent, unreadable or unmarked) is never overwritten.

use std::fs;
use std::path::{Path, PathBuf};

use super::files;
use super::{Environment, InstallError};

/// The skill's directory name under each agent's `skills/`.
pub const SKILL_NAME: &str = "agterm";

/// The sentinel in the bundled `SKILL.md` telling an agterm-installed skill from the user's own.
pub const MARKER: &str = "<!-- agterm-skill -->";

/// One destination: the agent's label and its `…/skills/agterm` directory.
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

/// Whether a destination may be replaced: absent, or holding a `SKILL.md` with the marker.
#[must_use]
pub fn may_overwrite(directory_exists: bool, existing_skill: Option<&str>) -> bool {
    !directory_exists || existing_skill.is_some_and(|text| text.contains(MARKER))
}

/// What the install will do at one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetPlan {
    /// Copy the skill there; `fresh_skills_dir` when the agent's `skills/` does not exist yet.
    Install { target: Target, fresh_skills_dir: bool },
    /// A different `agterm` skill is there: left untouched.
    UserOwned(Target),
}

/// The plan: the bundled source and one step per target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPlan {
    pub source: PathBuf,
    pub targets: Vec<TargetPlan>,
}

/// Plans the install over `env`'s home, without writing.
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

/// Carries `plan` out: each target's prior agterm install is replaced by a fresh copy; a failure skips that target.
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
                "The agent uses it automatically when you ask it to control agterm. It drives the app via the \
                 agtermctl CLI, so install that too if you haven't: Help ▸ Install Command Line Tool…"
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
            skill_directory: "/h/.claude/skills/agterm".into(),
        };
        let codex = Target {
            agent: "Codex",
            skill_directory: "/h/.codex/skills/agterm".into(),
        };
        assert_eq!(install_targets(home, true, true), [claude.clone(), codex.clone()]);
        assert_eq!(install_targets(home, false, true), [codex]);
        assert_eq!(install_targets(home, false, false), [claude]);
    }

    #[test]
    fn only_an_absent_or_marked_skill_is_overwritten() {
        assert!(may_overwrite(false, None));
        assert!(!may_overwrite(true, None), "a directory without SKILL.md is the user's");
        assert!(!may_overwrite(true, Some("---\nname: agterm\n---\n")));
        assert!(may_overwrite(
            true,
            Some("---\nname: agterm\n---\n<!-- agterm-skill -->\n")
        ));
    }

    #[test]
    fn the_report_names_what_was_installed_and_skipped() {
        let report = SkillReport {
            installed: vec![("Claude Code", "/h/.claude/skills/agterm".into())],
            skipped: vec![(
                "Codex",
                "a different 'agterm' skill is already there — left untouched".to_owned(),
            )],
            fresh_skills_dir: true,
        };
        assert!(report.is_warning());
        assert_eq!(report.title(), "Agent Skill Installed");
        let text = report.text();
        assert!(text.starts_with("Installed for:\n  • Claude Code: /h/.claude/skills/agterm\n"));
        assert!(text.contains("Help ▸ Install Command Line Tool…"));
        assert!(text.contains("Skipped:\n  • Codex: a different 'agterm' skill"));
        assert!(text.ends_with("so it picks up the new skill."));
        let none = SkillReport::default();
        assert_eq!(none.title(), "Skill Not Installed");
        assert_eq!(none.text(), "The skill was not installed.");
    }
}
