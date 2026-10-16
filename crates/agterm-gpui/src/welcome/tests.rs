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
