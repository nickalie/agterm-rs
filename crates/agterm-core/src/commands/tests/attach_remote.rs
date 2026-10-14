//! File ▸ Attach Remote: `remotes.conf` as the core follows it, its problems banner, and the picker and HUD flow
//! (upstream's `RemotesWatcherTests`, `RemoteAttachTests` and `notifyRemotesDiagnostics`), over a fake ssh runner that
//! answers only when the test says so.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use agterm_proto::command::SessionCmd;
use agterm_proto::{Command, ControlPickOutcome, ControlPickResult, Request};
use serde_json::json;
use uuid::Uuid;

use super::{Fixture, args, wire};
use crate::Core;
use crate::commands::notify::NotificationEffect;
use crate::hud::{HudSpec, HudSpinner};
use crate::modal::PendingPick;
use crate::palette_catalog::PaletteFacts;
use crate::persist::AppSettings;
use crate::remote::tree::{RemoteCommandResult, RemoteCommandRunner};
use crate::settings_model::SettingsChange;

const REMOTE_SESSION: &str = "0A0B0C0D-0000-0000-0000-0000000000B1";

// MARK: - fakes

/// Holds every remote command until the test answers it, in call order (upstream's `FakeAttacher.answerTree`).
#[derive(Default)]
struct GatedRunner {
    state: Mutex<Gate>,
    answered: Condvar,
}

#[derive(Default)]
struct Gate {
    /// The host of every command run, in order.
    hosts: Vec<String>,
    /// The answers given so far, by call.
    answers: Vec<Option<RemoteCommandResult>>,
}

impl RemoteCommandRunner for GatedRunner {
    fn run(&self, argv: &[String], _deadline: Duration) -> RemoteCommandResult {
        let mut gate = self.state.lock().unwrap();
        // the destination is the argument before the remote command line
        let host = argv.get(argv.len().saturating_sub(2)).cloned().unwrap_or_default();
        let call = gate.hosts.len();
        gate.hosts.push(host);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(answer) = gate.answers.get_mut(call).and_then(Option::take) {
                return answer;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return RemoteCommandResult {
                    status: -1,
                    stdout: String::new(),
                    stderr: "unanswered".to_owned(),
                };
            }
            gate = self.answered.wait_timeout(gate, left).unwrap().0;
        }
    }
}

impl GatedRunner {
    fn hosts(&self) -> Vec<String> {
        self.state.lock().unwrap().hosts.clone()
    }

    fn answer(&self, call: usize, result: RemoteCommandResult) {
        let mut gate = self.state.lock().unwrap();
        if gate.answers.len() <= call {
            gate.answers.resize(call + 1, None);
        }
        gate.answers[call] = Some(result);
        self.answered.notify_all();
    }
}

fn listing(sessions: &[serde_json::Value]) -> RemoteCommandResult {
    RemoteCommandResult {
        status: 0,
        stdout: json!({"ok": true, "result": {"remote": {
            "endpoint": {"executable": "/zmx", "socketDirectory": "/tmp/zmx"},
            "sessions": sessions,
        }}})
        .to_string(),
        stderr: String::new(),
    }
}

/// Upstream's `build`: one pane running vim, with a context; its daemon is one agterm can address.
fn build_session() -> serde_json::Value {
    let daemon = crate::zmx::daemon_name(Uuid::from_u128(0xd1));
    json!({
        "id": REMOTE_SESSION, "name": "build", "windowID": "w1", "windowName": "main", "workspaceID": "ws1",
        "workspaceName": "work", "context": "release prep", "cwd": "/repo",
        "panes": [{"pane": "left", "daemon": daemon, "foreground": ["/usr/bin/vim", "a.txt"]}]
    })
}

fn refused(stderr: &str) -> RemoteCommandResult {
    RemoteCommandResult {
        status: 255,
        stdout: String::new(),
        stderr: stderr.to_owned(),
    }
}

/// A window whose config directory is `config`, over a gated runner.
struct Attach {
    fixture: Fixture,
    runner: Arc<GatedRunner>,
    config: tempfile::TempDir,
    /// The window's selected session, which the attach's panels go over.
    session: Uuid,
}

impl Attach {
    fn new(remotes: Option<&str>) -> Self {
        Self::with(remotes, AppSettings::default())
    }

    fn with(remotes: Option<&str>, settings: AppSettings) -> Self {
        let config = tempfile::tempdir().unwrap();
        if let Some(text) = remotes {
            std::fs::write(config.path().join("remotes.conf"), text).unwrap();
        }
        let settings = AppSettings {
            config_directory: Some(config.path().to_string_lossy().into_owned()),
            ..settings
        };
        let mut fixture = Fixture::with_settings(0, settings);
        // the attach's panels are HUDs, which need the painter
        let helper = config.path().join("hud.sh");
        std::fs::write(&helper, "#!/bin/sh\n").unwrap();
        fixture.core.set_hud_helper(helper, config.path().join("bodies"));
        let runner = Arc::new(GatedRunner::default());
        fixture.core.set_remote_runner(runner.clone());
        let session = fixture.store().selected_session_id().unwrap();
        Self {
            fixture,
            runner,
            config,
            session,
        }
    }

    fn core(&mut self) -> &mut Core {
        &mut self.fixture.core
    }

    fn pending(&self) -> Option<PendingPick> {
        self.fixture
            .core
            .modal_slot(self.fixture.window)
            .and_then(|slot| slot.pending().cloned())
    }

    /// The panel over the starting session, while one shows.
    fn hud(&self) -> Option<HudSpec> {
        self.fixture
            .store()
            .session(self.session)
            .and_then(|session| session.overlay.effective_hud_spec())
    }

    /// Ticks the flows until `done` holds.
    fn drive(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        for _ in 0..400 {
            self.fixture.core.remote_tick(Instant::now());
            if done(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{what}: hud {:?}", self.hud());
    }

    /// Ticks long enough for an answered command to land, for checks that nothing happens.
    fn idle(&mut self) {
        for _ in 0..20 {
            self.fixture.core.remote_tick(Instant::now());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_runs(&mut self, count: usize) {
        let runner = Arc::clone(&self.runner);
        self.drive("the remote command starts", |_| runner.hosts().len() == count);
    }

    fn resolve(&mut self, pick: &PendingPick, outcome: ControlPickResult) {
        let window = self.fixture.window;
        self.fixture.core.resolve_pick(window, &pick.id, outcome);
    }

    fn cancel(&mut self, pick: &PendingPick) {
        self.resolve(pick, ControlPickResult::outcome(ControlPickOutcome::Cancelled));
    }

    fn banners(&mut self) -> Vec<String> {
        self.fixture
            .core
            .take_notifications()
            .into_iter()
            .filter_map(|effect| match effect {
                NotificationEffect::App { identity, title, body } => Some(format!("{identity}|{title}|{body}")),
                _ => None,
            })
            .collect()
    }

    fn remotes_file(&self) -> PathBuf {
        self.config.path().join("remotes.conf")
    }

    fn destinations(&self) -> Vec<String> {
        self.fixture
            .core
            .configured_remotes()
            .iter()
            .map(|entry| entry.destination.clone())
            .collect()
    }
}

fn progress(text: &str) -> HudSpec {
    HudSpec {
        spinner: Some(HudSpinner::Bar),
        hide_after: Some(30.0),
        ..HudSpec::new(format!("Attach Remote: {text}"))
    }
}

fn failure(text: &str) -> HudSpec {
    HudSpec {
        hide_after: Some(5.0),
        ..HudSpec::new(format!("Attach Remote: {text}"))
    }
}

fn replace(path: &Path, text: &str) {
    let staged = path.with_extension("tmp");
    std::fs::write(&staged, text).unwrap();
    std::fs::rename(&staged, path).unwrap();
}

// MARK: - remotes.conf and its banner

#[test]
fn the_launch_reports_a_file_already_broken_once() {
    let mut attach = Attach::new(Some("studio\n-bad\n"));
    assert_eq!(
        attach.destinations(),
        ["studio"],
        "the core reads the file it was built over"
    );
    assert!(attach.banners().is_empty(), "construction reports nothing");
    attach.core().load_remotes_at_launch();
    assert_eq!(
        attach.banners(),
        ["remotes-diagnostics|Remotes|1 issue in remotes.conf"]
    );
    attach.core().refresh_remotes(true);
    assert!(
        attach.banners().is_empty(),
        "a file that stays broken is not reported again"
    );
}

#[test]
fn a_clean_file_at_launch_posts_nothing_and_an_edit_that_breaks_it_does() {
    let mut attach = Attach::new(Some("studio\n"));
    attach.core().load_remotes_at_launch();
    assert!(attach.banners().is_empty());
    replace(&attach.remotes_file(), "-bad\n-worse\n");
    attach.core().refresh_remotes(true);
    assert_eq!(
        attach.banners(),
        ["remotes-diagnostics|Remotes|2 issues in remotes.conf"]
    );
    assert!(attach.destinations().is_empty());
}

#[test]
fn the_banner_waits_for_the_banners_setting() {
    let settings = AppSettings {
        notifications_enabled: Some(false),
        ..AppSettings::default()
    };
    let mut attach = Attach::with(Some("-bad\n"), settings);
    attach.core().load_remotes_at_launch();
    assert!(attach.banners().is_empty());
}

#[test]
fn the_menu_row_follows_the_file_once_it_is_read_again() {
    let mut attach = Attach::new(None);
    let window = attach.fixture.window;
    let has_remotes = |core: &Core| core.palette_context(window).has(PaletteFacts::HAS_REMOTES);
    assert!(!has_remotes(&attach.fixture.core), "no file lists nothing");
    std::fs::write(attach.remotes_file(), "studio\n").unwrap();
    attach.core().commit();
    assert!(
        !has_remotes(&attach.fixture.core),
        "a settle alone does not read the disk"
    );
    attach.core().refresh_remotes(true);
    assert!(has_remotes(&attach.fixture.core));
    std::fs::remove_file(attach.remotes_file()).unwrap();
    attach.core().refresh_remotes(true);
    assert!(!has_remotes(&attach.fixture.core), "a deleted file");
    assert!(attach.banners().is_empty(), "a missing file is no problem");
}

#[test]
fn a_config_directory_switch_reads_the_new_file_and_reports_nothing() {
    let mut attach = Attach::new(Some("studio\n"));
    let remotes_banners = |attach: &mut Attach| -> Vec<String> {
        attach
            .banners()
            .into_iter()
            .filter(|banner| banner.starts_with("remotes-diagnostics"))
            .collect()
    };
    assert!(remotes_banners(&mut attach).is_empty(), "a clean file at the start");
    // from a clean file to a broken one: the edge a refresh reports, which a switch must not
    let broken = attach.config.path().join("broken");
    std::fs::create_dir(&broken).unwrap();
    std::fs::write(broken.join("remotes.conf"), "mini\n-bad\n").unwrap();
    assert!(attach.core().change_settings(SettingsChange::ConfigDirectory(Some(
        broken.to_string_lossy().into_owned()
    ))));
    assert_eq!(attach.destinations(), ["mini"], "the broken file switched to");
    assert!(
        remotes_banners(&mut attach).is_empty(),
        "a switch from a clean file to a broken one reports nothing"
    );
    attach.core().refresh_remotes(true);
    assert!(
        remotes_banners(&mut attach).is_empty(),
        "the switch counted the episode: a later read of the same file reports nothing"
    );

    let later = attach.config.path().join("later/nested");
    assert!(attach.core().change_settings(SettingsChange::ConfigDirectory(Some(
        later.to_string_lossy().into_owned()
    ))));
    assert!(attach.destinations().is_empty());
    assert_eq!(attach.fixture.core.watched_remotes_path(), later.join("remotes.conf"));
    std::fs::create_dir_all(&later).unwrap();
    std::fs::write(later.join("remotes.conf"), "mini\n-bad\n").unwrap();
    attach.core().refresh_remotes(true);
    assert_eq!(
        attach.destinations(),
        ["mini"],
        "a file in a directory that did not exist"
    );
    attach.banners();

    replace(&attach.remotes_file(), "other\n-bad\n");
    let back = attach.config.path().to_string_lossy().into_owned();
    assert!(
        attach
            .core()
            .change_settings(SettingsChange::ConfigDirectory(Some(back)))
    );
    assert_eq!(
        attach.destinations(),
        ["other"],
        "the file of the directory switched to"
    );
    assert!(remotes_banners(&mut attach).is_empty(), "a switch never reports");
}

// MARK: - the flow

#[test]
fn no_remotes_does_nothing() {
    let mut attach = Attach::new(None);
    let window = attach.fixture.window;
    attach.core().attach_remote(window);
    attach.idle();
    assert!(attach.pending().is_none());
    assert!(attach.runner.hosts().is_empty());
    assert_eq!(attach.fixture.core.remote_wake(), None);
}

#[test]
fn a_single_remote_is_listed_without_asking() {
    let mut attach = Attach::new(Some("studio Mac Studio\n"));
    let window = attach.fixture.window;
    attach.core().attach_remote(window);
    attach.wait_for_runs(1);
    assert_eq!(attach.runner.hosts(), ["studio"]);
    assert!(attach.pending().is_none());
    assert_eq!(attach.hud(), Some(progress("listing sessions on studio…")));
    attach.runner.answer(0, listing(&[]));
    attach.drive("the flow ends", |attach| attach.fixture.core.remote_wake().is_none());
}

#[test]
fn dismissing_the_machine_picker_contacts_nothing() {
    let mut attach = Attach::new(Some("studio\nmini\n"));
    let window = attach.fixture.window;
    attach.core().attach_remote(window);
    let hosts = attach.pending().expect("the machine picker opens");
    assert_eq!(hosts.prompt.as_deref(), Some("Attach from which Mac?"));
    assert_eq!(hosts.items[1].label, "mini");
    assert_eq!(hosts.items[1].subtitle, None, "a bare destination has no subtitle");
    attach.cancel(&hosts);
    attach.idle();
    assert!(attach.pending().is_none());
    assert!(attach.runner.hosts().is_empty());
    assert_eq!(attach.hud(), None);
    assert_eq!(attach.fixture.core.remote_wake(), None, "the flow ended");
}

#[test]
fn the_progress_panel_is_gone_before_the_session_picker_and_the_attach_closes_its_own() {
    let mut attach = Attach::new(Some("studio\n"));
    let window = attach.fixture.window;
    attach.core().attach_remote(window);
    attach.wait_for_runs(1);
    attach.runner.answer(0, listing(&[build_session()]));
    attach.drive("the session picker opens", |attach| attach.pending().is_some());
    assert_eq!(attach.hud(), None, "the progress panel is gone before the picker shows");
    let sessions = attach.pending().unwrap();
    assert_eq!(sessions.prompt.as_deref(), Some("Attach from studio"));
    assert_eq!(
        sessions.items[0].subtitle.as_deref(),
        Some("main/work  ·  release prep  ·  /repo  ·  vim")
    );
    attach.resolve(&sessions, sessions.picked(0).unwrap());
    attach.wait_for_runs(2);
    assert_eq!(attach.hud(), Some(progress("attaching build…")));
    assert!(attach.pending().is_none());
    let before = attach.fixture.store().sessions().count();
    attach.runner.answer(1, listing(&[build_session()]));
    attach.drive("the attach lands", |attach| {
        attach.fixture.store().sessions().count() == before + 1
    });
    assert_eq!(attach.hud(), None, "the attach panel closes");
}

#[test]
fn a_failed_or_empty_listing_shows_a_timed_error_panel_and_no_picker() {
    let answers = [
        (
            refused("ssh: \u{1b}[31mconnection refused\u{1b}[0m\n"),
            "studio: ssh: connection refused",
        ),
        (listing(&[]), "nothing to attach on studio"),
    ];
    for (call, (answer, shown)) in answers.into_iter().enumerate() {
        let mut attach = Attach::new(Some("studio\n"));
        let window = attach.fixture.window;
        attach.core().attach_remote(window);
        attach.wait_for_runs(1);
        attach.runner.answer(0, answer);
        attach.drive("the error panel shows", |attach| {
            attach.hud().is_some_and(|hud| hud.spinner.is_none())
        });
        assert_eq!(attach.hud(), Some(failure(shown)), "answer {call}");
        assert!(attach.pending().is_none());
        assert_eq!(attach.fixture.core.remote_wake(), None, "the flow ended");
    }
}

#[test]
fn a_refused_attach_shows_a_timed_error_panel() {
    let mut attach = Attach::new(Some("studio\n"));
    let window = attach.fixture.window;
    attach.core().attach_remote(window);
    attach.wait_for_runs(1);
    attach.runner.answer(0, listing(&[build_session()]));
    attach.drive("the session picker opens", |attach| attach.pending().is_some());
    let sessions = attach.pending().unwrap();
    attach.resolve(&sessions, sessions.picked(0).unwrap());
    attach.wait_for_runs(2);
    // the session went away between the listing and the attach
    attach.runner.answer(1, listing(&[]));
    attach.drive("the error panel shows", |attach| {
        attach.hud().is_some_and(|hud| hud.spinner.is_none())
    });
    assert_eq!(
        attach.hud(),
        Some(failure(&format!("no attachable session {REMOTE_SESSION} on studio")))
    );
}

#[test]
fn an_attach_leaves_a_hud_another_caller_posted_during_its_wait_alone() {
    let mut attach = Attach::new(Some("studio\n"));
    let window = attach.fixture.window;
    attach.core().attach_remote(window);
    attach.wait_for_runs(1);
    let target = wire(attach.session);
    let response = attach.core().dispatch(&Request {
        cmd: Command::Session(SessionCmd::HudOpen),
        target: Some(target),
        args: Some(args(|a| a.message = Some("an agent's own panel".to_owned()))),
    });
    assert!(response.ok, "{response:?}");
    attach.runner.answer(0, listing(&[build_session()]));
    attach.drive("the session picker opens", |attach| attach.pending().is_some());
    assert_eq!(
        attach.hud().map(|hud| hud.message).as_deref(),
        Some("an agent's own panel"),
        "the finished listing closed nothing"
    );
    let sessions = attach.pending().unwrap();
    attach.cancel(&sessions);
}

#[test]
fn an_overlong_error_is_cut_to_what_a_hud_accepts() {
    let mut attach = Attach::new(Some("studio\n"));
    let window = attach.fixture.window;
    attach.core().attach_remote(window);
    attach.wait_for_runs(1);
    attach.runner.answer(0, refused(&"q\u{301}".repeat(400)));
    attach.drive("the error panel shows", |attach| {
        attach.hud().is_some_and(|hud| hud.spinner.is_none())
    });
    let message = attach.hud().unwrap().message;
    assert!(crate::hud::layout::text_length(&message) <= HudSpec::MAX_TEXT_LENGTH);
    // a reason this long leaves no room for the name, as upstream's `CommandFailure.message` cuts it
    assert!(message.starts_with("studio: q\u{301}"), "{message}");
}

#[test]
fn dismissing_the_session_picker_attaches_nothing() {
    let mut attach = Attach::new(Some("studio\n"));
    let window = attach.fixture.window;
    attach.core().attach_remote(window);
    attach.wait_for_runs(1);
    attach.runner.answer(0, listing(&[build_session()]));
    attach.drive("the session picker opens", |attach| attach.pending().is_some());
    let before = attach.fixture.store().sessions().count();
    let sessions = attach.pending().unwrap();
    attach.cancel(&sessions);
    attach.idle();
    assert!(attach.pending().is_none());
    assert_eq!(attach.runner.hosts().len(), 1, "nothing is attached");
    assert_eq!(attach.fixture.store().sessions().count(), before);
    assert_eq!(attach.fixture.core.remote_wake(), None);
}

#[test]
fn a_window_closed_during_the_listing_gets_no_picker() {
    let mut attach = Attach::new(Some("studio\n"));
    let window = attach.fixture.window;
    let other = attach.core().library_mut().new_window(None);
    attach.core().attach_remote(window);
    attach.wait_for_runs(1);
    attach.core().close_window(window);
    attach.runner.answer(0, listing(&[build_session()]));
    attach.drive("the flow ends", |attach| attach.fixture.core.remote_wake().is_none());
    assert!(
        attach
            .fixture
            .core
            .modal_slot(window)
            .and_then(|slot| slot.pending())
            .is_none()
    );
    assert!(
        attach
            .fixture
            .core
            .modal_slot(other)
            .and_then(|slot| slot.pending())
            .is_none()
    );
    assert_eq!(attach.runner.hosts().len(), 1, "nothing is attached");
}

#[test]
fn attach_does_nothing_while_a_modal_holds_the_window() {
    let mut attach = Attach::new(Some("studio\n"));
    let window = attach.fixture.window;
    let busy = PendingPick {
        id: "busy".to_owned(),
        items: vec![agterm_proto::PickItem {
            id: "a".to_owned(),
            label: "A".to_owned(),
            subtitle: None,
        }],
        prompt: None,
        query: None,
        allow_custom: false,
        selection: None,
    };
    attach.core().modals.open_pick(window, busy).unwrap();
    attach.core().attach_remote(window);
    attach.idle();
    assert!(attach.runner.hosts().is_empty());
    assert_eq!(attach.pending().map(|pick| pick.id).as_deref(), Some("busy"));
    assert_eq!(attach.hud(), None);
}

#[test]
fn a_window_switch_while_choosing_the_mac_keeps_the_attach_in_the_starting_window() {
    let mut attach = Attach::new(Some("studio\nmini\n"));
    let window = attach.fixture.window;
    let other = attach.core().library_mut().new_window(None);
    attach.core().report_frontmost(window);
    attach.core().attach_remote(window);
    let hosts = attach.pending().expect("the machine picker opens");
    attach.core().report_frontmost(other);
    attach.resolve(&hosts, hosts.picked(0).unwrap());
    attach.wait_for_runs(1);
    assert_eq!(attach.hud(), Some(progress("listing sessions on studio…")));
    attach.runner.answer(0, listing(&[build_session()]));
    attach.drive("the session picker opens in the starting window", |attach| {
        attach.pending().is_some()
    });
    assert!(
        attach
            .fixture
            .core
            .modal_slot(other)
            .and_then(|slot| slot.pending())
            .is_none()
    );
    let sessions = attach.pending().unwrap();
    attach.resolve(&sessions, sessions.picked(0).unwrap());
    attach.wait_for_runs(2);
    let before = attach.fixture.store().sessions().count();
    attach.runner.answer(1, listing(&[build_session()]));
    attach.drive("the attach lands in the starting window", |attach| {
        attach.fixture.store().sessions().count() == before + 1
    });
    let attached = attach.fixture.store().sessions().last().unwrap();
    assert_eq!(attached.remote_host(), Some("studio"));
}
