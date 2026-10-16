//! The dialog's pure parts, ported from the GTK build's `ask_dialog.rs` tests: the keys, the labels and looks, the
//! strip, the answers, where a terminal ask shows and when it wants the keyboard. The dialog on a page, its keys and its
//! answers reaching the core are the deck's tests (`deck/tests.rs`); the GUI ask over its window is the window's.

use agterm_core::model::AppStore;
use agterm_host::fakes::FakeSurface;
use gpui_kit::Keystroke;

use super::*;

fn button(id: &str, label: &str, hotkey: Option<&str>) -> AskButton {
    AskButton {
        id: id.to_owned(),
        label: label.to_owned(),
        hotkey: hotkey.map(str::to_owned),
    }
}

fn ask(style: AskStyle) -> PendingAsk {
    PendingAsk {
        id: "A1".to_owned(),
        title: "Deploy?".to_owned(),
        message: Some("to prod".to_owned()),
        buttons: vec![
            button("yes", "Yes", Some("y")),
            button("no", "No", Some("n")),
            button("wipe", "Wipe", None),
        ],
        default_id: None,
        destructive_id: Some("wipe".to_owned()),
        style,
        align: AskAlignment::Right,
        width: None,
        anchor: None,
    }
}

#[test]
fn keys_map_as_upstreams_key_catcher() {
    let key = |source: &str| ask_key(&Keystroke::parse(source).unwrap());
    assert_eq!(key("tab"), Some(AskKey::Forward));
    assert_eq!(key("shift-tab"), Some(AskKey::Backward));
    assert_eq!(key("down"), Some(AskKey::Forward));
    assert_eq!(key("up"), Some(AskKey::Backward));
    assert_eq!(key("enter"), Some(AskKey::Activate));
    assert_eq!(key("escape"), Some(AskKey::Cancel));
    assert_eq!(key("y"), Some(AskKey::Hotkey("y".to_owned())));
    assert_eq!(key("shift-y"), Some(AskKey::Hotkey("y".to_owned())));
    assert_eq!(key("space"), None, "Space is no ask key");
    assert_eq!(key("ctrl-y"), None, "a modified key goes on to the window");
}

#[test]
fn a_hotkey_is_underlined_in_its_label_or_appended() {
    let found = button_label(&button("y", "Yes", Some("y")), false);
    assert_eq!(
        (found.text.as_str(), found.underline.clone(), found.bold),
        ("Yes", Some(0..1), false)
    );
    let later = button_label(&button("n", "Cancel now", Some("n")), false);
    assert_eq!(later.underline, Some(2..3), "the first occurrence, whatever its case");
    let appended = button_label(&button("x", "Go", Some("x")), false);
    assert_eq!(appended.text, "Go (X)");
    assert_eq!(&appended.text[appended.underline.clone().unwrap()], "X");
    let destructive = button_label(&button("w", "Wipe", Some("w")), true);
    assert_eq!(destructive.text, "! Wipe");
    assert_eq!(&destructive.text[destructive.underline.clone().unwrap()], "W");
    assert!(destructive.bold);
    let plain = button_label(&button("p", "Plain", None), false);
    assert_eq!((plain.text.as_str(), plain.underline), ("Plain", None));
}

#[test]
fn buttons_look_by_highlight_and_destructiveness() {
    assert_eq!(
        button_look(AskStyle::Gui, true, false),
        AskButtonLook::Gui(ButtonLook::Suggested)
    );
    assert_eq!(
        button_look(AskStyle::Gui, true, true),
        AskButtonLook::Gui(ButtonLook::Destructive)
    );
    assert_eq!(
        button_look(AskStyle::Gui, false, true),
        AskButtonLook::Gui(ButtonLook::DestructiveText)
    );
    assert_eq!(
        button_look(AskStyle::Gui, false, false),
        AskButtonLook::Gui(ButtonLook::Plain)
    );
    assert_eq!(button_look(AskStyle::Terminal, true, true), AskButtonLook::Inverted);
    assert_eq!(button_look(AskStyle::Terminal, false, true), AskButtonLook::Block);
}

#[test]
fn buttons_go_in_a_row_while_it_fits() {
    assert!((row_width(50.0, 3, 8.0) - 166.0).abs() < 1e-9);
    assert!((row_width(50.0, 1, 8.0) - 50.0).abs() < 1e-9);
    assert_eq!(strip_axis(50.0, 3, 8.0, 166.0), StripAxis::Row);
    assert_eq!(strip_axis(50.0, 3, 8.0, 165.0), StripAxis::Column);
}

#[test]
fn the_look_counts_cells_from_the_font() {
    let look = Look {
        family: None,
        size: 15.0,
        foreground: Rgb::WHITE,
        background: Rgb::BLACK,
    };
    assert!((look.font_pixels() - 20.0).abs() < 1e-9);
    assert!((look.terminal_cell() - 12.0).abs() < 1e-9);
    assert!((look.cell(AskStyle::Gui) - GUI_CELL).abs() < 1e-9);
    let small = Look { size: 6.0, ..look };
    assert!((small.terminal_cell() - 8.0).abs() < 1e-9, "never under 8");
}

#[test]
fn answers_report_the_button_or_escaped() {
    let ask = ask(AskStyle::Gui);
    let answered = Answer::Button(1).result(&ask).unwrap();
    assert_eq!(answered.result, ControlAskOutcome::Answered);
    assert_eq!(answered.id.as_deref(), Some("no"));
    assert_eq!(answered.index, Some(1));
    assert_eq!(Answer::Button(9).result(&ask), None);
    assert_eq!(Answer::Escaped.result(&ask).unwrap().result, ControlAskOutcome::Escaped);
}

/// What a test ask is opened over.
#[derive(Clone, Copy)]
enum Over {
    Session,
    Primary,
    /// A pane identity no pane carries.
    Gone,
}

/// A seeded store whose session (primary pane `P`) waits on `ask` opened `over`.
fn store_with(ask: PendingAsk, over: Over) -> (agterm_core::model::WindowLibrary, Uuid, Uuid) {
    let mut library = agterm_core::model::WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let id = store.sessions().next().unwrap().id();
    let session = store.session_mut(id).unwrap();
    session.surface.replace(Box::new(FakeSurface::new("P")));
    let identity = match over {
        Over::Session => None,
        Over::Primary => Some(session.pane_identity),
        Over::Gone => Some(Uuid::new_v4()),
    };
    assert!(store.open_session_ask(id, ask, identity));
    (library, window, id)
}

fn session_of(store: &AppStore, id: Uuid) -> &Session {
    store.session(id).unwrap()
}

#[test]
fn a_session_ask_shows_over_its_whole_page() {
    let (mut library, window, id) = store_with(ask(AskStyle::Terminal), Over::Session);
    let store = library.store_mut(window).unwrap();
    assert_eq!(shown_scope(session_of(store, id), true), Some(AskScope::Session));
    assert_eq!(
        shown_scope(session_of(store, id), false),
        None,
        "not shown: nothing draws"
    );
    store.session_mut(id).unwrap().scratch_active = true;
    let session = session_of(store, id);
    assert_eq!(
        shown_scope(session, true),
        Some(AskScope::Session),
        "a session ask covers the scratch"
    );
    assert!(wants_focus(session, false));
    assert!(!wants_focus(session, true));
    let (library, window, id) = store_with(ask(AskStyle::Terminal), Over::Gone);
    let gone = session_of(library.store(window).unwrap(), id);
    assert_eq!(shown_scope(gone, true), None, "a pane that is gone");
}

#[test]
fn a_pane_ask_shows_over_its_pane_while_it_is_laid_out() {
    let (mut library, window, id) = store_with(ask(AskStyle::Terminal), Over::Primary);
    let store = library.store_mut(window).unwrap();
    assert_eq!(
        shown_scope(session_of(store, id), true),
        Some(AskScope::Pane(Slot::Primary))
    );
    store.session_mut(id).unwrap().scratch_active = true;
    assert_eq!(
        shown_scope(session_of(store, id), true),
        None,
        "it hides under the scratch"
    );
    store.session_mut(id).unwrap().scratch_active = false;
    store.set_split_visibility(id, true, None);
    store
        .session_mut(id)
        .unwrap()
        .split_surface
        .replace(Box::new(FakeSurface::new("S")));
    store.set_split_visibility(id, false, None);
    let session = session_of(store, id);
    assert!(session.split.focused, "a new split focuses right");
    assert_eq!(shown_scope(session, true), None, "its pane is not laid out");
    assert!(!wants_focus(session, false), "the other pane holds the focus");
    store.set_split_visibility(id, true, None);
    assert_eq!(
        shown_scope(session_of(store, id), true),
        Some(AskScope::Pane(Slot::Primary))
    );
    store.set_split_focus(id, false);
    assert!(wants_focus(session_of(store, id), false));
}

#[test]
fn the_navigation_answers_once() {
    let ask = ask(AskStyle::Terminal);
    let mut navigation = AskNavigation::new(&ask);
    assert_eq!(navigation.highlighted(), Some(0), "the first non-destructive button");
    navigation.move_backward();
    assert_eq!(navigation.highlighted(), Some(2), "wraps to the last");
    assert_eq!(navigation.hotkey("x"), None, "no such hotkey");
    assert_eq!(navigation.hotkey("n"), Some(1));
}

#[test]
fn a_button_out_of_view_is_scrolled_in_the_least() {
    let viewport = px(10.)..px(110.);
    assert_eq!(
        reveal_offset(viewport.clone(), px(20.)..px(40.), px(-5.)),
        px(-5.),
        "in view"
    );
    assert_eq!(
        reveal_offset(viewport.clone(), px(100.)..px(130.), px(0.)),
        px(-20.),
        "below: up to the bottom"
    );
    assert_eq!(
        reveal_offset(viewport.clone(), px(0.)..px(20.), px(-30.)),
        px(-20.),
        "above: down to the top"
    );
    assert_eq!(
        reveal_offset(viewport, px(50.)..px(300.), px(0.)),
        px(-40.),
        "taller: its top"
    );
}

/// Presses `key` and lets the frames it asks for come.
fn press(cx: &mut gpui_kit::VisualTestContext, key: &str) {
    cx.simulate_keystrokes(key);
    cx.run_until_parked();
    cx.update(gpui_kit::Window::simulate_next_frame);
    cx.run_until_parked();
}

/// Whether button `id` lies inside the content's viewport.
fn shown(cx: &mut gpui_kit::VisualTestContext, id: &'static str) -> bool {
    let content = cx.debug_bounds("ask-content").expect("the content is laid out");
    let button = cx.debug_bounds(id).expect("the button is laid out");
    content.top() <= button.top() && button.bottom() <= content.bottom()
}

#[gpui_kit::test]
fn the_keyboard_brings_the_highlighted_button_into_view(cx: &mut gpui_kit::TestAppContext) {
    cx.update(gpui_kit::init);
    let mut pending = ask(AskStyle::Gui);
    pending.destructive_id = None;
    // a column of buttons far taller than the window
    pending.buttons = (0..16)
        .map(|index| button(&format!("b{index}"), &format!("Button {index}"), None))
        .collect();
    let look = Look {
        family: None,
        size: 13.0,
        foreground: Rgb::WHITE,
        background: Rgb::BLACK,
    };
    let owner = AskOwner::Session(Uuid::new_v4());
    let (dialog, cx) =
        cx.add_window_view(|_, cx| AskDialog::new(&pending, owner, look, crate::state::detached_poster(), cx));
    cx.simulate_resize(gpui_kit::size(px(300.), px(240.)));
    cx.update(|window, cx| {
        window.activate_window();
        let focus = dialog.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
    });
    cx.run_until_parked();
    assert!(!shown(cx, "ask-button-b15"), "the last button starts out of view");
    for _ in 0..15 {
        press(cx, "tab");
    }
    assert_eq!(dialog.read_with(cx, |dialog, _| dialog.highlighted()), Some(15));
    assert!(shown(cx, "ask-button-b15"), "Tab scrolled the highlighted button in");
    press(cx, "tab");
    assert!(shown(cx, "ask-button-b0"), "wrapped to the first, scrolled back up");
}
