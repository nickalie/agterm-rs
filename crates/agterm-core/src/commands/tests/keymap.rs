//! `keymap.*` over a temp config dir and a fake process launcher: reload and list read-back, `keymap.run`'s refusals
//! and what a run starts, failure banners.

use std::sync::{Arc, Mutex};

use agterm_host::ProcessExit;
use agterm_host::fakes::{FakeProcessLauncher, FakeSurface};
use agterm_proto::command::{KeymapCmd, SessionCmd};
use agterm_proto::{Args, Command, ControlKeymapMenuItem};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Fixture, UNKNOWN, args, failure, wire};
use crate::keymap::{CustomCommand, SurfaceOwner, TypingSurface};
use crate::paths::{ConfigLocator, MapEnv};
use crate::{CoreTask, NotificationEffect, ProcessHost};

/// The tasks exits posted, run on the core by [`Host::drain`].
pub(super) type Queue = Arc<Mutex<Vec<CoreTask>>>;

/// A fixture whose config dir is a temp dir and whose processes go to a fake launcher.
pub(super) struct Host {
    pub fixture: Fixture,
    pub launcher: Arc<FakeProcessLauncher>,
    pub queue: Queue,
    pub dir: tempfile::TempDir,
}

impl Host {
    pub fn new(extra: usize) -> Self {
        let mut fixture = Fixture::new(extra);
        // the seeded windows' own events are sequenced before any hook exists
        let _ = fixture.core.commit();
        let dir = tempfile::tempdir().unwrap();
        fixture.core.set_config_locator(ConfigLocator::capture(
            &MapEnv::new().with("AGTERM_STATE_DIR", dir.path()),
        ));
        let launcher = Arc::new(FakeProcessLauncher::new());
        let queue: Queue = Arc::default();
        let posted = Arc::clone(&queue);
        fixture.core.set_process_host(ProcessHost {
            launcher: Arc::clone(&launcher) as _,
            poster: Arc::new(move |task| posted.lock().unwrap().push(task)),
            socket: "/tmp/agterm-test.sock".to_owned(),
            cli_dir: Some("/opt/agterm/bin".into()),
            path: Some("/usr/bin:/bin".to_owned()),
        });
        Self {
            fixture,
            launcher,
            queue,
            dir,
        }
    }

    pub fn config(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join("config").join(name)
    }

    pub fn write(&self, name: &str, text: &str) {
        std::fs::create_dir_all(self.dir.path().join("config")).unwrap();
        std::fs::write(self.config(name), text).unwrap();
    }

    /// Runs the posted exit tasks, then commits as the main loop's settle would.
    pub fn drain(&mut self) {
        let tasks: Vec<CoreTask> = std::mem::take(&mut *self.queue.lock().unwrap());
        for task in tasks {
            task(&mut self.fixture.core);
            let _ = self.fixture.core.commit();
        }
    }

    fn keymap(&mut self, cmd: KeymapCmd, target: Option<&str>, args: Args) -> agterm_proto::Response {
        self.fixture.send(Command::Keymap(cmd), target, args)
    }
}

fn result(response: &agterm_proto::Response) -> Value {
    assert!(response.ok, "{response:?}");
    response.result.clone().unwrap_or(Value::Null)
}

fn app_banners(host: &mut Host) -> Vec<(String, String)> {
    host.fixture
        .core
        .take_notifications()
        .into_iter()
        .filter_map(|effect| match effect {
            NotificationEffect::App { identity, body, .. } => Some((identity, body)),
            _ => None,
        })
        .collect()
}

#[test]
fn list_without_a_file_reports_the_linux_builtins_alone() {
    let mut host = Host::new(0);
    assert_eq!(
        result(&host.keymap(KeymapCmd::Reload, None, Args::default())),
        json!({"count": 0})
    );
    let listing = &result(&host.keymap(KeymapCmd::List, None, Args::default()))["keymap"];
    assert_eq!(listing["path"], json!(host.config("keymap.conf").to_string_lossy()));
    assert_eq!(listing["commands"], json!([]));
    assert_eq!(listing["diagnostics"], json!([]));
    assert!(listing.get("menu").is_none());
    let actions = listing["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 49);
    let new_session = actions.iter().find(|action| action["action"] == "new_session").unwrap();
    assert_eq!(new_session, &json!({"action": "new_session", "chord": "ctrl+shift+t"}));
}

#[test]
fn reload_applies_the_edited_file_and_banners_its_problems() {
    let mut host = Host::new(0);
    host.write(
        "keymap.conf",
        "# conformance keymap\nmap ctrl+shift+f12|ctrl+shift+f10 new_session\ncommand \"plain\" true\n\
         command \"keyed\" ctrl+shift+f11 --error-hud --error-position top-left --error-pane right true\n\
         bogus line here\n",
    );
    let before = host.fixture.core.keymap_generation();
    assert_eq!(
        result(&host.keymap(KeymapCmd::Reload, None, Args::default())),
        json!({"count": 1})
    );
    assert_ne!(host.fixture.core.keymap_generation(), before);
    assert_eq!(
        app_banners(&mut host),
        [(
            "keymap-diagnostics".to_owned(),
            "1 issue — see Settings ▸ Key Mapping".to_owned()
        )]
    );
    let listing = &result(&host.keymap(KeymapCmd::List, None, Args::default()))["keymap"];
    let new_session = listing["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["action"] == "new_session")
        .unwrap();
    assert_eq!(
        new_session,
        &json!({"action": "new_session", "chord": "ctrl+shift+f12", "alternates": ["ctrl+shift+f10"], "overridden": true})
    );
    assert_eq!(
        listing["commands"],
        json!([
            {"name": "plain", "errorHud": false, "errorPosition": "center", "repeats": false},
            {"name": "keyed", "shortcut": "ctrl+shift+f11", "errorHud": true, "errorPosition": "top-left",
             "errorPane": "right", "repeats": false}
        ])
    );
    assert_eq!(
        listing["diagnostics"],
        json!([{"line": 5, "message": "unknown verb 'bogus'"}])
    );
}

#[test]
fn an_unreadable_file_reports_the_error_and_loads_nothing() {
    let mut host = Host::new(0);
    host.write("keymap.conf", "command \"plain\" true\n");
    assert!(host.keymap(KeymapCmd::Reload, None, Args::default()).ok);
    std::fs::write(host.config("keymap.conf"), b"command \"x\" \xff\n").unwrap();
    assert_eq!(
        result(&host.keymap(KeymapCmd::Reload, None, Args::default())),
        json!({"count": 1})
    );
    let listing = &result(&host.keymap(KeymapCmd::List, None, Args::default()))["keymap"];
    assert_eq!(listing["commands"], json!([]));
    let message = listing["diagnostics"][0]["message"].as_str().unwrap();
    assert!(message.starts_with("could not read keymap.conf: "), "{message}");
    assert_eq!(listing["diagnostics"][0]["line"], 0);
}

#[test]
fn the_live_menu_rides_the_listing() {
    let mut host = Host::new(0);
    host.fixture.core.set_menu_reader(Box::new(|| {
        vec![ControlKeymapMenuItem {
            menu: "File".to_owned(),
            title: "New Session".to_owned(),
            chord: "ctrl+shift+t".to_owned(),
            selector: Some("menu.menu-newSession".to_owned()),
            enabled: None,
        }]
    }));
    let listing = &result(&host.keymap(KeymapCmd::List, None, Args::default()))["keymap"];
    assert_eq!(
        listing["menu"],
        json!([{"menu": "File", "title": "New Session", "chord": "ctrl+shift+t", "selector": "menu.menu-newSession"}])
    );
}

#[test]
fn run_refusals() {
    let mut host = Host::new(0);
    host.write("keymap.conf", "command \"plain\" true\n");
    assert!(host.keymap(KeymapCmd::Reload, None, Args::default()).ok);
    for (target, name, error) in [
        (None, None, "keymap.run requires a command name"),
        (None, Some(""), "keymap.run requires a command name"),
        (None, Some("nope"), "no custom command named nope"),
        (None, Some(" plain"), "no custom command named  plain"),
        (Some(UNKNOWN), Some("plain"), "no such session: zzz"),
    ] {
        let response = host.keymap(KeymapCmd::Run, target, args(|a| a.name = name.map(str::to_owned)));
        assert_eq!(response, failure(error), "{name:?}");
    }
    assert!(host.launcher.launches().is_empty());
}

#[test]
fn run_without_a_launcher_is_unavailable() {
    let mut fixture = Fixture::new(0);
    let dir = tempfile::tempdir().unwrap();
    fixture.core.set_config_locator(ConfigLocator::capture(
        &MapEnv::new().with("AGTERM_STATE_DIR", dir.path()),
    ));
    std::fs::create_dir_all(dir.path().join("config")).unwrap();
    std::fs::write(dir.path().join("config/keymap.conf"), "command \"plain\" true\n").unwrap();
    fixture.core.reload_keymap();
    let response = fixture.send(
        Command::Keymap(KeymapCmd::Run),
        None,
        args(|a| a.name = Some("plain".into())),
    );
    assert_eq!(response, failure("custom commands are not available"));
}

#[test]
fn run_starts_the_expanded_command_with_the_sessions_context() {
    let mut host = Host::new(2);
    host.write(
        "keymap.conf",
        "command \"mark\" printf '%s' {AGT_SESSION_ID} \"$AGT_PANE\"\n",
    );
    assert!(host.keymap(KeymapCmd::Reload, None, Args::default()).ok);
    let target = host.fixture.sessions[1];
    let response = host.keymap(
        KeymapCmd::Run,
        Some(&wire(target)),
        args(|a| a.name = Some("mark".into())),
    );
    assert_eq!(result(&response), json!({"id": wire(target)}));
    let launches = host.launcher.launches();
    let [launch] = launches.as_slice() else {
        panic!("{launches:?}")
    };
    assert_eq!(launch.program, std::path::Path::new("/bin/sh"));
    assert_eq!(
        launch.args,
        ["-c".to_owned(), format!("printf '%s' {} \"$AGT_PANE\"", wire(target))]
    );
    assert_eq!(launch.cwd.as_deref(), Some(std::path::Path::new("/tmp")));
    assert!(!launch.capture_stderr && launch.stdin.is_none());
    let env = |key: &str| {
        launch
            .env
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(env("AGT_SESSION_ID"), Some(wire(target)));
    assert_eq!(env("AGT_PANE").as_deref(), Some("left"));
    assert_eq!(env("AGT_WINDOW_ID"), Some(wire(host.fixture.window)));
    assert_eq!(env("AGT_SOCKET").as_deref(), Some("/tmp/agterm-test.sock"));
    assert_eq!(env("PATH").as_deref(), Some("/opt/agterm/bin:/usr/bin:/bin"));
    // the active session when none is named
    let active = host.fixture.selected().unwrap();
    assert_eq!(
        result(&host.keymap(KeymapCmd::Run, None, args(|a| a.name = Some("mark".into())))),
        json!({"id": wire(active)})
    );
}

#[test]
fn a_failing_run_banners_once_it_exits() {
    let mut host = Host::new(1);
    host.write("keymap.conf", "command \"boom\" --error-hud exit 3\n");
    assert!(host.keymap(KeymapCmd::Reload, None, Args::default()).ok);
    assert!(
        host.keymap(KeymapCmd::Run, None, args(|a| a.name = Some("boom".into())))
            .ok
    );
    assert!(host.launcher.launches()[0].capture_stderr);
    let pid = host.launcher.running()[0];
    assert!(app_banners(&mut host).is_empty());
    host.launcher.finish(
        pid,
        ProcessExit {
            status: 3,
            stderr_tail: b"it broke\n".to_vec(),
            stdin_failure: None,
        },
    );
    host.drain();
    assert_eq!(
        app_banners(&mut host),
        [("command-failure:boom".to_owned(), "boom (exit 3)".to_owned())]
    );
    // a clean exit reports nothing
    assert!(
        host.keymap(KeymapCmd::Run, None, args(|a| a.name = Some("boom".into())))
            .ok
    );
    let pid = host.launcher.running()[0];
    host.launcher.finish(pid, ProcessExit::default());
    host.drain();
    assert!(app_banners(&mut host).is_empty());
}

#[test]
fn a_vanished_directory_or_a_launch_error_refuses_the_run() {
    let mut host = Host::new(1);
    host.write("keymap.conf", "command \"mark\" true\n");
    assert!(host.keymap(KeymapCmd::Reload, None, Args::default()).ok);
    host.launcher.fail_with(Some(std::io::ErrorKind::NotFound));
    let response = host.keymap(KeymapCmd::Run, None, args(|a| a.name = Some("mark".into())));
    assert_eq!(response, failure("mark did not start: fake launch of /bin/sh failed"));
    assert_eq!(app_banners(&mut host).len(), 1);
}

#[test]
fn a_run_counts_toward_usage_only_with_persistence() {
    // in-memory cores write nothing; the store itself is covered in `keymap::usage`
    let mut host = Host::new(1);
    host.write("keymap.conf", "command \"mark\" true\n");
    assert!(host.keymap(KeymapCmd::Reload, None, Args::default()).ok);
    assert!(
        host.keymap(KeymapCmd::Run, None, args(|a| a.name = Some("mark".into())))
            .ok
    );
    assert!(!host.dir.path().join("custom-command-usage.json").exists());
}

#[test]
fn launch_writes_the_starter_once_and_reads_it_clean() {
    let mut host = Host::new(0);
    host.fixture.core.load_keymap_at_launch();
    let starter = std::fs::read_to_string(host.config("keymap.conf")).unwrap();
    assert!(starter.starts_with("# agterm keymap"));
    assert!(host.fixture.core.keymap_diagnostics().is_empty());
    host.write("keymap.conf", "command \"kept\" true\n");
    host.fixture.core.load_keymap_at_launch();
    assert_eq!(host.fixture.core.keymap().commands()[0].name, "kept");
}

#[test]
fn a_session_directory_that_is_gone_refuses_the_run() {
    // the seeded session sits at the fixture's home, which does not exist here
    let mut host = Host::new(0);
    host.write("keymap.conf", "command \"mark\" true\n");
    assert!(host.keymap(KeymapCmd::Reload, None, Args::default()).ok);
    let response = host.keymap(KeymapCmd::Run, None, args(|a| a.name = Some("mark".into())));
    assert_eq!(
        response,
        failure("mark did not start: session directory /home/u no longer exists")
    );
    assert!(host.launcher.launches().is_empty());
}

/// Upstream's `testSpawnFailurePostsOnlyWhenOptedIn`: a command that cannot start in a removed directory opens the
/// failure panel only when its line opts in, at the line's position and pane, with no detail.
#[test]
fn a_missing_directory_opens_the_failure_panel_only_when_opted_in() {
    let mut host = Host::new(0);
    let helper = host.dir.path().join("hud.sh");
    std::fs::write(&helper, "#!/bin/sh\n").unwrap();
    host.fixture.core.set_hud_helper(helper, host.dir.path().join("bodies"));
    host.write(
        "keymap.conf",
        "command \"quiet\" true\n\
         command \"loud\" --error-hud --error-position top-left --error-pane left true\n",
    );
    assert!(host.keymap(KeymapCmd::Reload, None, Args::default()).ok);
    let missing = host.dir.path().join("missing-directory").to_string_lossy().into_owned();
    let window = host.fixture.window;
    let session = host.fixture.selected().unwrap();
    host.fixture
        .store_mut()
        .unwrap()
        .session_mut(session)
        .unwrap()
        .current_cwd = Some(missing.clone());
    let [quiet, loud] = host.fixture.core.keymap().commands() else {
        panic!("two commands")
    };
    let (quiet, loud) = (quiet.clone(), loud.clone());
    let reason = format!("session directory {missing} no longer exists");

    assert_eq!(
        host.fixture.core.run_custom_command(&quiet, window, session),
        Some(reason.clone())
    );
    assert!(host.fixture.store().session(session).unwrap().overlay.hud.is_none());

    assert_eq!(
        host.fixture.core.run_custom_command(&loud, window, session),
        Some(reason)
    );
    let state = host.fixture.store().session(session).unwrap();
    let hud = state.overlay.hud.as_ref().expect("the failure panel");
    assert_eq!(
        hud.spec.message,
        format!("loud: session directory {missing} no longer exists")
    );
    assert_eq!(hud.spec.detail, None);
    assert_eq!(hud.spec.position, crate::hud::HudPosition::TopLeft);
    assert_eq!(hud.pane_identity, Some(state.pane_identity));
    assert!(host.launcher.launches().is_empty());
}

#[test]
fn a_refused_editor_leaves_the_open_editors_reload_alone() {
    use crate::app_actions::ConfigFile;
    use crate::palette_catalog::PaletteCommand;
    let mut host = Host::new(0);
    let window = host.fixture.window;
    let session = host.fixture.selected().unwrap();
    host.fixture
        .core
        .perform_menu_command(window, PaletteCommand::EditKeymap);
    let state = host.fixture.store().session(session).unwrap();
    assert!(state.overlay.program_active());
    let generation = state.overlay.generation;
    // the slot is held by the keymap editor, so the hooks editor is refused
    host.fixture
        .core
        .perform_menu_command(window, PaletteCommand::EditHooks);
    assert_eq!(
        host.fixture.store().session(session).unwrap().overlay.generation,
        generation
    );
    assert_eq!(
        host.fixture.core.config_edit_ended(session, generation),
        Some(ConfigFile::Keymap)
    );
}

/// What the last launch exported as `key`.
fn last_env(host: &Host, key: &str) -> Option<String> {
    let launches = host.launcher.launches();
    let launch = launches.last().expect("a launch");
    launch
        .env
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

/// A host with a `mark` command and session `typed` (an added one at `/tmp`, not the selected one) whose main pane
/// carries terminal `L` with a selection of its own.
fn chord_host() -> (Host, CustomCommand, Uuid) {
    let mut host = Host::new(2);
    host.write("keymap.conf", "command \"mark\" true\n");
    assert!(host.keymap(KeymapCmd::Reload, None, Args::default()).ok);
    let command = host.fixture.core.keymap().commands()[0].clone();
    let selected = host.fixture.selected().unwrap();
    let typed = *host.fixture.sessions.iter().rev().find(|id| **id != selected).unwrap();
    let surface = FakeSurface::new("L");
    surface.probe().set_selection(Some("pane-selection"));
    let session = host.fixture.store_mut().unwrap().session_mut(typed).unwrap();
    session.surface.replace(Box::new(surface));
    (host, command, typed)
}

fn typed_in(owner: SurfaceOwner, selection: Option<&str>) -> TypingSurface {
    TypingSurface {
        owner,
        selection: selection.map(str::to_owned),
    }
}

#[test]
fn a_chord_runs_for_the_terminal_typed_in_not_the_selected_session() {
    // a zoomed session shows and takes the keyboard without being selected: the chord is its own
    let (mut host, command, typed) = chord_host();
    let window = host.fixture.window;
    let owner = SurfaceOwner::Pane {
        session: typed,
        token: "L".into(),
    };
    host.fixture
        .core
        .run_custom_command_from_keybind(&command, Some(window), Some(typed_in(owner, Some("typed"))));
    assert_eq!(last_env(&host, "AGT_SESSION_ID"), Some(wire(typed)));
    assert_eq!(last_env(&host, "AGT_PANE").as_deref(), Some("left"));
    assert_eq!(last_env(&host, "AGT_PANE_ID").as_deref(), Some("L"));
    assert_eq!(last_env(&host, "AGT_SELECTION").as_deref(), Some("typed"));
    assert_eq!(last_env(&host, "AGT_WINDOW_ID"), Some(wire(window)));
    // the scratch names itself
    let session = host.fixture.store_mut().unwrap().session_mut(typed).unwrap();
    session.scratch_surface.replace(Box::new(FakeSurface::new("S")));
    let owner = SurfaceOwner::Pane {
        session: typed,
        token: "S".into(),
    };
    host.fixture
        .core
        .run_custom_command_from_keybind(&command, Some(window), Some(typed_in(owner, None)));
    assert_eq!(last_env(&host, "AGT_PANE").as_deref(), Some("scratch"));
    assert_eq!(last_env(&host, "AGT_PANE_ID").as_deref(), Some("S"));
    assert_eq!(last_env(&host, "AGT_SELECTION").as_deref(), Some(""));
}

#[test]
fn an_overlay_chord_reads_the_overlays_selection_and_names_the_pane_underneath() {
    let (mut host, command, typed) = chord_host();
    let window = host.fixture.window;
    let opened = host.fixture.session(
        SessionCmd::OverlayOpen,
        Some(&wire(typed)),
        args(|a| a.command = Some("exec cat".into())),
    );
    assert!(opened.ok, "{opened:?}");
    let generation = host.fixture.store().session(typed).unwrap().overlay.generation;
    let overlay = |selection: Option<&str>| {
        Some(typed_in(
            SurfaceOwner::Overlay {
                session: typed,
                generation,
                pane: false,
            },
            selection,
        ))
    };
    host.fixture
        .core
        .run_custom_command_from_keybind(&command, Some(window), overlay(Some("in-overlay")));
    assert_eq!(last_env(&host, "AGT_SESSION_ID"), Some(wire(typed)));
    assert_eq!(last_env(&host, "AGT_PANE").as_deref(), Some("left"));
    assert_eq!(last_env(&host, "AGT_PANE_ID").as_deref(), Some("L"));
    assert_eq!(last_env(&host, "AGT_SELECTION").as_deref(), Some("in-overlay"));
    // an overlay with nothing selected never reads the pane under it
    host.fixture
        .core
        .run_custom_command_from_keybind(&command, Some(window), overlay(None));
    assert_eq!(last_env(&host, "AGT_SELECTION").as_deref(), Some(""));
    // a shown scratch is what lies under the overlay
    host.fixture
        .store_mut()
        .unwrap()
        .session_mut(typed)
        .unwrap()
        .scratch_active = true;
    host.fixture
        .core
        .run_custom_command_from_keybind(&command, Some(window), overlay(None));
    assert_eq!(last_env(&host, "AGT_PANE").as_deref(), Some("scratch"));
    // a closed overlay's terminal no longer speaks for the session: the selected session does
    host.fixture
        .store_mut()
        .unwrap()
        .session_mut(typed)
        .unwrap()
        .scratch_active = false;
    assert!(
        host.fixture
            .session(SessionCmd::OverlayClose, Some(&wire(typed)), Args::default())
            .ok
    );
    host.fixture
        .core
        .run_custom_command_from_keybind(&command, Some(window), overlay(Some("gone")));
    let selected = host.fixture.selected().unwrap();
    assert_eq!(last_env(&host, "AGT_SESSION_ID"), Some(wire(selected)));
}

#[test]
fn a_chord_off_any_session_terminal_runs_for_the_active_session_else_sessionless() {
    let (mut host, command, typed) = chord_host();
    let window = host.fixture.window;
    let selected = host.fixture.selected().unwrap();
    // the quick terminal, a page or a HUD: the active session's focused pane, as the palette runs it
    host.fixture
        .core
        .run_custom_command_from_keybind(&command, Some(window), None);
    assert_eq!(last_env(&host, "AGT_SESSION_ID"), Some(wire(selected)));
    assert_eq!(last_env(&host, "AGT_PANE").as_deref(), Some("left"));
    // a terminal whose token no pane carries any more is no session's
    let stale = SurfaceOwner::Pane {
        session: typed,
        token: "gone".into(),
    };
    host.fixture
        .core
        .run_custom_command_from_keybind(&command, Some(window), Some(typed_in(stale, None)));
    assert_eq!(last_env(&host, "AGT_SESSION_ID"), Some(wire(selected)));
    // an emptied window: only a launcher naming no session token runs
    let mut host = Host::new(0);
    host.write(
        "keymap.conf",
        "command \"launch\" true\ncommand \"scoped\" echo {AGT_SESSION_ID}\n",
    );
    assert!(host.keymap(KeymapCmd::Reload, None, Args::default()).ok);
    let only = host.fixture.selected().unwrap();
    assert!(host.fixture.store_mut().unwrap().close_session(only).is_some());
    let commands = host.fixture.core.keymap().commands().to_vec();
    let window = host.fixture.window;
    host.fixture
        .core
        .run_custom_command_from_keybind(&commands[1], Some(window), None);
    assert!(host.launcher.launches().is_empty());
    host.fixture
        .core
        .run_custom_command_from_keybind(&commands[0], Some(window), None);
    assert_eq!(last_env(&host, "AGT_SESSION_ID").as_deref(), Some(""));
    assert_eq!(last_env(&host, "AGT_WINDOW_ID"), Some(wire(window)));
}
