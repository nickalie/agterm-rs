//! Reset Live Sessions…: its alerts (the GTK build's cases), the flow through a core over a fake session host (refused
//! outside Live, confirmed and quitting at once with the marker armed), the alert's answers in a test window, and the
//! relauncher waiting for the app to exit before it starts it.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agterm_core::model::{StatusPane, WindowLibrary};
use agterm_core::persist::RestoreMode;
use agterm_core::restore::mode::RestoreLaunchDecision;
use agterm_core::zmx::support::Inputs;
use agterm_core::{Core, CoreConfig, CoreState, LiveInstall};
use agterm_host::fakes::{FakeSessionHost, FakeSurface};
use agterm_host::{MainLoop, MainLoopClosed, MainTask, SessionHost};
use agterm_proto::ControlZmxEndpoint;
use gpui_kit::TestAppContext;

use super::*;
use crate::modal::tests::host;
use crate::modal::{alert_open, answer_front_alert, front_alert};

fn seeded_core() -> Core {
    Core::new(
        CoreConfig {
            app: agterm_proto::AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library: WindowLibrary::seeded("/home/u"),
            ..CoreState::default()
        },
    )
}

/// A core launched and configured Live over a fake host whose one daemon, the shown session's live pane, is outdated
/// (created before the launch's cutoff), so a reset selects it; the reset marker goes under `state_dir`.
pub(crate) fn live_core(state_dir: &Path) -> Core {
    let mut core = seeded_core();
    core.set_restore_launch(RestoreLaunchDecision::new(RestoreMode::Live, None));
    assert!(core.save_restore_mode(RestoreMode::Live));
    let host = Arc::new(FakeSessionHost::new());
    core.install_live_sessions(LiveInstall {
        host: Arc::clone(&host) as Arc<dyn SessionHost>,
        endpoint: ControlZmxEndpoint {
            executable: "/opt/agterm/bin/zmx".to_owned(),
            socket_directory: "/tmp/agterm-zmx-test".to_owned(),
        },
        wrap: Inputs {
            zmx_executable: "/bin/sh".into(),
            login_shell: Some("/bin/zsh".to_owned()),
            state_dir: state_dir.to_path_buf(),
            ..Inputs::default()
        },
        state_dir: state_dir.to_path_buf(),
        outdated_before: Some(100),
    });
    let window = core.library().active_window_id().unwrap();
    let session = core
        .library_mut()
        .store_mut(window)
        .and_then(|store| {
            let id = store.selected_session_id()?;
            store.session_mut(id)
        })
        .unwrap();
    let identity = session.pane_identity;
    session.pane_slot_mut(StatusPane::Left).replace(Box::new(
        FakeSurface::new(agterm_core::pane_env::uuid_string(identity)).with_live_session(true),
    ));
    host.set_record(&agterm_core::zmx::daemon_name(identity), Some(1), Some(100), Some(50));
    core
}

#[test]
fn the_confirmation_defaults_to_cancel_and_a_refusal_explains_itself() {
    let prompt = ResetPrompt {
        title: "Reset Live Sessions?".to_owned(),
        body: "2 live sessions will be reset.".to_owned(),
    };
    let spec = confirm_alert(&prompt);
    assert_eq!(spec.heading, "Reset Live Sessions?");
    assert_eq!(spec.body, "2 live sessions will be reset.");
    let labels: Vec<&str> = spec.buttons.iter().map(|button| button.label.as_ref()).collect();
    assert_eq!(labels, ["Cancel", "Reset"], "Cancel first, as upstream");
    assert_eq!(spec.default_button(), Some(0), "Return cancels");
    assert_eq!(spec.cancel_button(), Some(0), "Escape cancels");
    assert_eq!(spec.buttons[RESET].appearance, crate::modal::Appearance::Destructive);
    let refusal = refusal_alert(ResetRefusal::NothingToReset);
    assert_eq!(refusal.heading, "Reset Live Sessions");
    assert_eq!(refusal.body, ResetRefusal::NothingToReset.user_message());
    let labels: Vec<&str> = refusal.buttons.iter().map(|button| button.label.as_ref()).collect();
    assert_eq!(labels, ["OK"]);
}

#[test]
fn outside_live_the_menu_item_and_its_confirmation_are_refused_without_quitting() {
    let mut state = AppState::new(seeded_core());
    state.reset_live_sessions();
    let work = state.take_frontend_work();
    assert_eq!(work.live_reset, Some(ResetAlert::Refused(ResetRefusal::NotLive)));
    assert!(!work.quit);
    state.confirm_live_reset();
    let work = state.take_frontend_work();
    assert_eq!(work.live_reset, Some(ResetAlert::Refused(ResetRefusal::NotLive)));
    assert!(!work.quit, "nothing confirmed, nothing quits");
}

#[test]
fn a_confirmed_reset_quits_at_once_and_the_quit_arms_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(live_core(dir.path()));
    assert!(state.core.live_reset_menu_visible());
    state.reset_live_sessions();
    let work = state.take_frontend_work();
    let Some(ResetAlert::Confirm(prompt)) = work.live_reset else {
        panic!("no confirmation: {:?}", work.live_reset);
    };
    let (title, body) = agterm_core::zmx::reset::dialog_text(1, 1);
    assert_eq!((prompt.title, prompt.body), (title, body));
    assert!(!work.quit, "nothing quits before the answer");
    state.confirm_live_reset();
    let work = state.take_frontend_work();
    assert!(work.quit, "Reset quits at once");
    assert_eq!(work.quit_prompt, None, "without the quit prompt");
    assert_eq!(work.live_reset, None);
    assert!(state.core.arm_live_reset_on_quit(), "the quit arms the reset");
    assert!(dir.path().join(agterm_core::zmx::reset::MARKER_FILE_NAME).exists());
}

/// A loop that keeps what is posted.
#[derive(Default)]
struct Posted(Mutex<Vec<MainTask<AppState>>>);

impl MainLoop<AppState> for Posted {
    fn post(&self, task: MainTask<AppState>) -> Result<(), MainLoopClosed> {
        self.0.lock().unwrap().push(task);
        Ok(())
    }

    fn post_after(&self, _: Duration, task: MainTask<AppState>) -> Result<(), MainLoopClosed> {
        self.post(task)
    }
}

#[gpui_kit::test]
fn cancel_and_escape_keep_the_app_and_reset_confirms(cx: &mut TestAppContext) {
    let posted = Arc::new(Posted::default());
    let main_loop: Arc<dyn MainLoop<AppState>> = Arc::clone(&posted) as _;
    let weak = Arc::downgrade(&main_loop);
    let cx = host(cx);
    let confirm = ResetAlert::Confirm(ResetPrompt {
        title: "Reset Live Sessions?".to_owned(),
        body: "1 live session will be reset.".to_owned(),
    });
    for key in ["escape", "enter"] {
        let asking = weak.clone();
        let alert = confirm.clone();
        cx.update(move |window, cx| present(&alert, asking, window, cx));
        cx.run_until_parked();
        assert!(cx.update(|window, cx| alert_open(window.window_handle(), cx)));
        cx.simulate_keystrokes(key);
        assert!(
            !cx.update(|window, cx| alert_open(window.window_handle(), cx)),
            "{key} answers"
        );
        assert!(posted.0.lock().unwrap().is_empty(), "{key} cancels");
    }
    let (asking, alert) = (weak.clone(), confirm.clone());
    cx.update(move |window, cx| present(&alert, asking, window, cx));
    cx.run_until_parked();
    assert!(cx.update(|window, cx| answer_front_alert(window, "Reset", cx)));
    let tasks = std::mem::take(&mut *posted.0.lock().unwrap());
    assert_eq!(tasks.len(), 1, "Reset posts the confirmation");
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(live_core(dir.path()));
    for task in tasks {
        task(&mut state);
    }
    assert!(state.take_frontend_work().quit);
    let refused = ResetAlert::Refused(ResetRefusal::ListingFailed);
    cx.update(move |window, cx| present(&refused, weak, window, cx));
    cx.run_until_parked();
    let body =
        cx.update(|window, cx| front_alert(window.window_handle(), cx).map(|alert| alert.read(cx).spec().body.clone()));
    assert_eq!(body.as_deref(), Some(ResetRefusal::ListingFailed.user_message()));
    cx.simulate_keystrokes("enter");
    assert!(!cx.update(|window, cx| alert_open(window.window_handle(), cx)));
    assert!(posted.0.lock().unwrap().is_empty(), "a refusal's OK runs nothing");
}

#[test]
fn the_relauncher_waits_for_the_pid_then_starts_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let app = dir.path().join("app");
    agterm_host::fakes::write_executable(&app, &format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    let mut waited = std::process::Command::new("sleep").arg("0.3").spawn().unwrap();
    let mut relauncher = relaunch_command(&app, waited.id()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(!marker.exists(), "started before the app exited");
    waited.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(marker.exists(), "never started");
    assert!(relauncher.wait().unwrap().success());
}

#[test]
fn the_relauncher_gives_up_on_a_process_that_never_exits() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let app = dir.path().join("app");
    agterm_host::fakes::write_executable(&app, &format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    let mut waited = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let command = relaunch_command(&app, waited.id());
    let mut args: Vec<_> = command.get_args().map(std::ffi::OsStr::to_owned).collect();
    assert_eq!(
        args.last(),
        Some(&RELAUNCH_MAX_WAITS.to_string().into()),
        "the polls ride last"
    );
    // the same waiter allowed one poll only
    args.pop();
    let status = std::process::Command::new("/bin/sh")
        .args(&args)
        .arg("1")
        .status()
        .unwrap();
    assert!(!status.success(), "gave up");
    assert!(!marker.exists(), "never started the app");
    waited.kill().unwrap();
    waited.wait().unwrap();
}
