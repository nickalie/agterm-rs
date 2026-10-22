//! The picker's rows, keys and answers (ported from the GTK build's `picker.rs` tests and upstream's `PaletteTests`),
//! and the view in GPUI's test window: the prefill filters, the caller's row is selected, typing refilters, one answer.

use std::cell::RefCell;

use gpui_kit::{Keystroke, TestAppContext};

use super::*;
use crate::terminal::input::Source;

fn item(id: &str, label: &str) -> PickItem {
    PickItem {
        id: id.to_owned(),
        label: label.to_owned(),
        subtitle: Some(format!("{label} subtitle")),
    }
}

fn items() -> Vec<PickItem> {
    vec![
        item("b", "beta"),
        item("a", "Alpha"),
        item("ns", "New Session"),
        item("nw", "New Workspace"),
    ]
}

fn pick(allow_custom: bool) -> PendingPick {
    PendingPick {
        id: "P1".to_owned(),
        items: items(),
        prompt: None,
        query: None,
        allow_custom,
        selection: Some("ns".to_owned()),
    }
}

#[test]
fn a_blank_query_keeps_the_callers_order() {
    let all: Vec<PickRow> = (0..4).map(PickRow::Item).collect();
    assert_eq!(filter_rows(&items(), "", false), all);
    assert_eq!(filter_rows(&items(), " \n\t", true), all, "trimmed of newlines too");
}

#[test]
fn a_query_ranks_labels_best_first_and_drops_misses() {
    assert_eq!(
        filter_rows(&items(), "new", false),
        [PickRow::Item(2), PickRow::Item(3)]
    );
    assert_eq!(filter_rows(&items(), "work", false), [PickRow::Item(3)]);
    assert!(
        filter_rows(&items(), "subtitle", false).is_empty(),
        "a caller's subtitle is not searched"
    );
}

#[test]
fn an_unmatched_query_offers_custom_text_only_when_allowed() {
    assert!(filter_rows(&items(), "zzz", false).is_empty());
    assert_eq!(
        filter_rows(&items(), "  zzz top ", true),
        [PickRow::Custom {
            label: "Use \"zzz top\"".to_owned(),
            query: "zzz top".to_owned()
        }]
    );
    assert_eq!(
        filter_rows(&items(), "alp", true),
        [PickRow::Item(1)],
        "a match offers no custom row"
    );
}

#[test]
fn the_selection_opens_on_the_callers_row_else_the_first() {
    let items = items();
    let rows = filter_rows(&items, "", false);
    assert_eq!(initial_selection(&rows, &items, Some("nw")), 3);
    assert_eq!(initial_selection(&rows, &items, None), 0);
    let filtered = filter_rows(&items, "alp", false);
    assert_eq!(
        initial_selection(&filtered, &items, Some("nw")),
        0,
        "a prefill hiding it leaves the first row"
    );
}

#[test]
fn moving_holds_at_the_ends() {
    assert_eq!(moved(0, -1, 3), 0);
    assert_eq!(moved(1, 1, 3), 2);
    assert_eq!(moved(2, 1, 3), 2);
    assert_eq!(moved(5, 0, 3), 2);
    assert_eq!(moved(0, 1, 0), 0);
}

#[test]
fn a_row_answers_with_its_callers_index_or_the_typed_text() {
    let pick = pick(true);
    let picked = outcome(&pick, &PickRow::Item(2)).unwrap();
    assert_eq!(picked.result, ControlPickOutcome::Picked);
    assert_eq!(picked.id.as_deref(), Some("ns"));
    assert_eq!(picked.index, Some(2));
    let custom = outcome(
        &pick,
        &PickRow::Custom {
            label: String::new(),
            query: "zzz".to_owned(),
        },
    )
    .unwrap();
    assert_eq!(custom.result, ControlPickOutcome::Custom);
    assert_eq!(custom.query.as_deref(), Some("zzz"));
    assert_eq!(outcome(&pick, &PickRow::Item(9)), None);
}

/// The key a GPUI keystroke spells, read off a US keyboard as without a native record.
fn key(source: &str) -> Option<PickKey> {
    let keystroke = Keystroke::parse(source).unwrap();
    pick_key(&KeyStroke::new(&keystroke, &Source::Keystroke, false))
}

#[test]
fn keys_move_run_cancel_and_stay_in_the_picker() {
    assert_eq!(key("up"), Some(PickKey::Move(-1)));
    assert_eq!(key("down"), Some(PickKey::Move(1)));
    assert_eq!(key("enter"), Some(PickKey::Run));
    assert_eq!(key("escape"), Some(PickKey::Cancel));
    assert_eq!(key("tab"), Some(PickKey::Swallow));
    assert_eq!(key("shift-tab"), Some(PickKey::Swallow));
    assert_eq!(key("ctrl-j"), Some(PickKey::Move(1)));
    assert_eq!(key("ctrl-k"), Some(PickKey::Move(-1)));
    assert_eq!(key("j"), None, "typed text");
    for held in ["ctrl-shift-j", "ctrl-alt-j", "ctrl-super-j", "alt-j"] {
        assert_eq!(key(held), None, "{held}");
    }
    assert_eq!(key("ctrl-a"), None, "the field's own");
    assert_eq!(key("left"), None);
}

// the keypad's 8 and 2 (XKB 80 and 88) are its Up and Down with NumLock off, digits for the field with it on
#[test]
fn keypad_arrows_move_only_with_num_lock_off() {
    use ngterm_core::ghostty_config::binds::Mods;

    use crate::terminal::input::NativeKey;
    let keypad = |keycode: u32, name: &str, locks: Mods, text: &str| {
        let native = NativeKey {
            keycode,
            text: text.into(),
            locks,
            ..NativeKey::default()
        };
        pick_key(&KeyStroke::new(
            &Keystroke::parse(name).unwrap(),
            &Source::Native(native),
            false,
        ))
    };
    assert_eq!(keypad(80, "up", Mods::empty(), ""), Some(PickKey::Move(-1)));
    assert_eq!(keypad(88, "down", Mods::empty(), ""), Some(PickKey::Move(1)));
    assert_eq!(keypad(80, "8", Mods::NUM_LOCK, "8"), None, "typed text");
    assert_eq!(keypad(88, "2", Mods::NUM_LOCK, "2"), None, "typed text");
}

// upstream's PaletteTests, by XKB keycodes (J 44, K 45, Dvorak's j on C 54 and k on V 55, A 38)
const KEYCODE_J: u32 = 44;
const KEYCODE_K: u32 = 45;

#[test]
fn latin_layout_moves_by_the_typed_letter() {
    assert_eq!(selection_step(KEYCODE_J, Some('j'), true), Some(1));
    assert_eq!(selection_step(KEYCODE_K, Some('k'), true), Some(-1));
    assert_eq!(selection_step(KEYCODE_J, Some('J'), true), Some(1), "caps lock");
    assert_eq!(selection_step(KEYCODE_K, Some('K'), true), Some(-1), "caps lock");
}

// regression: on a Cyrillic layout the J and K keys type `о` and `л`, and the selection did not move
#[test]
fn non_latin_layout_moves_by_the_physical_position() {
    assert_eq!(selection_step(KEYCODE_J, Some('о'), false), Some(1));
    assert_eq!(selection_step(KEYCODE_K, Some('л'), false), Some(-1));
    assert_eq!(selection_step(KEYCODE_J, None, false), Some(1));
}

// on Dvorak the physical J position types `h`, and `j` sits on the C key
#[test]
fn alternative_latin_layout_keeps_its_own_letter_positions() {
    assert_eq!(selection_step(KEYCODE_J, Some('h'), true), None);
    assert_eq!(selection_step(54, Some('j'), true), Some(1));
    assert_eq!(selection_step(55, Some('k'), true), Some(-1));
}

#[test]
fn other_keys_do_not_move() {
    assert_eq!(selection_step(38, Some('a'), true), None);
    assert_eq!(selection_step(38, Some('ф'), false), None);
    assert_eq!(
        selection_step(KEYCODE_J, Some('о'), true),
        None,
        "a Latin layout typing a non-ASCII character binds what it types"
    );
    assert_eq!(selection_step(KEYCODE_J, None, true), None);
}

#[test]
fn the_panel_is_scaled_fitted_and_centered_over_the_terminal_area() {
    let viewport = gpui_kit::size(px(1000.), px(600.));
    let at_default = Placement::of(viewport, 0.0, PANEL_WIDTH, Metrics::default());
    assert!((at_default.width - 520.0).abs() < 1e-9);
    assert!((at_default.left - 240.0).abs() < 1e-9);
    assert!((at_default.top - 72.0).abs() < 1e-9);
    // 16 pt scales the 520 ideal to 640, which a 221-pt sidebar's terminal area still fits
    let large = Placement::of(viewport, 221.0, PANEL_WIDTH, Metrics::new(16.0));
    assert!((large.width - 640.0).abs() < 1e-9);
    assert!(large.left + large.width <= 1000.0, "inside the window");
    let metrics = Metrics::new(16.0);
    assert!((metrics.secondary - 12.0).abs() < 1e-9 && (metrics.shortcut - 15.0).abs() < 1e-9);
    let small = Metrics::new(9.0);
    assert!((small.secondary - 8.0).abs() < 1e-9, "the floor");
}

#[gpui_kit::test]
fn the_picker_filters_moves_and_answers_once(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let answers: Rc<RefCell<Vec<ControlPickResult>>> = Rc::default();
    let seen = Rc::clone(&answers);
    let mut pending = pick(true);
    pending.query = Some("new".to_owned());
    // the caller's row is not the filter's first, so the seeding is told apart from the first-row fallback
    pending.selection = Some("nw".to_owned());
    let resolve: Resolve = Rc::new(move |result| seen.borrow_mut().push(result));
    let (picker, cx) =
        cx.add_window_view(move |window, cx| Picker::new(&pending, 0.0, Metrics::default(), resolve, window, cx));
    cx.run_until_parked();
    picker.read_with(cx, |picker, _| {
        assert_eq!(
            picker.rows(),
            [PickRow::Item(2), PickRow::Item(3)],
            "the prefill filters"
        );
        assert_eq!(picker.selection(), 1, "the caller's row is selected");
    });
    assert!(cx.debug_bounds("pick-palette").is_some() && cx.debug_bounds("pick-scrim").is_some());
    assert!(cx.debug_bounds("palette-item-nw").is_some(), "a row by its item id");
    picker.update(cx, |picker, cx| picker.key(PickKey::Move(-1), cx));
    assert_eq!(picker.read_with(cx, |picker, _| picker.selection()), 0);
    let field = picker.read_with(cx, |picker, _| picker.field().clone());
    field.update_in(cx, |field, window, cx| {
        field.focus(window, cx);
        field.select_all(window, cx);
    });
    cx.simulate_input("zzz");
    picker.read_with(cx, |picker, _| {
        assert_eq!(picker.rows().len(), 1, "the custom row");
        assert_eq!(picker.selection(), 0, "an edit goes back to the first row");
    });
    field.update_in(cx, InputState::select_all);
    cx.simulate_keystrokes("backspace");
    picker.read_with(cx, |picker, _| assert_eq!(picker.rows().len(), 4));
    picker.update(cx, |picker, cx| {
        picker.key(PickKey::Move(1), cx);
        picker.key(PickKey::Run, cx);
        picker.key(PickKey::Cancel, cx);
    });
    let answers = answers.borrow();
    assert_eq!(answers.len(), 1, "the first answer only");
    assert_eq!(answers[0].index, Some(1), "the caller's index");
}
