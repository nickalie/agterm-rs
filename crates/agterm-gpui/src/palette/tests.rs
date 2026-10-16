//! The palette's rows and requests (ported from the GTK build's `palette.rs` tests) in GPUI's test window: A→Z rows,
//! filtering, an inert row that does not run, the theme palette opening on the theme on screen and previewing the
//! selected row once it rests.

use std::cell::RefCell;

use agterm_core::palette_catalog::PaletteCommand;
use gpui_kit::TestAppContext;

use super::*;

fn entry(id: &str, action: PaletteAction) -> PaletteEntry {
    PaletteEntry {
        id: id.to_owned(),
        title: id.to_owned(),
        subtitle: None,
        shortcut: None,
        badge: None,
        indicator: None,
        action,
    }
}

fn view(mode: PaletteMode, generation: u64, entries: Vec<PaletteEntry>, enabled: &[bool]) -> PaletteView {
    PaletteView {
        mode,
        generation,
        looks: enabled
            .iter()
            .map(|enabled| RowLook {
                enabled: *enabled,
                shortcut: None,
                glyph: None,
            })
            .collect(),
        entries,
        initial: None,
    }
}

#[test]
fn the_selection_stays_on_its_row_when_the_rows_change() {
    let entries = [
        entry("a", PaletteAction::Theme(None)),
        entry("b", PaletteAction::Theme(None)),
        entry("c", PaletteAction::Theme(None)),
    ];
    assert_eq!(reselect(&entries, &[0, 1, 2], Some("c"), 0), 2);
    assert_eq!(reselect(&entries, &[2, 0], Some("a"), 0), 1);
    assert_eq!(reselect(&entries, &[0, 1], Some("gone"), 5), 1, "clamped");
    assert_eq!(reselect(&entries, &[], None, 3), 0);
}

#[test]
fn shortcuts_read_as_gtk_labels_and_custom_text() {
    let chord = agterm_core::keymap::parse_keybind("ctrl+shift+p").unwrap().remove(0);
    assert_eq!(shortcut_label(&PaletteShortcut::Chord(chord)), "Shift+Ctrl+P");
    assert_eq!(
        shortcut_label(&PaletteShortcut::Text("ctrl+a>g".to_owned())),
        "ctrl+a>g"
    );
}

/// A palette over `opened` in a test window, its requests collected.
fn open(
    cx: &mut TestAppContext,
    opened: PaletteView,
) -> (
    Entity<Palette>,
    &mut gpui_kit::VisualTestContext,
    Rc<RefCell<Vec<Request>>>,
) {
    cx.update(gpui_kit::init);
    let requests: Rc<RefCell<Vec<Request>>> = Rc::default();
    let seen = Rc::clone(&requests);
    let sink: Sink = Rc::new(move |request| seen.borrow_mut().push(request));
    let (palette, cx) =
        cx.add_window_view(move |window, cx| Palette::new(opened, 0.0, Metrics::default(), sink, window, cx));
    cx.run_until_parked();
    (palette, cx, requests)
}

#[gpui_kit::test]
fn the_palette_filters_moves_and_runs_live_rows(cx: &mut TestAppContext) {
    let entries = vec![
        entry("New Session", PaletteAction::Command(PaletteCommand::NewSession)),
        entry("Swap Panes", PaletteAction::Command(PaletteCommand::SwapPanes)),
        entry("Close Session", PaletteAction::Command(PaletteCommand::CloseSession)),
    ];
    let (palette, cx, requests) = open(cx, view(PaletteMode::Actions, 1, entries, &[true, false, true]));
    let titles = |cx: &mut gpui_kit::VisualTestContext| palette.read_with(cx, |palette, _| palette.titles());
    assert_eq!(titles(cx), ["Close Session", "New Session", "Swap Panes"], "A→Z");
    assert!(cx.debug_bounds("command-palette").is_some() && cx.debug_bounds("palette-scrim").is_some());
    let field = palette.read_with(cx, |palette, _| palette.field().clone());
    field.update_in(cx, |field, window, cx| field.focus(window, cx));
    cx.simulate_input("swap");
    assert_eq!(titles(cx), ["Swap Panes"]);
    palette.update(cx, |palette, cx| palette.key(PickKey::Run, cx));
    assert!(requests.borrow().is_empty(), "an inert row does not run");
    field.update_in(cx, InputState::select_all);
    cx.simulate_keystrokes("backspace");
    assert_eq!(titles(cx).len(), 3);
    palette.update(cx, |palette, cx| {
        palette.key(PickKey::Move(1), cx);
        palette.key(PickKey::Run, cx);
        palette.key(PickKey::Cancel, cx);
        palette.launcher(BuiltinAction::CommandPalette, cx);
    });
    assert_eq!(
        *requests.borrow(),
        [
            Request::Run(PaletteAction::Command(PaletteCommand::NewSession)),
            Request::Close,
            Request::Launcher(BuiltinAction::CommandPalette),
        ]
    );
}

#[gpui_kit::test]
fn the_theme_palette_opens_on_the_theme_on_screen_and_previews_once_the_selection_rests(cx: &mut TestAppContext) {
    let themes = vec![
        entry("theme:A", PaletteAction::Theme(Some("A".to_owned()))),
        entry("theme:B", PaletteAction::Theme(Some("B".to_owned()))),
        entry("theme:C", PaletteAction::Theme(Some("C".to_owned()))),
    ];
    let mut opened = view(PaletteMode::Themes, 2, themes, &[true, true, true]);
    opened.initial = Some("theme:B".to_owned());
    let (palette, cx, requests) = open(cx, opened);
    assert_eq!(palette.read_with(cx, |palette, _| palette.selection()), 1, "on screen");
    assert!(requests.borrow().is_empty(), "opening previews nothing");
    palette.update(cx, |palette, cx| {
        palette.key(PickKey::Move(-1), cx);
        palette.key(PickKey::Move(2), cx);
    });
    cx.run_until_parked();
    assert!(requests.borrow().is_empty(), "debounced");
    cx.executor().advance_clock(PREVIEW_DEBOUNCE);
    cx.run_until_parked();
    assert_eq!(
        *requests.borrow(),
        [Request::Preview(Some("C".to_owned()))],
        "only the row the selection rested on"
    );
    requests.borrow_mut().clear();
    palette.update(cx, |palette, cx| {
        palette.key(PickKey::Move(-1), cx);
        palette.key(PickKey::Run, cx);
    });
    cx.executor().advance_clock(PREVIEW_DEBOUNCE);
    cx.run_until_parked();
    assert_eq!(
        *requests.borrow(),
        [Request::Run(PaletteAction::Theme(Some("B".to_owned())))],
        "a run drops the preview still waiting"
    );
}
