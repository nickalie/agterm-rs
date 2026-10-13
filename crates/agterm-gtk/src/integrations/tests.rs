//! The installers end to end on a temp home: what each plans from what it finds (status detection), what it writes,
//! and that a second run changes nothing (managed-marker idempotence). Nothing here touches the real home.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use super::hooks::{
    self, ClaudePlan, CodexPlan, CodexResult, OpenCodePlan, OpenCodeResult, OpenCodeStart, PiPlan, PiResult,
};
use super::hooks_config::{RC_MARKER_BEGIN, WRAPPER_NAME};
use super::opencode::{Probe, Version};
use super::{Environment, files, skill};

fn resources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources")
}

/// A temp home with the checkout's bundled resources and a fake bundled `agtermctl`.
struct Home {
    _dir: tempfile::TempDir,
    env: Environment,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let tool = dir.path().join("stage/bin/agtermctl");
        fs::create_dir_all(tool.parent().unwrap()).unwrap();
        fs::write(&tool, "#!/bin/sh\n").unwrap();
        Self {
            env: Environment {
                home,
                agent_status: Some(resources().join("agent-status")),
                skill: Some(resources().join("skills/agterm")),
                bundled_cli: Some(tool),
                vars: HashMap::new(),
            },
            _dir: dir,
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.env.home.join(relative)
    }

    fn mkdir(&self, relative: &str) {
        fs::create_dir_all(self.path(relative)).unwrap();
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path(relative)).unwrap()
    }

    /// Every file under the home with its contents, for before/after comparisons.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        files::relative_files(&self.env.home)
            .unwrap()
            .into_iter()
            .map(|relative| {
                let bytes = fs::read(self.env.home.join(&relative)).unwrap();
                (relative, bytes)
            })
            .collect()
    }

    fn install_hooks(&self, start: &OpenCodeStart, probe: Option<&Probe>) -> hooks::HooksOutcome {
        hooks::apply(&hooks::plan(&self.env, start, probe).unwrap()).unwrap()
    }
}

fn no_opencode(home: &Home) -> OpenCodeStart {
    hooks::opencode_start(&home.env.home, None)
}

#[test]
fn the_plan_detects_each_agent_and_writes_nothing() {
    let home = Home::new();
    home.write(".claude/settings.json", "{\"model\": \"opus\"}\n");
    home.mkdir(".codex");
    home.write(".bashrc", "export FOO=1\n");
    let before = home.snapshot();

    let plan = hooks::plan(&home.env, &no_opencode(&home), None).unwrap();

    assert_eq!(home.snapshot(), before, "planning writes nothing");
    assert!(!home.path(".config/agterm").exists());
    assert!(
        matches!(&plan.claude, ClaudePlan::Merge(write) if write.backup.as_deref() == Some("{\"model\": \"opus\"}\n"))
    );
    assert!(
        matches!(&plan.codex, CodexPlan::Merge(write) if write.backup.is_none()),
        "{:?}",
        plan.codex
    );
    assert_eq!(plan.pi, PiPlan::NoPi);
    assert_eq!(
        plan.opencode,
        OpenCodePlan::Decided(OpenCodeResult::NoOpenCode(home.path(".config/opencode")))
    );
    let rcs: Vec<&Path> = plan.shell_rcs.iter().map(|write| write.path.as_path()).collect();
    assert_eq!(
        rcs,
        [home.path(".zshrc"), home.path(".bashrc")],
        "fish only once ~/.config/fish exists"
    );
    assert_eq!(plan.script_dir, home.path(".config/agterm/agent-status"));
    assert_eq!(
        plan.bake.as_deref(),
        home.env.bundled_cli.as_deref().and_then(Path::to_str)
    );
}

#[test]
fn a_fresh_install_wires_every_present_agent() {
    let home = Home::new();
    home.write(".claude/settings.json", "{\"model\": \"opus\"}\n");
    fs::set_permissions(home.path(".claude/settings.json"), fs::Permissions::from_mode(0o600)).unwrap();
    home.mkdir(".codex");
    home.mkdir(".config/fish");
    home.mkdir(".pi/agent");

    let outcome = home.install_hooks(&no_opencode(&home), None);

    assert!(!outcome.settings_skipped);
    assert_eq!(outcome.codex, CodexResult::Merged);
    assert_eq!(outcome.pi, PiResult::Installed);
    assert_eq!(outcome.is_warning(), outcome.opencode.is_warning());
    let script_dir = home.path(".config/agterm/agent-status");
    let text = outcome.text();
    assert!(text.starts_with(&format!("Scripts installed to {}.\n", script_dir.display())));
    assert!(text.ends_with("Open a new terminal for the shell integration to take effect."));
    assert_fresh_install_files(&home);
}

fn assert_fresh_install_files(home: &Home) {
    let wrapper = home.path(".config/agterm/agent-status").join(WRAPPER_NAME);
    assert_eq!(
        files::posix_mode(&wrapper).unwrap() & 0o111,
        0o111,
        "the wrapper stays executable"
    );
    let tool = home.env.bundled_cli.clone().unwrap();
    assert!(
        fs::read_to_string(&wrapper)
            .unwrap()
            .contains(&format!("  AGTERMCTL='{}'", tool.display()))
    );
    assert!(
        home.read(".claude/settings.json")
            .contains("agterm-claude-status.sh' completed --auto-reset")
    );
    assert_eq!(home.read(".claude/settings.json.bak"), "{\"model\": \"opus\"}\n");
    assert_eq!(files::posix_mode(&home.path(".claude/settings.json")), Some(0o600));
    assert_eq!(files::posix_mode(&home.path(".claude/settings.json.bak")), Some(0o600));
    assert!(home.read(".codex/config.toml").contains("[[hooks.PermissionRequest]]"));
    assert!(
        !home.path(".codex/config.toml.bak").exists(),
        "no backup of a file that was not there"
    );
    assert!(home.read(".zshrc").contains("shell/integration.sh'"));
    assert!(home.read(".bashrc").contains("shell/integration.sh'"));
    assert!(
        home.read(".config/fish/config.fish")
            .contains("shell/integration.fish'")
    );
    assert_eq!(
        home.read(".pi/agent/extensions/agterm-status.ts"),
        fs::read_to_string(resources().join("agent-status/pi/agterm-status.ts")).unwrap()
    );
}

/// Managed-marker idempotence: a second install finds its own markers and leaves every file as it was.
#[test]
fn a_second_install_changes_nothing() {
    let home = Home::new();
    home.write(".claude/settings.json", "{}\n");
    home.write(".codex/config.toml", "model = \"gpt-5\"\n");
    home.mkdir(".config/fish");
    home.mkdir(".pi/agent");
    home.install_hooks(&no_opencode(&home), None);
    let after_first = home.snapshot();

    let plan = hooks::plan(&home.env, &no_opencode(&home), None).unwrap();
    assert_eq!(plan.claude, ClaudePlan::Current);
    assert_eq!(plan.codex, CodexPlan::Current);
    assert_eq!(plan.pi, PiPlan::Current);
    assert!(plan.shell_rcs.is_empty());
    let outcome = hooks::apply(&plan).unwrap();

    assert_eq!(outcome.codex, CodexResult::AlreadyConfigured);
    assert_eq!(outcome.pi, PiResult::AlreadyConfigured);
    assert_eq!(home.snapshot(), after_first);
    assert_eq!(home.read(".zshrc").matches(RC_MARKER_BEGIN).count(), 1);
    assert_eq!(home.read(".codex/config.toml").matches(RC_MARKER_BEGIN).count(), 1);
}

#[test]
fn files_the_install_cannot_safely_change_are_left_alone() {
    let home = Home::new();
    home.write(".claude/settings.json", "{ not json");
    home.write(
        ".codex/config.toml",
        "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"echo\"\n",
    );
    home.write(
        ".pi/agent/extensions/agterm-status.ts",
        "export default () => {} // mine\n",
    );
    let zshrc = [0xff_u8, 0xfe, b'\n'];
    fs::write(home.path(".zshrc"), zshrc).unwrap();

    let outcome = home.install_hooks(&no_opencode(&home), None);

    assert!(outcome.settings_skipped);
    assert_eq!(outcome.codex, CodexResult::HooksExist);
    assert!(outcome.codex.needs_manual_merge());
    assert_eq!(outcome.pi, PiResult::UserOwned);
    assert!(outcome.is_warning());
    assert_eq!(outcome.title(), "Agent Status Hooks Installed — with a warning");
    assert_eq!(home.read(".claude/settings.json"), "{ not json");
    assert!(!home.path(".claude/settings.json.bak").exists());
    assert_eq!(
        home.read(".pi/agent/extensions/agterm-status.ts"),
        "export default () => {} // mine\n"
    );
    assert_eq!(
        fs::read(home.path(".zshrc")).unwrap(),
        zshrc,
        "an unreadable rc file is not replaced"
    );
    assert_eq!(outcome.rcs_skipped, [home.path(".zshrc")]);
    assert!(
        home.read(".bashrc").contains("shell/integration.sh'"),
        "the readable one still gets it"
    );
    let text = outcome.text();
    assert!(
        text.contains(&format!(
            "Except {}: it couldn't be read, so agterm left it untouched",
            home.path(".zshrc").display()
        )),
        "{text}"
    );
}

/// An unreadable rc file alone is enough for the warning title.
#[test]
fn a_skipped_rc_file_alone_makes_the_install_a_warning() {
    let home = Home::new();
    fs::write(home.path(".bashrc"), [0xff_u8, 0xfe]).unwrap();
    let outcome = home.install_hooks(&no_opencode(&home), None);
    assert!(outcome.is_warning());
    assert_eq!(outcome.title(), "Agent Status Hooks Installed — with a warning");
    let clean = Home::new();
    let outcome = clean.install_hooks(&no_opencode(&clean), None);
    assert!(outcome.rcs_skipped.is_empty() && !outcome.is_warning());
    assert!(
        outcome
            .text()
            .contains("The source line was added to ~/.zshrc, ~/.bashrc (and")
    );
    assert!(!outcome.text().contains("Except"));
}

#[test]
fn a_failed_write_names_the_path_it_stopped_on() {
    let home = Home::new();
    home.write(".config/agterm", "a file where the directory should be");
    let plan = hooks::plan(&home.env, &no_opencode(&home), None).unwrap();
    let err = hooks::apply(&plan).unwrap_err();
    let script_dir = home.path(".config/agterm/agent-status");
    assert!(
        err.0.starts_with(&format!(
            "Could not install the scripts into {}: ",
            script_dir.display()
        )),
        "{}",
        err.0
    );
}

#[test]
fn a_symlinked_config_keeps_its_link_and_the_backup_stays_beside_it() {
    let home = Home::new();
    home.write("dotfiles/settings.json", "{}\n");
    home.mkdir(".claude");
    symlink(home.path("dotfiles/settings.json"), home.path(".claude/settings.json")).unwrap();

    home.install_hooks(&no_opencode(&home), None);

    assert!(
        fs::symlink_metadata(home.path(".claude/settings.json"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(home.read("dotfiles/settings.json").contains("UserPromptSubmit"));
    assert_eq!(home.read(".claude/settings.json.bak"), "{}\n");
    assert!(!home.path("dotfiles/settings.json.bak").exists());
}

#[test]
fn files_the_package_does_not_ship_survive_a_reinstall() {
    let home = Home::new();
    home.write(".config/agterm/agent-status/agterm-claude-restore.sh", "#!/bin/sh\n");
    home.install_hooks(&no_opencode(&home), None);
    assert_eq!(
        home.read(".config/agterm/agent-status/agterm-claude-restore.sh"),
        "#!/bin/sh\n"
    );
}

#[test]
fn the_opencode_start_follows_the_probe_and_the_configs_present() {
    let home = Home::new();
    let config = home.path(".config/opencode");
    let probe = |version, directory: Option<&Path>| Probe {
        version,
        v2_configuration_directory: directory.map(|path| path.display().to_string()),
    };
    assert_eq!(
        hooks::opencode_start(&home.env.home, None),
        OpenCodeStart::Decided(OpenCodeResult::NoOpenCode(config.clone()))
    );
    let elsewhere = home.path("elsewhere/opencode");
    assert_eq!(
        hooks::opencode_start(&home.env.home, Some(&probe(None, Some(&elsewhere)))),
        OpenCodeStart::Decided(OpenCodeResult::NoOpenCode(elsewhere.clone()))
    );
    assert_eq!(
        hooks::opencode_start(&home.env.home, Some(&probe(Some(Version::V2), None))),
        OpenCodeStart::Install(Version::V2)
    );
    home.mkdir(".config/opencode");
    assert_eq!(hooks::opencode_start(&home.env.home, None), OpenCodeStart::Ask);
    assert_eq!(
        hooks::opencode_start(&home.env.home, Some(&probe(None, Some(&elsewhere)))),
        OpenCodeStart::Ask
    );
}

#[test]
fn opencode_v1_installs_once_and_is_then_current() {
    let home = Home::new();
    home.mkdir(".config/opencode");
    let start = OpenCodeStart::Install(Version::V1);
    let plugin = home.path(".config/opencode/plugins/agterm-status.js");

    let outcome = home.install_hooks(&start, None);
    assert_eq!(outcome.opencode, OpenCodeResult::Installed(Version::V1, plugin.clone()));
    assert!(!home.path(".config/opencode/plugins/agterm-v2").exists());
    let again = home.install_hooks(&start, None);
    assert_eq!(again.opencode, OpenCodeResult::AlreadyConfigured(Version::V1, plugin));
}

#[test]
fn opencode_v2_removes_the_managed_v1_plugin_and_keeps_a_users() {
    let home = Home::new();
    let config = home.path("custom opencode");
    let probe = Probe {
        version: Some(Version::V2),
        v2_configuration_directory: Some(config.display().to_string()),
    };
    let legacy = config.join("plugins/agterm-status.js");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, "// agterm-opencode-status-plugin\n").unwrap();

    let start = hooks::opencode_start(&home.env.home, Some(&probe));
    let outcome = home.install_hooks(&start, Some(&probe));
    assert_eq!(
        outcome.opencode,
        OpenCodeResult::Installed(Version::V2, config.join("plugins/agterm-v2/tui.js"))
    );
    assert!(!legacy.exists());
    assert!(hooks::opencode_text(&outcome.opencode).contains(&config.display().to_string()));

    fs::write(&legacy, "user-owned").unwrap();
    let outcome = home.install_hooks(&start, Some(&probe));
    assert_eq!(outcome.opencode, OpenCodeResult::V1CleanupUserOwned(legacy.clone()));
    assert_eq!(fs::read_to_string(&legacy).unwrap(), "user-owned");
}

#[test]
fn opencode_v2_without_a_known_directory_or_skipped_writes_nothing() {
    let home = Home::new();
    home.mkdir(".config/opencode");
    let outcome = home.install_hooks(&OpenCodeStart::Install(Version::V2), None);
    assert_eq!(outcome.opencode, OpenCodeResult::V2ConfigUnavailable);
    assert!(outcome.opencode.is_warning());
    let outcome = home.install_hooks(&OpenCodeStart::Decided(OpenCodeResult::Skipped), None);
    assert_eq!(outcome.opencode, OpenCodeResult::Skipped);
    assert!(!outcome.opencode.is_warning());
    assert!(!home.path(".config/opencode/plugins").exists());
}

#[test]
fn a_build_without_the_package_fails_before_writing() {
    let mut home = Home::new();
    home.env.agent_status = None;
    let err = hooks::plan(&home.env, &no_opencode(&home), None).unwrap_err();
    assert_eq!(err.0, "The agent-status scripts are not bundled in this build.");
    assert!(home.snapshot().is_empty());
}

#[test]
fn every_alert_line_stays_one_line_and_never_inlines_the_codex_block() {
    for codex in [
        CodexResult::Merged,
        CodexResult::AlreadyConfigured,
        CodexResult::HooksExist,
        CodexResult::Unparseable,
        CodexResult::Unreadable,
        CodexResult::NoCodex,
    ] {
        let text = hooks::codex_text(codex);
        assert!(!text.contains('\n') && !text.contains("[[hooks."), "{codex:?}");
        assert_eq!(
            codex.needs_manual_merge(),
            text.contains("Add Codex hooks by hand"),
            "{codex:?}"
        );
    }
    let path = PathBuf::from("/custom/opencode/plugins/v.js");
    for version in Version::ALL {
        for result in [
            OpenCodeResult::Installed(version, path.clone()),
            OpenCodeResult::AlreadyConfigured(version, path.clone()),
            OpenCodeResult::UserOwned(version, path.clone()),
            OpenCodeResult::Unreadable(version, path.clone()),
            OpenCodeResult::WriteFailed(version, path.clone()),
        ] {
            let text = hooks::opencode_text(&result);
            assert!(text.contains(&format!("OpenCode {}", version.raw())) && text.contains("/custom/opencode"));
            assert!(!text.contains('\n'));
        }
    }
    assert_eq!(
        hooks::opencode_text(&OpenCodeResult::NoOpenCode("/custom/opencode".into())),
        "No /custom/opencode found, so the OpenCode plugin was skipped. Start OpenCode once, then run this again. \
         Coarse shell detection for opencode is off by default."
    );
    for pi in [
        PiResult::Installed,
        PiResult::UserOwned,
        PiResult::WriteFailed,
        PiResult::NoPi,
    ] {
        assert!(!hooks::pi_text(pi).contains('\n'));
    }
}

#[test]
fn the_skill_installs_for_each_agent_and_refuses_a_users_skill() {
    let home = Home::new();
    home.mkdir(".claude");
    home.write(".codex/skills/agterm/SKILL.md", "---\nname: agterm\n---\nmine\n");

    let plan = skill::plan(&home.env).unwrap();
    assert!(!home.path(".claude/skills").exists(), "planning writes nothing");
    assert!(matches!(
        &plan.targets[0],
        skill::TargetPlan::Install {
            fresh_skills_dir: true,
            ..
        }
    ));
    assert!(matches!(&plan.targets[1], skill::TargetPlan::UserOwned(target) if target.agent == "Codex"));
    let report = skill::apply(&plan);

    assert_eq!(report.installed, [("Claude Code", home.path(".claude/skills/agterm"))]);
    assert_eq!(report.skipped.len(), 1);
    assert!(report.fresh_skills_dir);
    assert_eq!(
        files::relative_files(&home.path(".claude/skills/agterm")).unwrap(),
        files::relative_files(&resources().join("skills/agterm")).unwrap()
    );
    assert_eq!(
        home.read(".codex/skills/agterm/SKILL.md"),
        "---\nname: agterm\n---\nmine\n"
    );
    assert!(
        files::posix_mode(&home.path(".claude/skills/agterm/scripts/show-image.sh")).unwrap() & 0o111 != 0,
        "the helper script stays executable"
    );
}

/// Managed-marker idempotence for the skill: the marked copy is replaced by an identical one.
#[test]
fn a_second_skill_install_replaces_its_own_copy() {
    let home = Home::new();
    home.write(".claude/skills/agterm/stale.md", "from an older build");
    home.write(".claude/skills/agterm/SKILL.md", "<!-- agterm-skill -->\nold\n");

    let report = skill::apply(&skill::plan(&home.env).unwrap());
    assert_eq!(report.installed.len(), 1);
    assert!(!report.fresh_skills_dir);
    assert!(
        !home.path(".claude/skills/agterm/stale.md").exists(),
        "an agterm install is replaced whole"
    );
    let first = home.snapshot();
    let _ = skill::apply(&skill::plan(&home.env).unwrap());
    assert_eq!(home.snapshot(), first);
}

#[test]
fn a_symlink_at_the_skill_destination_is_the_users() {
    let home = Home::new();
    home.mkdir(".claude/skills");
    symlink(home.path("nowhere"), home.path(".claude/skills/agterm")).unwrap();
    let report = skill::apply(&skill::plan(&home.env).unwrap());
    assert!(report.installed.is_empty());
    assert!(
        fs::symlink_metadata(home.path(".claude/skills/agterm")).is_ok(),
        "the dangling link survives"
    );
}

#[test]
fn the_bundled_resources_carry_their_ownership_markers() {
    let package = resources().join("agent-status");
    let read = |relative: &str| fs::read_to_string(package.join(relative)).unwrap();
    assert!(read("pi/agterm-status.ts").contains(super::hooks_config::PI_EXTENSION_MARKER));
    for version in Version::ALL {
        assert!(read(&version.relative_path()).contains(version.marker()));
    }
    let skill_md = fs::read_to_string(resources().join("skills/agterm/SKILL.md")).unwrap();
    assert!(skill_md.contains(skill::MARKER));
    for script in [
        "agterm-agent-status.sh",
        "agterm-claude-status.sh",
        "agterm-codex-status.sh",
    ] {
        assert_eq!(
            files::posix_mode(&package.join(script)).unwrap() & 0o111,
            0o111,
            "{script}"
        );
    }
}
