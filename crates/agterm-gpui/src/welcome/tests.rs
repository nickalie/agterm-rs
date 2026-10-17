//! The welcome: its alert (ported from the GTK build's test), its keys in a test window, and the launch marking it
//! shown before it opens. Install is never pressed: it runs the real installers.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::TestAppContext;

use super::*;
use crate::modal::tests::host;
use crate::modal::{AlertAnswer, alert_open};

#[test]
fn the_welcome_offers_both_installs_checked_and_later_dismisses() {
    let spec = alert();
    assert_eq!(spec.heading, "Welcome to agterm");
    assert_eq!(spec.checks.len(), 2);
    assert!(
        spec.checks.iter().all(|check| check.checked),
        "both installs start checked"
    );
    assert_eq!(
        spec.checks[0].id.as_ref().map(AsRef::as_ref),
        Some("welcome-skill-checkbox")
    );
    assert_eq!(
        spec.checks[1].id.as_ref().map(AsRef::as_ref),
        Some("welcome-hooks-checkbox")
    );
    assert_eq!(spec.buttons[INSTALL].label.as_ref(), "Install");
    assert_eq!(spec.default_button(), Some(INSTALL), "Return installs");
    assert_eq!(spec.buttons[spec.cancel_button().unwrap()].label.as_ref(), "Later");
}

#[gpui_kit::test]
fn a_check_can_be_dropped_before_installing(cx: &mut TestAppContext) {
    let cx = host(cx);
    let answers: Rc<RefCell<Vec<AlertAnswer>>> = Rc::default();
    let recorded = Rc::clone(&answers);
    cx.update(|window, cx| {
        crate::modal::open_alert(
            window,
            alert(),
            move |answer, _, _| recorded.borrow_mut().push(answer),
            cx,
        );
    });
    cx.run_until_parked();
    // the ring starts on Install; back past Later onto the hooks check, which Space drops
    cx.simulate_keystrokes("shift-tab shift-tab space tab tab enter");
    let answers = answers.borrow();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].button, INSTALL);
    assert_eq!(answers[0].checks, [true, false], "the skill alone");
}

#[gpui_kit::test]
fn later_dismisses_the_welcome(cx: &mut TestAppContext) {
    let cx = host(cx);
    cx.update(present);
    cx.run_until_parked();
    assert!(cx.update(|window, cx| alert_open(window.window_handle(), cx)));
    cx.simulate_keystrokes("escape");
    assert!(
        !cx.update(|window, cx| alert_open(window.window_handle(), cx)),
        "Later closes it and runs nothing"
    );
}

#[test]
fn the_welcome_is_marked_shown_before_it_opens() {
    let mut state = AppState::new(agterm_core::Core::new(
        agterm_core::CoreConfig {
            app: agterm_proto::AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        agterm_core::CoreState::default(),
    ));
    state.request_welcome();
    assert_eq!(state.core.settings().welcome_shown, Some(true));
    assert!(state.take_frontend_work().welcome);
}

// MARK: - the quit confirmation

fn seeded_core(sessions: usize) -> agterm_core::Core {
    let mut library = agterm_core::model::WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let workspace = store.current_workspace_id().unwrap();
    for _ in 1..sessions {
        store
            .add_session(workspace, agterm_core::model::NewSession::at_cwd("/home/u"))
            .unwrap();
    }
    agterm_core::Core::new(
        agterm_core::CoreConfig {
            app: agterm_proto::AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        agterm_core::CoreState {
            library,
            ..agterm_core::CoreState::default()
        },
    )
}

#[test]
fn a_users_quit_names_what_it_closes_unless_a_ui_test_launched() {
    let core = seeded_core(2);
    assert_eq!(
        quit_prompt(&core, false).as_deref(),
        Some("This closes 1 window and 2 sessions, ending all running shells.")
    );
    assert_eq!(quit_prompt(&core, true), None, "a UI-test launch quits at once");
    let spec = quit_alert("This closes 1 window and 1 session.");
    assert_eq!(spec.heading, "Quit Agterm?");
    assert_eq!(spec.buttons[QUIT].label.as_ref(), "Quit");
    assert_eq!(spec.default_button(), Some(QUIT), "Return quits");
    assert_eq!(spec.buttons[spec.cancel_button().unwrap()].label.as_ref(), "Cancel");
}

#[test]
fn the_menus_quit_asks_first_and_quits_only_once_confirmed() {
    let mut state = AppState::new(seeded_core(1));
    state.quit_from_menu();
    let work = state.take_frontend_work();
    if agterm_core::is_ui_test_launch(&ProcessEnv) {
        assert!(work.quit, "a UI-test launch quits at once");
        return;
    }
    assert!(!work.quit, "nothing quits before the answer");
    assert_eq!(
        work.quit_prompt.as_deref(),
        Some("This closes 1 window and 1 session, ending all running shells.")
    );
}

/// A loop that keeps what is posted.
#[derive(Default)]
struct Posted(std::sync::Mutex<Vec<agterm_host::MainTask<AppState>>>);

impl agterm_host::MainLoop<AppState> for Posted {
    fn post(&self, task: agterm_host::MainTask<AppState>) -> Result<(), agterm_host::MainLoopClosed> {
        self.0.lock().unwrap().push(task);
        Ok(())
    }

    fn post_after(
        &self,
        _: std::time::Duration,
        task: agterm_host::MainTask<AppState>,
    ) -> Result<(), agterm_host::MainLoopClosed> {
        self.post(task)
    }
}

#[gpui_kit::test]
fn cancel_keeps_the_app_and_quit_posts_the_quit(cx: &mut TestAppContext) {
    let posted = std::sync::Arc::new(Posted::default());
    let main_loop: std::sync::Arc<dyn agterm_host::MainLoop<AppState>> = std::sync::Arc::clone(&posted) as _;
    let answers = std::sync::Arc::downgrade(&main_loop);
    let cx = host(cx);
    let asking = answers.clone();
    cx.update(move |window, cx| present_quit("This closes 1 window and 1 session.", asking, window, cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    assert!(posted.0.lock().unwrap().is_empty(), "Cancel quits nothing");
    cx.update(move |window, cx| present_quit("This closes 1 window and 1 session.", answers, window, cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    let tasks = std::mem::take(&mut *posted.0.lock().unwrap());
    assert_eq!(tasks.len(), 1, "Quit posts the quit");
    let mut state = AppState::new(seeded_core(1));
    for task in tasks {
        task(&mut state);
    }
    assert!(state.take_frontend_work().quit);
}
