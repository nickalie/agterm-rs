//! `keymap.*` over a temp config dir and a fake process launcher: reload and list read-back, `keymap.run`'s refusals
//! and what a run starts, failure banners.

use std::sync::{Arc, Mutex};

use agterm_host::ProcessExit;
use agterm_host::fakes::FakeProcessLauncher;
use agterm_proto::command::KeymapCmd;
use agterm_proto::{Args, Command, ControlKeymapMenuItem};
use serde_json::{Value, json};

use super::{Fixture, UNKNOWN, args, failure, wire};
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
