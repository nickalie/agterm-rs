//! The panel geometry (ported from the GTK build's `modal.rs` tests), the dialog keys, and the alerts in GPUI's test
//! window: the keyboard's ring, Return and Escape, the check and the field, the queue, and the registry observers hear.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{Keystroke, TestAppContext, VisualTestContext, div};

use super::*;

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

#[test]
fn an_ask_panel_is_capped_by_its_anchor_or_its_percent() {
    // 90% of a 400-wide anchor is 360, under 72 cells of 8
    assert!(close(ask_panel_cap(None, 400.0, 8.0), 360.0));
    // 72 cells of 8 = 576, under 90% of 2000
    assert!(close(ask_panel_cap(None, 2000.0, 8.0), 576.0));
    assert!(close(ask_panel_cap(Some(50), 800.0, 8.0), 400.0));
    assert!(close(ask_panel_cap(Some(5), 800.0, 8.0), 80.0));
    assert!(close(ask_panel_cap(Some(150), 800.0, 8.0), 800.0));
}

#[test]
fn the_palette_fits_the_terminal_area_and_centers_over_it() {
    assert!(close(palette_width(1200.0, 0.0), 520.0));
    // the sidebar leaves 1000 - 221 - 32 = 747: the ideal fits
    assert!(close(palette_width(1000.0, 221.0), 520.0));
    // a narrow terminal area shrinks it, not under 280
    assert!(close(palette_width(700.0, 221.0), 447.0));
    assert!(close(palette_width(500.0, 300.0), 280.0));
    // never past the frame less its margins
    assert!(close(palette_width(250.0, 0.0), 218.0));
    assert!(close(palette_offset(520.0, 1000.0, 221.0), 110.5));
    assert!(
        close(palette_offset(520.0, 600.0, 221.0), 40.0),
        "kept inside the frame"
    );
    let rect = palette_rect(1000.0, 600.0, 0.0, 300.0, 50.0);
    assert_eq!(
        rect,
        Rect {
            x: 240,
            y: 72,
            width: 520,
            height: 300
        }
    );
    let short = palette_rect(1000.0, 100.0, 0.0, 300.0, 50.0);
    assert_eq!(short.height, 160, "the minimum cap");
}

#[test]
fn keys_navigate_as_upstreams_key_catcher() {
    let key = |source: &str| nav_key(&Keystroke::parse(source).unwrap());
    assert_eq!(key("tab"), Some(NavKey::Forward));
    assert_eq!(key("shift-tab"), Some(NavKey::Backward));
    for forward in ["right", "down"] {
        assert_eq!(key(forward), Some(NavKey::Forward));
    }
    for backward in ["left", "up"] {
        assert_eq!(key(backward), Some(NavKey::Backward));
    }
    assert_eq!(key("enter"), Some(NavKey::Activate));
    assert_eq!(key("escape"), Some(NavKey::Cancel));
    assert_eq!(key("space"), Some(NavKey::Space));
    assert_eq!(key("y"), Some(NavKey::Letter("y".to_owned())));
    assert_eq!(key("shift-y"), Some(NavKey::Letter("y".to_owned())));
    assert_eq!(key("1"), None, "only letters are hotkeys");
    for modified in ["ctrl-y", "alt-tab", "super-enter"] {
        assert_eq!(key(modified), None, "{modified} goes on to the window");
    }
}

#[test]
fn a_confirmation_defaults_to_cancel_and_draws_its_verb_destructive() {
    let spec = AlertSpec::confirm("Close “u”?", "body", "Close");
    assert_eq!(spec.default_button(), Some(0));
    assert_eq!(spec.cancel_button(), Some(0));
    assert_eq!(spec.buttons[1].look(), ButtonLook::Destructive);
    let info = AlertSpec::info("Too Many Folders", "body");
    assert_eq!((info.default_button(), info.cancel_button()), (Some(0), Some(0)));
}

/// A root view drawing the window's front alert, as the window's view does.
struct Host;

impl Render for Host {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(alert_layer(window.window_handle(), cx))
    }
}

type Answers = Rc<RefCell<Vec<AlertAnswer>>>;

/// A test window drawing the alert layer, active.
pub(crate) fn host(cx: &mut TestAppContext) -> &mut VisualTestContext {
    cx.update(gpui_kit::init);
    let (_, cx) = cx.add_window_view(|_, _| Host);
    cx.update(|window, _| window.activate_window());
    cx
}

fn open(cx: &mut VisualTestContext, spec: AlertSpec, answers: &Answers) -> Entity<Alert> {
    let answers = Rc::clone(answers);
    let alert =
        cx.update(|window, cx| open_alert(window, spec, move |answer, _, _| answers.borrow_mut().push(answer), cx));
    cx.run_until_parked();
    alert
}

fn focused(cx: &mut VisualTestContext, alert: &Entity<Alert>) -> bool {
    cx.update(|window, cx| alert.read(cx).holds_keyboard(window, cx))
}

#[gpui_kit::test]
fn an_alert_takes_the_keyboard_and_answers_by_its_keys(cx: &mut TestAppContext) {
    let cx = host(cx);
    let answers = Answers::default();
    let alert = open(cx, AlertSpec::confirm("Delete “w”?", "body", "Delete"), &answers);
    assert!(focused(cx, &alert), "the alert takes the keyboard");
    assert_eq!(
        alert.read_with(cx, |alert, _| alert.highlighted_button()),
        Some(0),
        "the keyboard starts on the default"
    );
    cx.simulate_keystrokes("tab");
    assert_eq!(alert.read_with(cx, |alert, _| alert.highlighted_button()), Some(1));
    cx.simulate_keystrokes("left right");
    assert_eq!(alert.read_with(cx, |alert, _| alert.highlighted_button()), Some(1));
    cx.simulate_keystrokes("enter");
    assert_eq!(
        answers.borrow().as_slice(),
        [AlertAnswer {
            button: 1,
            checked: false,
            checks: Vec::new(),
            text: None
        }]
    );
    assert!(
        !cx.update(|window, cx| alert_open(window.window_handle(), cx)),
        "answered, it is gone"
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(answers.borrow().len(), 1, "answered once");
}

#[gpui_kit::test]
fn escape_answers_with_the_cancel_button_and_the_next_alert_comes_up(cx: &mut TestAppContext) {
    let cx = host(cx);
    let answers = Answers::default();
    let first = open(cx, AlertSpec::confirm("first", "", "Close"), &answers);
    let second = open(cx, AlertSpec::info("second", ""), &answers);
    assert!(
        focused(cx, &first) && !focused(cx, &second),
        "the first one is in front"
    );
    let front = cx.update(|window, cx| front_alert(window.window_handle(), cx));
    assert_eq!(front.map(|alert| alert.entity_id()), Some(first.entity_id()));

    cx.simulate_keystrokes("escape");
    assert_eq!(answers.borrow()[0].button, 0, "Cancel");
    assert!(focused(cx, &second), "the next one takes the keyboard");
    cx.simulate_keystrokes("escape");
    assert_eq!(answers.borrow().len(), 2);
    assert!(!cx.update(|window, cx| alert_open(window.window_handle(), cx)));
}

#[gpui_kit::test]
fn the_check_rides_on_the_answer(cx: &mut TestAppContext) {
    let cx = host(cx);
    let answers = Answers::default();
    let spec = crate::terminal::prompt_alert(agterm_core::clipboard_policy::ClipboardAccess::Read);
    assert_eq!(spec.checks[0].label, crate::terminal::REMEMBER);
    let alert = open(cx, spec, &answers);
    assert_eq!(
        alert.read_with(cx, |alert, _| alert.highlighted_button()),
        Some(1),
        "Allow is the default"
    );
    // the ring wraps from Allow to the check, which Space toggles
    cx.simulate_keystrokes("tab space");
    assert!(alert.read_with(cx, |alert, _| alert.checked()));
    cx.simulate_keystrokes("shift-tab enter");
    assert_eq!(
        answers.borrow().as_slice(),
        [AlertAnswer {
            button: 1,
            checked: true,
            checks: vec![true],
            text: None
        }]
    );
}

#[gpui_kit::test]
fn a_field_takes_the_keyboard_and_returns_its_text(cx: &mut TestAppContext) {
    let cx = host(cx);
    let answers = Answers::default();
    let alert = open(cx, crate::keymap_actions::rename_alert("work"), &answers);
    assert!(focused(cx, &alert), "the field holds the keyboard");
    // the name is selected: typing replaces it
    cx.simulate_keystrokes("p l a y");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        answers.borrow().as_slice(),
        [AlertAnswer {
            button: 1,
            checked: false,
            checks: Vec::new(),
            text: Some("play".to_owned())
        }]
    );

    let alert = open(cx, crate::keymap_actions::rename_alert("work"), &answers);
    assert!(focused(cx, &alert));
    cx.simulate_keystrokes("escape");
    assert_eq!(answers.borrow()[1].button, 0, "Escape cancels from the field");
}

#[gpui_kit::test]
fn a_click_answers_and_observers_hear_every_open_and_close(cx: &mut TestAppContext) {
    let cx = host(cx);
    let heard = Rc::new(RefCell::new(0_usize));
    let counting = Rc::clone(&heard);
    let _observer = cx.update(|_, cx| cx.observe_global::<Alerts>(move |_| *counting.borrow_mut() += 1));
    let answers = Answers::default();
    open(cx, AlertSpec::confirm("q", "", "Close"), &answers);
    let opened = *heard.borrow();
    assert!(opened >= 1, "an open is heard");
    assert!(cx.update(|window, cx| answer_front_alert(window, "Close", cx)));
    cx.run_until_parked();
    assert_eq!(answers.borrow()[0].button, 1);
    assert!(*heard.borrow() > opened, "a close is heard");
    assert!(
        !cx.update(|window, cx| answer_front_alert(window, "Close", cx)),
        "none left"
    );
}
