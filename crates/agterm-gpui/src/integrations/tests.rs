//! The installers driven as the Help items and the welcome run them, in GPUI's test window over a temp home (never the
//! real one): each install's alert, what it wrote, and the planners' read-back of the home after each step (what a
//! next run would find: current, absent again after the user removed it, the user's own). The `OpenCode` probe runs a
//! fake login shell and a fake `opencode` from the temp home's `PATH`, so the machine's shell and `OpenCode` stay out.
//! One test runs a child of this binary whose `HOME` and `XDG_CONFIG_HOME` are temp directories, so the process
//! environment the app reads is checked too.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use agterm_app::integrations::cli::LinkStatus;
use agterm_app::integrations::hooks::{ClaudePlan, OpenCodePlan};
use agterm_app::integrations::hooks_config::{RC_MARKER_BEGIN, WRAPPER_NAME};
use agterm_app::integrations::skill::{MARKER, TargetPlan};
use agterm_app::integrations::{cli, files, skill};
use agterm_host::fakes::write_executable;
use gpui_kit::{TestAppContext, VisualTestContext};

use super::*;
use crate::modal::tests::host;
use crate::modal::{answer_front_alert, front_alert};

/// The login shell the `OpenCode` probe starts (`$SHELL -ilc <hop>`): runs the hop, reads no startup file.
const FAKE_SHELL: &str = "#!/bin/sh\n[ \"$1\" = -ilc ] || exit 64\nexec /bin/sh -c \"$2\"\n";

/// A temp home with the checkout's bundled resources, a fake bundled `agtermctl`, a fake login shell and a `PATH` of
/// the home's own `bin` alone. The installers read its environment through a [`Source`]; a test may change it between
/// runs.
struct Home {
    dir: tempfile::TempDir,
    env: Arc<Mutex<Environment>>,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = dir.path().join("bin");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let tool = dir.path().join("stage/bin/agtermctl");
        fs::create_dir_all(tool.parent().unwrap()).unwrap();
        write_executable(&tool, "#!/bin/sh\n").unwrap();
        let shell = bin.join("login-shell");
        write_executable(&shell, FAKE_SHELL).unwrap();
        let vars = HashMap::from([
            ("HOME".to_owned(), home.display().to_string()),
            ("SHELL".to_owned(), shell.display().to_string()),
            ("PATH".to_owned(), bin.display().to_string()),
        ]);
        let env = Environment {
            home,
            agent_status: Some(resources().join("agent-status")),
            skill: Some(resources().join("skills/agterm")),
            bundled_cli: Some(tool),
            vars,
        };
        Self {
            dir,
            env: Arc::new(Mutex::new(env)),
        }
    }

    fn env(&self) -> Environment {
        self.env.lock().unwrap().clone()
    }

    fn set_var(&self, key: &str, value: &str) {
        self.env.lock().unwrap().vars.insert(key.to_owned(), value.to_owned());
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.env().home.join(relative)
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

    /// An `opencode` on the home's `PATH` printing `output` for `--version`.
    fn opencode(&self, output: &str) {
        let script = format!("#!/bin/sh\n[ \"$1\" = --version ] || exit 2\necho '{output}'\n");
        write_executable(&self.dir.path().join("bin/opencode"), &script).unwrap();
    }

    /// Makes this home what the window's installs read.
    fn serve(&self, cx: &mut VisualTestContext) {
        let env = Arc::clone(&self.env);
        cx.update(|_, cx| Source::new(move || Some(env.lock().unwrap().clone())).install(cx));
    }

    /// The skill planner's read-back: what a next install would do at each target.
    fn skill_status(&self) -> Vec<TargetPlan> {
        skill::plan(&self.env()).unwrap().targets
    }

    /// The command line tool planner's read-back of the link.
    fn cli_status(&self) -> LinkStatus {
        match cli::plan(&self.env()) {
            cli::CliPlan::Link { status, .. } => status,
            cli::CliPlan::Failed(message) => panic!("the plan failed: {message}"),
        }
    }
}

fn resources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources")
}

/// What the window's front alert shows.
fn front(cx: &mut VisualTestContext) -> Option<AlertSpec> {
    cx.update(|window, cx| front_alert(window.window_handle(), cx).map(|alert| alert.read(cx).spec().clone()))
}

/// The front alert, which must be up.
fn shown(cx: &mut VisualTestContext) -> AlertSpec {
    front(cx).expect("an alert is up")
}

fn labels(spec: &AlertSpec) -> Vec<&str> {
    spec.buttons.iter().map(|button| button.label.as_ref()).collect()
}

/// Clicks the front alert's button `label` and lets what it starts run.
fn press(cx: &mut VisualTestContext, label: &str) {
    assert!(
        cx.update(|window, cx| answer_front_alert(window, label, cx)),
        "no {label} to press: {:?}",
        front(cx)
    );
    cx.run_until_parked();
}

/// Starts `run` over the test window, as its Help item does, and lets it run up to its first alert.
fn start(cx: &mut VisualTestContext, run: fn(AnyWindowHandle, &mut App)) {
    cx.update(|window, cx| run(window.window_handle(), cx));
    cx.run_until_parked();
}

#[test]
fn the_docs_button_is_second_so_ok_stays_the_default() {
    let alert = result_alert("Installed", "done", true);
    assert_eq!(alert.default_button(), Some(0));
    assert_eq!(alert.cancel_button(), Some(0));
    assert_eq!(labels(&alert), ["OK", "Open Docs"]);
    assert_eq!(labels(&result_alert("Installed", "done", false)), ["OK"]);
}

#[test]
fn the_opencode_chooser_skips_first_then_offers_each_version() {
    let alert = opencode_chooser();
    assert_eq!(alert.heading, "Choose OpenCode Version");
    assert_eq!(
        alert.body,
        "Could not determine the OpenCode version in your login shell (opencode --version). Choose the version you \
         use, or skip OpenCode and install the other hooks."
    );
    assert_eq!(labels(&alert), ["Skip OpenCode", "OpenCode v1", "OpenCode v2"]);
    assert_eq!(alert.cancel_button(), Some(0), "Escape skips");
    assert_eq!(
        alert.default_button(),
        Some(0),
        "Return skips, as upstream's first button"
    );
    assert_eq!(chosen_version(0), None);
    assert_eq!(chosen_version(1), Some(Version::V1));
    assert_eq!(chosen_version(2), Some(Version::V2));
    assert_eq!(chosen_version(3), None);
}

/// Whether every target the skill planner reads back is agterm's to install, its skills directory new or not.
fn all_fresh(home: &Home, expected: bool) -> bool {
    home.skill_status()
        .iter()
        .all(|plan| matches!(plan, TargetPlan::Install { fresh_skills_dir, .. } if *fresh_skills_dir == expected))
}

#[gpui_kit::test]
fn the_skill_installs_reads_back_current_and_comes_back_after_a_removal(cx: &mut TestAppContext) {
    let home = Home::new();
    home.mkdir(".claude");
    home.mkdir(".codex");
    let cx = host(cx);
    home.serve(cx);
    assert!(all_fresh(&home, true), "both agents' skills directories are new");
    first_skill_install(cx, &home);
    assert!(
        all_fresh(&home, false),
        "read back: agterm's own copies, replaced by the next run"
    );
    skill_removed_and_reinstalled(cx, &home);
    users_skill_left_alone(cx, &home);
}

fn first_skill_install(cx: &mut VisualTestContext, home: &Home) {
    let claude = home.path(".claude/skills/agterm");
    let codex = home.path(".codex/skills/agterm");
    start(cx, run_skill);
    let alert = shown(cx);
    assert_eq!(alert.heading, "Agent Skill Installed");
    let installed = format!(
        "Installed for:\n  • Claude Code: {}\n  • Codex: {}\n\nThe agent uses it automatically",
        claude.display(),
        codex.display()
    );
    assert!(alert.body.starts_with(&installed), "{}", alert.body);
    assert!(alert.body.ends_with(
        "A skills directory was created for the first time — restart any running agent session so it picks up the \
         new skill."
    ));
    assert_eq!(labels(&alert), ["OK"]);
    for installed in [&claude, &codex] {
        assert!(fs::read_to_string(installed.join("SKILL.md")).unwrap().contains(MARKER));
    }
    press(cx, "OK");
    assert!(front(cx).is_none(), "OK closes it and nothing follows");
}

/// The user removes Claude Code's copy: the next run finds it absent and puts it back.
fn skill_removed_and_reinstalled(cx: &mut VisualTestContext, home: &Home) {
    let claude = home.path(".claude/skills/agterm");
    fs::remove_dir_all(&claude).unwrap();
    assert!(!files::entry_exists(&claude));
    start(cx, run_skill);
    let alert = shown(cx);
    assert!(alert.body.contains(&format!("  • Claude Code: {}", claude.display())));
    assert!(
        !alert.body.contains("for the first time"),
        "both skills directories are there"
    );
    press(cx, "OK");
    assert!(claude.join("SKILL.md").is_file());
}

/// A skill of the user's under the name is never overwritten.
fn users_skill_left_alone(cx: &mut VisualTestContext, home: &Home) {
    let users = "---\nname: agterm\n---\nmine\n";
    let skill = home.path(".codex/skills/agterm/SKILL.md");
    fs::write(&skill, users).unwrap();
    assert!(matches!(&home.skill_status()[1], TargetPlan::UserOwned(target) if target.agent == "Codex"));
    start(cx, run_skill);
    let alert = shown(cx);
    assert_eq!(alert.heading, "Agent Skill Installed");
    let skipped = "Skipped:\n  • Codex: a different 'agterm' skill is already there — left untouched";
    assert!(alert.body.ends_with(skipped), "{}", alert.body);
    press(cx, "OK");
    assert_eq!(fs::read_to_string(&skill).unwrap(), users);
}

#[gpui_kit::test]
fn the_command_line_tool_links_reads_back_current_and_refuses_a_real_file(cx: &mut TestAppContext) {
    let home = Home::new();
    let link = home.path(".local/bin/agtermctl");
    let tool = home.env().bundled_cli.unwrap();
    let cx = host(cx);
    home.serve(cx);
    assert_eq!(home.cli_status(), LinkStatus::Absent);

    start(cx, run_cli);
    let alert = shown(cx);
    assert_eq!(alert.heading, "Command Line Tool Installed");
    assert_eq!(
        alert.body,
        format!(
            "agtermctl was linked into {}. Open a new terminal and run “agtermctl --help”. {} is not on your PATH yet: \
             add it in your shell's startup file.",
            link.display(),
            link.parent().unwrap().display()
        )
    );
    press(cx, "OK");
    assert_eq!(fs::read_link(&link).unwrap(), tool);
    assert_eq!(home.cli_status(), LinkStatus::Current);

    // with the directory on the PATH the text is upstream's alone; the current link is left as it is
    let path = format!("{}:{}", home.env().vars["PATH"], link.parent().unwrap().display());
    home.set_var("PATH", &path);
    start(cx, run_cli);
    assert_eq!(
        shown(cx).body,
        format!(
            "agtermctl was linked into {}. Open a new terminal and run “agtermctl --help”.",
            link.display()
        )
    );
    press(cx, "OK");
    assert_eq!(home.cli_status(), LinkStatus::Current);

    // removed by the user, then linked again; an earlier build's link is replaced
    fs::remove_file(&link).unwrap();
    assert_eq!(home.cli_status(), LinkStatus::Absent);
    symlink("/an/old/build/agtermctl", &link).unwrap();
    assert_eq!(
        home.cli_status(),
        LinkStatus::OtherLink("/an/old/build/agtermctl".into())
    );
    start(cx, run_cli);
    press(cx, "OK");
    assert_eq!(home.cli_status(), LinkStatus::Current);

    // a real file there is the user's
    fs::remove_file(&link).unwrap();
    fs::write(&link, "a package's binary").unwrap();
    start(cx, run_cli);
    let alert = shown(cx);
    assert_eq!(alert.heading, "Install Failed");
    assert_eq!(
        alert.body,
        format!(
            "{} exists and is not a link agterm made, so agterm left it untouched. Remove it and run this again.",
            link.display()
        )
    );
    press(cx, "OK");
    assert_eq!(fs::read_to_string(&link).unwrap(), "a package's binary");
}

#[gpui_kit::test]
fn the_hooks_ask_for_the_opencode_version_and_move_between_its_plugins(cx: &mut TestAppContext) {
    let home = Home::new();
    home.mkdir(".config/opencode");
    // no `opencode` on the PATH: the probe reads the shell but no version
    let cx = host(cx);
    home.serve(cx);
    hooks_with_opencode_v1(cx, &home);
    opencode_v2_takes_over(cx, &home);
    opencode_skipped_and_current(cx, &home);
}

fn hooks_with_opencode_v1(cx: &mut VisualTestContext, home: &Home) {
    let v1 = home.path(".config/opencode/plugins/agterm-status.js");
    let script_dir = home.path(".config/agterm/agent-status");
    start(cx, run_hooks);
    assert_eq!(shown(cx), opencode_chooser(), "the probe could not tell the version");
    assert!(!script_dir.exists(), "nothing is written before the answer");
    press(cx, "OpenCode v1");
    let alert = shown(cx);
    assert_eq!(alert.heading, "Agent Status Hooks Installed");
    let scripts = format!("Scripts installed to {}.\n", script_dir.display());
    assert!(alert.body.starts_with(&scripts), "{}", alert.body);
    let plugin = format!(
        "OpenCode v1 status plugin installed to {}. Restart OpenCode.",
        v1.display()
    );
    assert!(alert.body.contains(&plugin), "{}", alert.body);
    assert_eq!(labels(&alert), ["OK"], "no Codex merge to do by hand");
    press(cx, "OK");
    assert!(script_dir.join(WRAPPER_NAME).is_file());
    let settings = home.read(".claude/settings.json");
    assert!(settings.contains(&script_dir.display().to_string()));
    assert!(home.read(".zshrc").contains(RC_MARKER_BEGIN));
    assert!(fs::read_to_string(&v1).unwrap().contains(Version::V1.marker()));
    // read back: every step current
    let plan = hooks::plan(&home.env(), &OpenCodeStart::Install(Version::V1), None).unwrap();
    assert_eq!(plan.claude, ClaudePlan::Current);
    assert!(plan.shell_rcs.is_empty());
    assert!(matches!(plan.opencode, OpenCodePlan::Install { contents: None, .. }));
}

/// `OpenCode` 2 takes over: its plugin goes in and agterm's v1 plugin comes out.
fn opencode_v2_takes_over(cx: &mut VisualTestContext, home: &Home) {
    let v1 = home.path(".config/opencode/plugins/agterm-status.js");
    let v2 = home.path(".config/opencode/plugins/agterm-v2/tui.js");
    start(cx, run_hooks);
    press(cx, "OpenCode v2");
    let alert = shown(cx);
    let plugin = format!("OpenCode v2 status plugin installed to {}.", v2.display());
    assert!(alert.body.contains(&plugin), "{}", alert.body);
    press(cx, "OK");
    assert!(fs::read_to_string(&v2).unwrap().contains(Version::V2.marker()));
    assert!(!files::entry_exists(&v1), "the managed v1 plugin is removed");
}

#[gpui_kit::test]
fn return_on_the_opencode_chooser_skips_opencode(cx: &mut TestAppContext) {
    let home = Home::new();
    home.mkdir(".config/opencode");
    let cx = host(cx);
    home.serve(cx);

    start(cx, run_hooks);
    let chooser = cx.update(|window, cx| front_alert(window.window_handle(), cx).expect("the chooser is up"));
    assert_eq!(
        chooser.read_with(cx, |alert, _| alert.spec().clone()),
        opencode_chooser()
    );
    assert_eq!(
        chooser.read_with(cx, |alert, _| alert.highlighted_button()),
        Some(0),
        "the keyboard starts on Skip OpenCode"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let alert = shown(cx);
    assert_eq!(alert.heading, "Agent Status Hooks Installed");
    assert!(
        alert.body.contains("OpenCode status plugin installation was skipped."),
        "{}",
        alert.body
    );
    press(cx, "OK");
    assert!(
        !home.path(".config/opencode/plugins").exists(),
        "no OpenCode plugin of either version is written"
    );
}

/// Escape skips `OpenCode` and leaves its plugin alone; the same version again is already current.
fn opencode_skipped_and_current(cx: &mut VisualTestContext, home: &Home) {
    let v2 = home.path(".config/opencode/plugins/agterm-v2/tui.js");
    start(cx, run_hooks);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let alert = shown(cx);
    assert!(alert.body.contains("OpenCode status plugin installation was skipped."));
    assert_eq!(alert.heading, "Agent Status Hooks Installed");
    press(cx, "OK");
    assert!(v2.is_file());

    start(cx, run_hooks);
    press(cx, "OpenCode v2");
    let current = format!("OpenCode v2 status plugin is already current at {}.", v2.display());
    assert!(shown(cx).body.contains(&current));
    press(cx, "OK");
    assert!(front(cx).is_none());
}

#[gpui_kit::test]
fn a_detected_opencode_installs_into_its_xdg_config_without_asking(cx: &mut TestAppContext) {
    let home = Home::new();
    let xdg = home.path("xdg");
    home.set_var("XDG_CONFIG_HOME", &xdg.display().to_string());
    fs::create_dir_all(xdg.join("opencode")).unwrap();
    home.opencode("opencode 2.3.1");
    let cx = host(cx);
    home.serve(cx);

    start(cx, run_hooks);
    let alert = shown(cx);
    assert_eq!(
        alert.heading, "Agent Status Hooks Installed",
        "no chooser: the probe told"
    );
    let plugin = xdg.join("opencode/plugins/agterm-v2/tui.js");
    assert!(
        alert
            .body
            .contains(&format!("OpenCode v2 status plugin installed to {}.", plugin.display()))
    );
    press(cx, "OK");
    assert!(plugin.is_file());
    assert!(
        !home.path(".config/opencode").exists(),
        "the default directory is not seeded"
    );
}

#[gpui_kit::test]
fn codex_hooks_of_the_users_send_the_alert_to_the_docs(cx: &mut TestAppContext) {
    let home = Home::new();
    let own = "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"echo done\"\n";
    home.write(".codex/config.toml", own);
    let cx = host(cx);
    home.serve(cx);

    start(cx, run_hooks);
    let alert = shown(cx);
    assert_eq!(alert.heading, "Agent Status Hooks Installed — with a warning");
    assert!(alert.body.contains(
        "Your ~/.codex/config.toml already defines its own hooks, so agterm left it untouched. See the Add Codex hooks \
         by hand section of the agterm docs for the block to add, then run /hooks in Codex."
    ));
    assert_eq!(labels(&alert), ["OK", "Open Docs"]);
    assert_eq!(cx.opened_url(), None);
    press(cx, "Open Docs");
    assert_eq!(cx.opened_url().as_deref(), Some(hooks::CODEX_MANUAL_DOCS_URL));
    assert_eq!(home.read(".codex/config.toml"), own, "the user's config is untouched");
}

#[gpui_kit::test]
fn a_second_hooks_start_while_one_asks_does_nothing(cx: &mut TestAppContext) {
    let home = Home::new();
    home.mkdir(".config/opencode");
    let cx = host(cx);
    home.serve(cx);

    start(cx, run_hooks);
    assert_eq!(shown(cx).heading, "Choose OpenCode Version");
    start(cx, run_hooks);
    start(cx, run_picked_hooks);
    press(cx, "Skip OpenCode");
    assert_eq!(shown(cx).heading, "Agent Status Hooks Installed");
    press(cx, "OK");
    assert!(front(cx).is_none(), "no second chooser queued behind the first");

    // once it ended a new start runs
    start(cx, run_hooks);
    assert_eq!(shown(cx).heading, "Choose OpenCode Version");
    press(cx, "Skip OpenCode");
    press(cx, "OK");
}

fn run_picked_hooks(window: AnyWindowHandle, cx: &mut App) {
    run_picked(window, false, true, cx);
}

#[gpui_kit::test]
fn the_welcome_runs_the_skill_then_the_hooks_after_its_alert(cx: &mut TestAppContext) {
    let home = Home::new();
    let script_dir = home.path(".config/agterm/agent-status");
    let cx = host(cx);
    home.serve(cx);

    cx.update(|window, cx| run_picked(window.window_handle(), true, true, cx));
    cx.run_until_parked();
    assert_eq!(shown(cx).heading, "Agent Skill Installed");
    assert!(
        home.path(".claude/skills/agterm/SKILL.md").is_file(),
        "Claude Code's when no agent is set up"
    );
    assert!(!script_dir.exists(), "the hooks wait for the skill's alert");
    press(cx, "OK");
    assert_eq!(shown(cx).heading, "Agent Status Hooks Installed");
    assert!(script_dir.is_dir());
    press(cx, "OK");
    assert!(front(cx).is_none());
}

#[gpui_kit::test]
fn a_failed_or_panicking_install_says_so_and_frees_the_hooks(cx: &mut TestAppContext) {
    let cx = host(cx);
    let no_home = Source::new(|| None);
    let panicking = Source::new(|| panic!("the install's work panicked"));
    for (source, text) in [(no_home, NO_HOME), (panicking, STOPPED)] {
        cx.update(|_, cx| source.install(cx));
        for run in [run_skill, run_cli, run_hooks] {
            start(cx, run);
            let alert = shown(cx);
            assert_eq!((alert.heading.as_str(), alert.body.as_str()), ("Install Failed", text));
            press(cx, "OK");
        }
        // the failed hooks install no longer counts as running
        let home = Home::new();
        home.serve(cx);
        start(cx, run_hooks);
        assert_eq!(shown(cx).heading, "Agent Status Hooks Installed", "after {text}");
        press(cx, "OK");
    }
}

#[gpui_kit::test]
fn a_tall_report_scrolls_under_its_buttons_and_three_buttons_stack(cx: &mut TestAppContext) {
    let cx = host(cx);
    cx.simulate_resize(gpui_kit::size(gpui_kit::px(420.), gpui_kit::px(320.)));
    let body = (0..60)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    cx.update(|window, cx| {
        open_alert(
            window,
            result_alert("Agent Status Hooks Installed", &body, true),
            |_, _, _| {},
            cx,
        );
    });
    cx.run_until_parked();
    let ok = cx.debug_bounds("alert-button-0").expect("OK is laid out");
    let docs = cx.debug_bounds("alert-button-1").expect("Open Docs is laid out");
    let text = cx.debug_bounds("alert-body").expect("the body is laid out");
    assert!(ok.bottom() <= gpui_kit::px(320.), "OK stays in the window: {ok:?}");
    assert!(text.bottom() <= ok.top(), "the body ends above the buttons");
    assert_eq!(ok.top(), docs.top(), "two buttons share a row");
    press(cx, "OK");

    cx.update(|window, cx| {
        open_alert(window, opencode_chooser(), |_, _, _| {}, cx);
    });
    cx.run_until_parked();
    let rows: Vec<_> = (0..3)
        .map(|index| {
            let id: &'static str = ["alert-button-0", "alert-button-1", "alert-button-2"][index];
            cx.debug_bounds(id).expect("the button is laid out")
        })
        .collect();
    assert!(
        rows[0].bottom() <= rows[1].top() && rows[1].bottom() <= rows[2].top(),
        "stacked, Skip first: {rows:?}"
    );
    assert_eq!(rows[0].size.width, rows[2].size.width, "each full width");
}

/// Names the temp home the child installs into; the child refuses any other.
const CHILD_HOME: &str = "AGTERM_GPUI_TEST_INSTALL_HOME";

#[test]
fn the_installers_read_home_and_xdg_config_home_from_the_process() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let xdg = dir.path().join("xdg");
    let bin = dir.path().join("bin");
    for directory in [home.join(".codex"), xdg.join("opencode"), bin.clone()] {
        fs::create_dir_all(directory).unwrap();
    }
    write_executable(&bin.join("login-shell"), FAKE_SHELL).unwrap();
    write_executable(&bin.join("opencode"), "#!/bin/sh\necho 2.0.0\n").unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "integrations::tests::child_installs_into_the_process_home",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg)
        .env("SHELL", bin.join("login-shell"))
        .env("PATH", &bin)
        .env(CHILD_HOME, &home)
        .status()
        .unwrap();
    assert!(status.success(), "the child's installs failed");
    assert!(
        home.join(".codex/skills/agterm/SKILL.md").is_file(),
        "the skill in $HOME"
    );
    assert!(home.join(".config/agterm/agent-status").join(WRAPPER_NAME).is_file());
    assert!(home.join(".codex/config.toml").is_file(), "Codex wired in $HOME");
    assert!(
        xdg.join("opencode/plugins/agterm-v2/tui.js").is_file(),
        "OpenCode 2's plugin under $XDG_CONFIG_HOME"
    );
}

#[gpui_kit::test]
#[ignore = "run by the_installers_read_home_and_xdg_config_home_from_the_process, with its temp HOME"]
fn child_installs_into_the_process_home(cx: &mut TestAppContext) {
    // only ever in the child the parent test started, whose HOME is the temp home it named: never the user's
    let Some(home) = std::env::var_os(CHILD_HOME).filter(|home| std::env::var_os("HOME").as_ref() == Some(home)) else {
        eprintln!("skipped: not in the parent test's temp home");
        return;
    };
    let home = PathBuf::from(home);
    let xdg = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    let cx = host(cx);
    // no Source installed: the app's own, the process environment
    cx.update(|window, cx| run_picked(window.window_handle(), true, true, cx));
    cx.run_until_parked();
    let skill = shown(cx);
    assert!(
        skill
            .body
            .contains(&format!("  • Codex: {}", home.join(".codex/skills/agterm").display())),
        "{}",
        skill.body
    );
    press(cx, "OK");
    let hooks = shown(cx);
    assert!(
        hooks.body.contains(&format!(
            "OpenCode v2 status plugin installed to {}.",
            xdg.join("opencode/plugins/agterm-v2/tui.js").display()
        )),
        "{}",
        hooks.body
    );
    press(cx, "OK");
}
