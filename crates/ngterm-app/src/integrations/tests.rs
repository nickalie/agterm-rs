//! The installers end to end on a temp home: what each plans from what it finds (status detection), what it writes,
//! and that a second run changes nothing (managed-marker idempotence). Nothing here touches the real home.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use super::hooks::{
    self, ClaudePlan, CodexPlan, CodexResult, OpenCodePlan, OpenCodeResult, OpenCodeStart, PiPlan, PiResult,
};
use super::hooks_config::{
    self as config, CodexMerge, FISH_INTEGRATION_RELATIVE_PATH, INTEGRATION_RELATIVE_PATH, RC_MARKER_BEGIN,
    WRAPPER_NAME,
};
use super::opencode::{Probe, Version};
use super::{Environment, files, skill};

fn resources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources")
}

/// A temp home with the checkout's bundled resources and a fake bundled `ngtermctl`.
struct Home {
    _dir: tempfile::TempDir,
    env: Environment,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let tool = dir.path().join("stage/bin/ngtermctl");
        fs::create_dir_all(tool.parent().unwrap()).unwrap();
        fs::write(&tool, "#!/bin/sh\n").unwrap();
        Self {
            env: Environment {
                home,
                agent_status: Some(resources().join("agent-status")),
                skill: Some(resources().join("skills/ngterm")),
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
    assert!(!home.path(".config/ngterm").exists());
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
    assert_eq!(plan.script_dir, home.path(".config/ngterm/agent-status"));
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
    let script_dir = home.path(".config/ngterm/agent-status");
    let text = outcome.text();
    assert!(text.starts_with(&format!("Scripts installed to {}.\n", script_dir.display())));
    assert!(text.ends_with("Open a new terminal for the shell integration to take effect."));
    assert_fresh_install_files(&home);
}

/// The restore adapter in the package, executable and with `ngtermctl` baked in, the Claude Code hooks calling it and
/// the Codex hooks not.
fn assert_restore_adapter_installed(home: &Home) {
    let restore = home
        .path(".config/ngterm/agent-status")
        .join(config::RESTORE_WRAPPER_NAME);
    assert_eq!(
        files::posix_mode(&restore).unwrap() & 0o111,
        0o111,
        "the restore adapter is executable"
    );
    let tool = home.env.bundled_cli.clone().unwrap();
    assert!(
        fs::read_to_string(&restore)
            .unwrap()
            .contains(&format!("  NGTERMCTL='{}'", tool.display())),
        "the restore adapter has ngtermctl baked in"
    );
    let settings = home.read(".claude/settings.json");
    for arguments in ["claude start", "claude end"] {
        assert!(
            settings.contains(&format!("{}' {arguments}", restore.display())),
            "{arguments}"
        );
    }
    assert!(
        !home.read(".codex/config.toml").contains(config::RESTORE_WRAPPER_NAME),
        "Codex gets no restore hooks"
    );
}

fn assert_fresh_install_files(home: &Home) {
    let wrapper = home.path(".config/ngterm/agent-status").join(WRAPPER_NAME);
    assert_eq!(
        files::posix_mode(&wrapper).unwrap() & 0o111,
        0o111,
        "the wrapper stays executable"
    );
    let tool = home.env.bundled_cli.clone().unwrap();
    assert!(
        fs::read_to_string(&wrapper)
            .unwrap()
            .contains(&format!("  NGTERMCTL='{}'", tool.display()))
    );
    assert!(
        home.read(".claude/settings.json")
            .contains("ngterm-claude-status.sh' completed --auto-reset")
    );
    assert_restore_adapter_installed(home);
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
        home.read(".pi/agent/extensions/ngterm-status.ts"),
        fs::read_to_string(resources().join("agent-status/pi/ngterm-status.ts")).unwrap()
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

/// `text` under the names an install from before the rename wrote: its markers, wrappers and plugin markers.
fn before_the_rename(text: &str) -> String {
    text.replace(RC_MARKER_BEGIN, config::LEGACY_RC_MARKER_BEGIN)
        .replace(config::RC_MARKER_END, config::LEGACY_RC_MARKER_END)
        .replace(config::CLAUDE_WRAPPER_NAME, config::LEGACY_CLAUDE_WRAPPER_NAME)
        .replace(config::CODEX_WRAPPER_NAME, config::LEGACY_CODEX_WRAPPER_NAME)
        .replace(WRAPPER_NAME, config::LEGACY_WRAPPER_NAME)
        .replace("// ngterm-", "// agterm-")
}

/// Claude settings `json` without the restore adapter's entries, which no install from before the rename wrote.
fn without_restore_hooks(json: &str) -> String {
    let mut root: serde_json::Value = serde_json::from_str(json).unwrap();
    let hooks = root["hooks"].as_object_mut().unwrap();
    for entries in hooks.values_mut() {
        entries
            .as_array_mut()
            .unwrap()
            .retain(|entry| !entry.to_string().contains(config::RESTORE_WRAPPER_NAME));
    }
    hooks.retain(|_, entries| entries.as_array().is_some_and(|entries| !entries.is_empty()));
    serde_json::to_string_pretty(&root).unwrap()
}

/// What an install from before the rename left in `home` with its package in `dir` (the legacy directory, as the fork
/// installs, or this build's own): every agent's config calling `dir` under the old names, the plugins under their old
/// names and markers, and its package there.
fn install_before_the_rename(home: &Home, dir: &Path) {
    let dir = dir.display().to_string();
    let (claude, _) = config::merge_claude_settings(Some("{\"model\": \"opus\"}\n"), &dir, &[]).unwrap();
    home.write(
        ".claude/settings.json",
        &before_the_rename(&without_restore_hooks(&claude)),
    );
    let CodexMerge::Merged(codex) = config::merge_codex_config("model = \"gpt-5\"\n", &dir) else {
        panic!("the old Codex block");
    };
    home.write(".codex/config.toml", &before_the_rename(&codex));
    for (rc, script) in [
        (".zshrc", INTEGRATION_RELATIVE_PATH),
        (".bashrc", INTEGRATION_RELATIVE_PATH),
        (".config/fish/config.fish", FISH_INTEGRATION_RELATIVE_PATH),
    ] {
        home.write(
            rc,
            &before_the_rename(&config::append_shell_rc("export FOO=1\n", &dir, script).0),
        );
    }
    // the plugins as the earlier package shipped them: calling its wrapper in its own directory
    let relative = Path::new(&dir).strip_prefix(&home.env.home).unwrap();
    let segments: Vec<String> = relative
        .iter()
        .map(|part| format!("\"{}\"", part.to_string_lossy()))
        .collect();
    let earlier = |bundled: &str| {
        let text = fs::read_to_string(resources().join("agent-status").join(bundled)).unwrap();
        before_the_rename(&text.replace("\".config\", \"ngterm\", \"agent-status\"", &segments.join(", ")))
    };
    home.write(".pi/agent/extensions/agterm-status.ts", &earlier("pi/ngterm-status.ts"));
    home.write(
        ".config/opencode/plugins/agterm-status.js",
        &earlier("opencode/ngterm-status.js"),
    );
    for name in [
        config::LEGACY_WRAPPER_NAME,
        config::LEGACY_CLAUDE_WRAPPER_NAME,
        config::LEGACY_CODEX_WRAPPER_NAME,
        "pi/agterm-status.ts",
        "opencode/agterm-status.js",
        "opencode/agterm-v2/tui.js",
    ] {
        home.write(&format!("{}/{name}", relative.display()), "#!/bin/sh\n");
    }
}

/// The old wrappers, which a reinstall leaves in the script directory for the shells and configs from before the
/// rename that still call them.
const OLD_WRAPPERS: [&str; 3] = [
    config::LEGACY_WRAPPER_NAME,
    config::LEGACY_CLAUDE_WRAPPER_NAME,
    config::LEGACY_CODEX_WRAPPER_NAME,
];

/// The Pi extension and the `OpenCode` v1 plugin under their old names.
const OLD_PLUGINS: [&str; 2] = [
    ".pi/agent/extensions/agterm-status.ts",
    ".config/opencode/plugins/agterm-status.js",
];

/// A reinstall over an install from before the rename, in the legacy directory or in this build's, moves every
/// reference onto the current names and directory, adds no second set, removes the old plugins that call this build's
/// directory and its old plugin copies, keeps its old wrappers, leaves the fork's package and plugins alone, and a run
/// after it changes nothing.
#[test]
fn an_install_from_before_the_rename_moves_every_reference() {
    let legacy = hooks::legacy_script_dir(Path::new(""));
    for dir in [legacy.clone(), hooks::script_dir(Path::new(""))] {
        let home = Home::new();
        install_before_the_rename(&home, &home.env.home.join(&dir));
        let start = OpenCodeStart::Install(Version::V1);
        let ours = dir != legacy;

        let outcome = home.install_hooks(&start, None);
        assert_eq!(outcome.codex, CodexResult::Merged);
        assert_eq!(outcome.pi, PiResult::Installed);
        assert!(matches!(outcome.opencode, OpenCodeResult::Installed(Version::V1, _)));
        assert_moved_onto_the_current_names(&home, ours);
        for name in OLD_WRAPPERS {
            assert!(
                home.env.home.join(&dir).join(name).is_file(),
                "{name} stays for its callers"
            );
        }
        assert_eq!(
            home.path(".config/agterm/agent-status/opencode/agterm-v2/tui.js")
                .exists(),
            !ours,
            "the fork's package stays"
        );
        assert!(
            !home.path(".config/ngterm/agent-status/opencode/agterm-v2").exists(),
            "this build's old plugin copies go, their folder with them"
        );
        assert_a_second_run_changes_nothing(&home, &start);
    }
}

/// Every agent's config calls the current names once, the old plugins are gone when `ours` (they call this build's
/// directory) and stay otherwise (the fork's), and nothing else names agterm.
fn assert_moved_onto_the_current_names(home: &Home, ours: bool) {
    let kept: &[&str] = if ours { &[] } else { &OLD_PLUGINS };
    assert_nothing_calls_the_old_names(home, kept);
    let settings = home.read(".claude/settings.json");
    assert_eq!(settings.matches("ngterm-claude-status.sh").count(), 6, "{settings}");
    for rc in [".zshrc", ".bashrc", ".config/fish/config.fish", ".codex/config.toml"] {
        assert_eq!(home.read(rc).matches(RC_MARKER_BEGIN).count(), 1, "{rc}");
    }
    assert_eq!(
        home.read(".pi/agent/extensions/ngterm-status.ts"),
        fs::read_to_string(resources().join("agent-status/pi/ngterm-status.ts")).unwrap()
    );
    for old in OLD_PLUGINS {
        assert_eq!(home.path(old).exists(), !ours, "{old}");
    }
}

/// A second install over what the first left finds everything current and changes nothing.
fn assert_a_second_run_changes_nothing(home: &Home, start: &OpenCodeStart) {
    let after_first = home.snapshot();
    let plan = hooks::plan(&home.env, start, None).unwrap();
    assert_eq!(plan.claude, ClaudePlan::Current);
    assert_eq!(plan.codex, CodexPlan::Current);
    assert_eq!(plan.pi, PiPlan::Current);
    assert!(plan.shell_rcs.is_empty());
    assert_eq!(plan.stale, hooks::Leftovers::default());
    hooks::apply(&plan).unwrap();
    assert_eq!(home.snapshot(), after_first, "a second run changes nothing");
}

/// No file in `home` but the fork's package, the backups, the old wrappers and `kept` still names the legacy directory
/// or an old name.
fn assert_nothing_calls_the_old_names(home: &Home, kept: &[&str]) {
    let script_dir = hooks::script_dir(Path::new(""));
    for (relative, bytes) in home.snapshot() {
        let text = String::from_utf8_lossy(&bytes);
        if relative.starts_with(".config/agterm")
            || relative.extension().is_some_and(|ext| ext == "bak")
            || OLD_WRAPPERS.iter().any(|name| relative == script_dir.join(name))
            || kept.iter().any(|path| relative == Path::new(path))
        {
            continue;
        }
        assert!(!text.contains("agterm"), "{} still names agterm", relative.display());
        assert!(
            !relative.to_string_lossy().contains("agterm"),
            "{} is still there",
            relative.display()
        );
    }
}

/// An old Pi extension or `OpenCode` plugin without its old marker is the user's: the install leaves it.
#[test]
fn unmarked_files_under_the_old_names_are_the_users() {
    let home = Home::new();
    home.mkdir(".pi/agent");
    home.write(
        ".pi/agent/extensions/agterm-status.ts",
        "export default () => {} // mine\n",
    );
    home.write(".config/opencode/plugins/agterm-status.js", "// mine\n");
    home.write(".config/opencode/plugins/agterm-v2/tui.js", "// mine\n");

    let outcome = home.install_hooks(&OpenCodeStart::Install(Version::V1), None);
    assert_eq!(outcome.pi, PiResult::Installed);
    assert_eq!(
        home.read(".pi/agent/extensions/agterm-status.ts"),
        "export default () => {} // mine\n"
    );
    assert_eq!(home.read(".config/opencode/plugins/agterm-status.js"), "// mine\n");
    assert_eq!(home.read(".config/opencode/plugins/agterm-v2/tui.js"), "// mine\n");
}

/// An old Pi extension or `OpenCode` plugin under `marker`, calling the wrapper in the script directory whose path
/// segments `dir` lists, as the earlier packages' `join(homedir(), …)` does.
fn old_plugin(marker: &str, dir: &str) -> String {
    format!("{marker}\nconst wrapper = join(homedir(), {dir}, \"agterm-agent-status.sh\");\n")
}

/// This build's script directory as its plugins from before the rename named it, and the fork's (upstream's too).
const OUR_DIR: &str = "\".config\", \"ngterm\", \"agent-status\"";
const FORKS_DIR: &str = "\".config\", \"agterm\", \"agent-status\"";

/// Probes `OpenCode` 2 with its configuration in `config`.
fn opencode_v2(config: &Path) -> Probe {
    Probe {
        version: Some(Version::V2),
        v2_configuration_directory: Some(config.display().to_string()),
    }
}

/// A v2 install removes both old plugins that call this build's directory, the v2 one's folder with it; the same files
/// calling the fork's directory (the fork and upstream install them under the same names and markers for their own
/// app) are theirs and stay.
#[test]
fn a_v2_install_removes_our_old_plugins_of_both_versions_and_keeps_the_forks() {
    for (dir, ours) in [(OUR_DIR, true), (FORKS_DIR, false)] {
        let home = Home::new();
        let config = home.path(".config/opencode");
        for version in Version::ALL {
            let (plugin, marker) = version.legacy_plugin(&config);
            home.write(
                plugin.strip_prefix(&home.env.home).unwrap().to_str().unwrap(),
                &old_plugin(marker, dir),
            );
        }
        let outcome = home.install_hooks(&OpenCodeStart::Install(Version::V2), Some(&opencode_v2(&config)));
        assert!(
            matches!(outcome.opencode, OpenCodeResult::Installed(Version::V2, _)),
            "{:?}",
            outcome.opencode
        );
        assert_eq!(config.join("plugins/agterm-status.js").exists(), !ours, "{dir}");
        assert_eq!(
            config.join("plugins/agterm-v2").exists(),
            !ours,
            "{dir}: the emptied folder goes too"
        );
        assert!(config.join("plugins/ngterm-v2/tui.js").is_file());
    }
}

/// The old Pi extension goes once the current one is in place when it calls this build's directory, and stays when
/// it calls the fork's.
#[test]
fn the_old_pi_extension_goes_only_when_it_is_ours() {
    for (dir, ours) in [(OUR_DIR, true), (FORKS_DIR, false)] {
        let home = Home::new();
        home.mkdir(".pi/agent");
        let old = old_plugin(config::LEGACY_PI_EXTENSION_MARKER, dir);
        home.write(OLD_PLUGINS[0], &old);
        let outcome = home.install_hooks(&no_opencode(&home), None);
        assert_eq!(outcome.pi, PiResult::Installed);
        assert_eq!(home.path(OLD_PLUGINS[0]).exists(), !ours, "{dir}");
        if !ours {
            assert_eq!(home.read(OLD_PLUGINS[0]), old);
        }
    }
}

/// A symlinked old v2 plugin folder goes by itself, the link only: nothing inside its target is removed, in the
/// `OpenCode` configuration and in the script directory alike.
#[test]
fn a_symlinked_old_v2_plugin_folder_is_unlinked_not_emptied() {
    let home = Home::new();
    let config = home.path(".config/opencode");
    let (plugin, marker) = Version::V2.legacy_plugin(&config);
    let folder = plugin.parent().unwrap().to_path_buf();
    let package_folder = hooks::script_dir(&home.env.home).join("opencode/agterm-v2");
    let mut targets = Vec::new();
    for (link, name) in [(&folder, "plugin"), (&package_folder, "package")] {
        let target = home.path(&format!("dotfiles/{name}/agterm-v2"));
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("tui.js"), old_plugin(marker, OUR_DIR)).unwrap();
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&target, link).unwrap();
        targets.push(target);
    }

    let outcome = home.install_hooks(&OpenCodeStart::Install(Version::V2), Some(&opencode_v2(&config)));
    assert!(matches!(outcome.opencode, OpenCodeResult::Installed(Version::V2, _)));
    for link in [&folder, &package_folder] {
        assert!(!files::entry_exists(link), "{} is unlinked", link.display());
    }
    for target in &targets {
        assert_eq!(
            fs::read_to_string(target.join("tui.js")).unwrap(),
            old_plugin(marker, OUR_DIR),
            "the link's target is left whole"
        );
    }
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
        ".pi/agent/extensions/ngterm-status.ts",
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
        home.read(".pi/agent/extensions/ngterm-status.ts"),
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
            "Except {}: it couldn't be read, so ngterm left it untouched",
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
    home.write(".config/ngterm", "a file where the directory should be");
    let plan = hooks::plan(&home.env, &no_opencode(&home), None).unwrap();
    let err = hooks::apply(&plan).unwrap_err();
    let script_dir = home.path(".config/ngterm/agent-status");
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
    home.write(".config/ngterm/agent-status/ngterm-claude-restore.sh", "#!/bin/sh\n");
    home.install_hooks(&no_opencode(&home), None);
    assert_eq!(
        home.read(".config/ngterm/agent-status/ngterm-claude-restore.sh"),
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
    let plugin = home.path(".config/opencode/plugins/ngterm-status.js");

    let outcome = home.install_hooks(&start, None);
    assert_eq!(outcome.opencode, OpenCodeResult::Installed(Version::V1, plugin.clone()));
    assert!(!home.path(".config/opencode/plugins/ngterm-v2").exists());
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
    let legacy = config.join("plugins/ngterm-status.js");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, "// ngterm-opencode-status-plugin\n").unwrap();

    let start = hooks::opencode_start(&home.env.home, Some(&probe));
    let outcome = home.install_hooks(&start, Some(&probe));
    assert_eq!(
        outcome.opencode,
        OpenCodeResult::Installed(Version::V2, config.join("plugins/ngterm-v2/tui.js"))
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
    home.write(".codex/skills/ngterm/SKILL.md", "---\nname: ngterm\n---\nmine\n");

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

    assert_eq!(report.installed, [("Claude Code", home.path(".claude/skills/ngterm"))]);
    assert_eq!(report.skipped.len(), 1);
    assert!(report.fresh_skills_dir);
    assert_eq!(
        files::relative_files(&home.path(".claude/skills/ngterm")).unwrap(),
        files::relative_files(&resources().join("skills/ngterm")).unwrap()
    );
    assert_eq!(
        home.read(".codex/skills/ngterm/SKILL.md"),
        "---\nname: ngterm\n---\nmine\n"
    );
    assert!(
        files::posix_mode(&home.path(".claude/skills/ngterm/scripts/show-image.sh")).unwrap() & 0o111 != 0,
        "the helper script stays executable"
    );
}

/// The fork's `agterm` skill, marked as an install of its own, is a different skill: never replaced or removed.
#[test]
fn the_forks_agterm_skill_is_left_alone() {
    let home = Home::new();
    let forks = "---\nname: agterm\n---\n<!-- agterm-skill -->\nthe fork's\n";
    home.write(".claude/skills/agterm/SKILL.md", forks);
    let report = skill::apply(&skill::plan(&home.env).unwrap());
    assert_eq!(report.installed, [("Claude Code", home.path(".claude/skills/ngterm"))]);
    assert_eq!(home.read(".claude/skills/agterm/SKILL.md"), forks);
}

/// Managed-marker idempotence for the skill: the marked copy is replaced by an identical one.
#[test]
fn a_second_skill_install_replaces_its_own_copy() {
    let home = Home::new();
    home.write(".claude/skills/ngterm/stale.md", "from an older build");
    home.write(".claude/skills/ngterm/SKILL.md", "<!-- ngterm-skill -->\nold\n");

    let report = skill::apply(&skill::plan(&home.env).unwrap());
    assert_eq!(report.installed.len(), 1);
    assert!(!report.fresh_skills_dir);
    assert!(
        !home.path(".claude/skills/ngterm/stale.md").exists(),
        "an ngterm install is replaced whole"
    );
    let first = home.snapshot();
    let _ = skill::apply(&skill::plan(&home.env).unwrap());
    assert_eq!(home.snapshot(), first);
}

#[test]
fn a_symlink_at_the_skill_destination_is_the_users() {
    let home = Home::new();
    home.mkdir(".claude/skills");
    symlink(home.path("nowhere"), home.path(".claude/skills/ngterm")).unwrap();
    let report = skill::apply(&skill::plan(&home.env).unwrap());
    assert!(report.installed.is_empty());
    assert!(
        fs::symlink_metadata(home.path(".claude/skills/ngterm")).is_ok(),
        "the dangling link survives"
    );
}

#[test]
fn the_bundled_resources_carry_their_ownership_markers() {
    let package = resources().join("agent-status");
    let read = |relative: &str| fs::read_to_string(package.join(relative)).unwrap();
    assert!(read("pi/ngterm-status.ts").contains(super::hooks_config::PI_EXTENSION_MARKER));
    for version in Version::ALL {
        assert!(read(&version.relative_path()).contains(version.marker()));
    }
    // NixOS, Guix and a non-merged /usr have no /usr/bin tools: only the shebang's env and macOS's plutil may be named
    for line in read("ngterm-codex-status.sh").lines() {
        let line = line.replace("/usr/bin/env", "").replace("/usr/bin/plutil", "");
        assert!(!line.contains("/usr/bin/"), "{line}");
    }
    let skill_md = fs::read_to_string(resources().join("skills/ngterm/SKILL.md")).unwrap();
    assert!(skill_md.contains(skill::MARKER));
    assert!(
        skill_md.starts_with(&format!("---\nname: {}\n", skill::SKILL_NAME)),
        "the frontmatter names the directory the skill installs into"
    );
    // the plugins call the wrapper where the install puts it
    let script_dir = hooks::script_dir(Path::new(""));
    let segments: Vec<String> = script_dir
        .iter()
        .map(|part| format!("\"{}\"", part.to_string_lossy()))
        .collect();
    let call = format!("join(homedir(), {}, \"{WRAPPER_NAME}\")", segments.join(", "));
    for plugin in [
        "pi/ngterm-status.ts",
        "opencode/ngterm-status.js",
        "opencode/ngterm-v2/tui.js",
    ] {
        assert!(read(plugin).contains(&call), "{plugin}: {call}");
    }
    for script in [
        "ngterm-agent-status.sh",
        "ngterm-claude-status.sh",
        "ngterm-codex-status.sh",
    ] {
        assert_eq!(
            files::posix_mode(&package.join(script)).unwrap() & 0o111,
            0o111,
            "{script}"
        );
    }
}
