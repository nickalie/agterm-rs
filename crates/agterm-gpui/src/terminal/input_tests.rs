//! Headless tests of the terminal's input (plan > Task 14): key events as the terminal and the keymap read them, the
//! routing order (the window's keymap before ghostty's binds before the encoder), the input method's preedit, the
//! mouse's selections, reports and wheel, links, focus reports, and a program's clipboard access under the prompt.
//!
//! Panes run `/bin/sh` scripts with no `AGTERM_*` or `ZMX_*` variable inherited; GPUI's test platform holds the
//! clipboards and records the URL it was asked to open, so nothing reaches the desktop. The fake face's cells are
//! 10x21 at scale 1 with 2 px of padding.

use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use agterm_core::clipboard_policy::{ClipboardAccess, ClipboardDecision};
use agterm_core::ghostty_config::{DEFAULTS, Loader};
use agterm_core::keymap::{CustomCommand, linux_defaults, parse_keymap};
use agterm_core::link_policy::ClickOrigin;
use agterm_core::persist::settings::LinkOpenMode;
use agterm_term::{GridPoint, Pane, PaneCommand, PaneConfig, SelectionText};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, EntityInputHandler as _, InteractiveElement as _, IntoElement,
    Keystroke, Modifiers, MouseButton, ParentElement as _, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent,
    Styled as _, TestAppContext, VisualTestContext, Window, div, point, px, size,
};

use super::input::{self, KeyStroke, NativeKey, Source};
use super::links::{self, LinkKind};
use super::{Terminal, TerminalConfig, TerminalEvent, TerminalSpec, TerminalView, ViewRole};
use crate::keymap_dispatch::{self, Fired, KeyOrigin};

const DEADLINE: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------------------------------------------
// key events

fn keystroke(text: &str) -> Keystroke {
    Keystroke::parse(text).unwrap().with_simulated_ime()
}

#[test]
fn a_native_key_is_its_physical_key_with_the_layouts_character() {
    // the Russian layout's С key is the US C key: ctrl+shift+key_c still copies
    let native = NativeKey {
        keycode: 54,
        modifiers: Modifiers {
            control: true,
            shift: true,
            ..Modifiers::default()
        },
        unshifted: Some('с'),
        ascii_layout: false,
        text: "С".into(),
        ..NativeKey::default()
    };
    let stroke = KeyStroke::new(&keystroke("ctrl-shift-c"), &Source::Native(native), false);
    assert_eq!(stroke.key.name(), "key_c");
    assert_eq!(stroke.unshifted, Some('с'));
    assert_eq!(input::chord(&stroke).map(|chord| chord.key).as_deref(), Some("c"));
    for (code, name) in [
        (36, "enter"),
        (111, "arrow_up"),
        (10, "digit_1"),
        (9, "escape"),
        (9999, "unidentified"),
    ] {
        assert_eq!(input::physical_key(code).name(), name, "{code}");
    }
}

#[test]
fn a_key_press_is_classified_for_the_status_glyph_as_the_gtk_build_does() {
    use agterm_core::model::StatusKeystroke::{Interrupt, Other, Submit};
    let classify = |text: &str| input::status_keystroke(&KeyStroke::new(&keystroke(text), &Source::Keystroke, false));
    for (text, expected) in [
        ("escape", Interrupt),
        ("shift-escape", Interrupt),
        ("ctrl-c", Interrupt),
        ("ctrl-shift-c", Other),
        ("enter", Submit),
        ("shift-enter", Other),
        ("alt-enter", Other),
        ("a", Other),
    ] {
        assert_eq!(classify(text), expected, "{text}");
    }
    // the Russian layout's С key with Ctrl is a bare Ctrl-C; Dvorak's `j` on the physical C key is not
    let native = |text: &str, unshifted: char| NativeKey {
        keycode: 54,
        modifiers: Modifiers {
            control: true,
            ..Modifiers::default()
        },
        unshifted: Some(unshifted),
        text: text.into(),
        ..NativeKey::default()
    };
    let at_c =
        |key: NativeKey| input::status_keystroke(&KeyStroke::new(&keystroke("ctrl-c"), &Source::Native(key), false));
    assert_eq!(at_c(native("с", 'с')), Interrupt);
    assert_eq!(at_c(native("j", 'j')), Other);
}

#[test]
fn without_a_native_key_the_key_is_read_off_a_us_keyboard() {
    let bang = KeyStroke::new(&keystroke("!"), &Source::Keystroke, false);
    assert_eq!(
        (bang.key.name(), bang.unshifted, bang.text.as_str()),
        ("digit_1", Some('1'), "!")
    );
    assert!(
        bang.mods.contains(agterm_core::ghostty_config::binds::Mods::SHIFT),
        "Shift restored"
    );
    let ctrl_a = KeyStroke::new(&keystroke("ctrl-a"), &Source::Keystroke, false);
    assert_eq!((ctrl_a.key.name(), ctrl_a.unshifted), ("key_a", Some('a')));
    for (text, name) in [
        ("enter", "enter"),
        ("f5", "f5"),
        ("pageup", "page_up"),
        ("space", "space"),
    ] {
        assert_eq!(
            KeyStroke::new(&keystroke(text), &Source::Keystroke, false).key.name(),
            name
        );
    }
    let release = KeyStroke::new(&keystroke("a"), &Source::Keystroke, true);
    assert_eq!(release.action, agterm_core::ghostty_config::binds::KeyAction::Release);
    // input method text has no position
    let text = KeyStroke::new(&keystroke("é"), &Source::Text, false);
    assert_eq!((text.key.name(), text.text.as_str()), ("unidentified", "é"));
}

#[test]
fn chords_name_keys_as_the_keymap_does() {
    let chord = |text: &str| input::chord(&KeyStroke::new(&keystroke(text), &Source::Keystroke, false));
    let t = chord("ctrl-shift-t").unwrap();
    assert_eq!(t.key, "t");
    assert_eq!(
        t.mods,
        agterm_core::keymap::Modifiers::CONTROL | agterm_core::keymap::Modifiers::SHIFT
    );
    assert_eq!(chord("backspace").unwrap().key, "delete");
    assert_eq!(chord("enter").unwrap().key, "return");
    assert_eq!(
        chord("super-left").unwrap().mods,
        agterm_core::keymap::Modifiers::COMMAND
    );
    // upstream's chordKey: by what the layout types, or the Latin key on a layout that types no ASCII
    assert_eq!(input::chord_key(32, Some('щ'), false).as_deref(), Some("o"));
    assert_eq!(input::chord_key(54, Some('j'), true).as_deref(), Some("j"));
    assert_eq!(input::chord_key(94, Some('/'), false), None);
    assert_eq!(input::chord_key(65, Some(' '), true), None);
    assert_eq!(input::latin_key(51), Some("\\"));
}

#[test]
fn the_wheel_multiplier_and_the_pointer_shapes_follow_ghostty() {
    use agterm_term::MouseShape;
    use gpui_kit::CursorStyle;
    assert_eq!(super::scroll_multiplier(None), (1.0, 3.0));
    assert_eq!(super::scroll_multiplier(Some("precision:2,discrete:5")), (2.0, 5.0));
    assert_eq!(super::scroll_multiplier(Some("4")), (4.0, 4.0));
    assert_eq!(super::scroll_multiplier(Some("discrete:0")), (1.0, 0.01));
    assert_eq!(super::cursor_style(MouseShape::Default), CursorStyle::IBeam);
    assert_eq!(super::cursor_style(MouseShape::Pointer), CursorStyle::PointingHand);
    assert_eq!(super::cursor_style(MouseShape::EwResize), CursorStyle::ResizeLeftRight);
}

// ---------------------------------------------------------------------------------------------------------------
// links on a real pane

/// `sh -c script` with the inherited `AGTERM_*`/`ZMX_*` variables removed, in `dir`.
fn command(script: &str, dir: &Path) -> PaneCommand {
    let mut command = PaneCommand::new("/bin/sh").arg("-c").arg(script).cwd(dir);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("AGTERM_") || name.starts_with("ZMX_") {
            command = command.env_remove(key);
        }
    }
    command
}

fn wait_frame(pane: &Pane, needle: &str) -> std::sync::Arc<agterm_term::Frame> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let frame = pane.frame().frame;
        if frame.lines().join("\n").contains(needle) {
            return frame;
        }
        assert!(Instant::now() < deadline, "timed out:\n{}", frame.lines().join("\n"));
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn urls_are_found_across_soft_wraps_and_osc8_links_by_their_cells() {
    let dir = tempfile::tempdir().unwrap();
    let size = agterm_term::GridSize {
        cols: 20,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    };
    let script = "printf 'go https://example.com/a/long/path. \\033]8;;https://osc8.test/x\\033\\\\[here]\\033]8;;\\033\\\\'; \
                  exec sleep 30";
    let pane = Pane::spawn(&command(script, dir.path()), &PaneConfig::new(size)).unwrap();
    let frame = wait_frame(&pane, "e]");
    // `go https://example.c` / `om/a/long/path. [her` / `e]`
    let url = links::url_at(&frame, 5, 1).expect("a URL on the wrapped row");
    assert_eq!(url.url, "https://example.com/a/long/path");
    assert_eq!(url.kind, LinkKind::Url);
    assert_eq!(url.cells, [(0, 3, 19), (1, 0, 13)]);
    assert!(url.covers(4, 0) && !url.covers(14, 1), "the full stop is no part of it");
    assert_eq!(links::url_at(&frame, 0, 0), None, "`go` is no link");
    let osc8 = links::osc8_at(&pane, &frame, 16, 1).expect("the hyperlinked cells");
    assert_eq!(osc8.url, "https://osc8.test/x");
    assert_eq!(osc8.cells, [(1, 16, 19)]);
    // Ctrl (or Super) alone makes them live; the hyperlink wins over a URL under it
    let mods = agterm_core::ghostty_config::binds::Mods::CTRL;
    assert_eq!(
        links::link_at(&pane, &frame, GridPoint::viewport(16, 1), mods, true).map(|link| link.kind),
        Some(LinkKind::Osc8)
    );
    let none = agterm_core::ghostty_config::binds::Mods::empty();
    assert_eq!(
        links::link_at(&pane, &frame, GridPoint::viewport(16, 1), none, true),
        None
    );
}

// ---------------------------------------------------------------------------------------------------------------
// the terminal in a test window

fn config(extra: &str) -> TerminalConfig {
    let mut loader = Loader::new(None);
    loader.load_text("ghostty-defaults.conf", DEFAULTS);
    loader.load_text(
        "test.conf",
        &format!("window-padding-x = 2\nwindow-padding-y = 2\n{extra}"),
    );
    TerminalConfig::from_ghostty(&loader.finish().finalize(false, &[]).config)
}

type Events = Rc<std::cell::RefCell<Vec<TerminalEvent>>>;

struct Fixture<'a> {
    terminal: Entity<Terminal>,
    view: Entity<TerminalView>,
    events: Events,
    cx: &'a mut VisualTestContext,
    dir: tempfile::TempDir,
}

impl Fixture<'_> {
    fn pane(&self) -> Rc<Pane> {
        self.terminal
            .read_with(self.cx, |terminal, _| terminal.shared().pane().unwrap())
    }

    /// Runs the app until `done` holds (the pane's threads run meanwhile), or fails.
    fn settle(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out:\n{}", self.painted());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn painted(&mut self) -> String {
        self.view.read_with(self.cx, |view, _| {
            view.painted().map(|frame| frame.lines().join("\n")).unwrap_or_default()
        })
    }

    fn wait_painted(&mut self, needle: &str) {
        self.settle(|fixture| fixture.painted().contains(needle));
    }

    fn focus(&mut self) {
        let view = self.view.clone();
        self.cx.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
        });
        self.cx.run_until_parked();
    }

    /// The window position of the middle of viewport cell (`col`, `row`).
    fn at(col: u16, row: u16) -> Point<Pixels> {
        point(
            px(2.0 + f32::from(col) * 10.0 + 5.0),
            px(2.0 + f32::from(row) * 21.0 + 10.0),
        )
    }

    fn click(&mut self, col: u16, row: u16, modifiers: Modifiers) {
        let at = Self::at(col, row);
        self.cx.simulate_mouse_down(at, MouseButton::Left, modifiers);
        self.cx.simulate_mouse_up(at, MouseButton::Left, modifiers);
    }

    fn selection(&self) -> Option<String> {
        self.pane().selection_text(SelectionText::Raw)
    }

    /// Presses and releases each key of `keys` (space-separated, GPUI's keystroke syntax).
    fn type_keys(&mut self, keys: &str) {
        for key in keys.split(' ') {
            let keystroke = keystroke(key);
            let pressed = keystroke.clone();
            self.cx.update(|window, cx| window.dispatch_keystroke(pressed, cx));
            self.cx.simulate_event(gpui_kit::KeyUpEvent { keystroke });
        }
        self.cx.run_until_parked();
    }

    /// Makes the test window the active one, as a window holding the keyboard is.
    fn activate(&mut self) {
        self.cx.update(|window, _| window.activate_window());
        self.cx.run_until_parked();
    }
}

/// A terminal with `config` running `script` (which prints `ready`) in a 640x480 test window, its events collected.
fn fixture<'a>(cx: &'a mut TestAppContext, script: &str, config: TerminalConfig) -> Fixture<'a> {
    let dir = tempfile::tempdir().unwrap();
    // the pane's threads wake the pump from outside the test scheduler
    cx.executor().allow_parking();
    let command = command(script, dir.path());
    let spawn: super::Spawn = Box::new(move |config: PaneConfig| Pane::spawn(&command, &config));
    let spec = TerminalSpec {
        token: "pane-token".into(),
        live: false,
        font_size: None,
    };
    let terminal = cx.new(|cx| Terminal::new(spec, config, spawn, cx));
    let events: Events = Rc::default();
    let seen = Rc::clone(&events);
    cx.update(|cx| {
        cx.subscribe(&terminal, move |_, event: &TerminalEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
    cx.simulate_scale_factor_change(1.0);
    cx.simulate_resize(size(px(640.), px(480.)));
    let mut fixture = Fixture {
        terminal,
        view,
        events,
        cx,
        dir,
    };
    fixture.wait_painted("ready");
    fixture
}

/// `cat -v` behind a tty that echoes nothing itself: what reaches the program is printed visibly.
const ECHO: &str = "stty -echo -icanon; printf 'ready\\r\\n'; exec cat -v";

#[gpui_kit::test]
fn the_keymap_takes_its_chords_before_the_binds_and_the_binds_before_the_encoder(cx: &mut TestAppContext) {
    let mut f = fixture(cx, ECHO, config(""));
    let keymap_text = "command \"x\" ctrl+shift+b true\ncommand \"e\" ctrl+e true\n";
    let (keymap, _) = parse_keymap(keymap_text, &linux_defaults());
    keymap_dispatch::load_keymap(&keymap);
    let window = f.cx.update(|window, _| window.window_handle());
    keymap_dispatch::register_window(window, KeyOrigin::Window(uuid::Uuid::from_u128(1)));
    let _ = keymap_dispatch::take_fired();
    f.focus();

    // the keymap: a custom command and a built-in menu chord never reach the terminal
    f.type_keys("ctrl-shift-b ctrl-shift-t ctrl-e");
    let fired = keymap_dispatch::take_fired();
    let names: Vec<String> = fired
        .iter()
        .map(|fired| match fired {
            Fired::Command(CustomCommand { name, .. }) => name.clone(),
            Fired::Builtin(action) => action.name().to_owned(),
        })
        .collect();
    assert_eq!(names, ["x", "new_session", "e"]);
    // ghostty's binds: agterm's physical select-all, performed, not encoded
    f.type_keys("ctrl-shift-a");
    f.settle(|f| f.pane().selection().is_some());
    // the encoder: everything else, legacy encoded (`cat -v` shows Ctrl+B's 0x02 as ^B)
    f.type_keys("x ctrl-b enter");
    f.wait_painted("x^B");
    // nothing the keymap or the binds took reached the program: the line holds exactly what was encoded
    let painted = f.painted();
    assert_eq!(painted.lines().nth(1), Some("x^B"), "{painted}");
    keymap_dispatch::unregister_window(window);
    // an unregistered window has no keymap: the chord is the terminal's
    f.type_keys("ctrl-e enter ctrl-shift-b enter");
    // Ghostty encodes Ctrl+Shift+letter unambiguously, as CSI 98;6u
    f.wait_painted("x^B\n^E\n^[[98;6u");
    let painted = f.painted();
    let lines: Vec<&str> = painted.lines().skip(1).take(3).collect();
    assert_eq!(lines, ["x^B", "^E", "^[[98;6u"]);
    assert!(keymap_dispatch::take_fired().is_empty());
}

#[gpui_kit::test]
fn a_composition_shows_at_the_cursor_and_its_commit_is_typed(cx: &mut TestAppContext) {
    let mut f = fixture(cx, "stty -echo -icanon; printf 'ready\\r\\n'; exec cat", config(""));
    f.focus();
    let view = f.view.clone();
    f.cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(None, "にほ", None, window, cx);
        });
    });
    let shared = f.view.read_with(f.cx, |view, _| Rc::clone(view.shared()));
    assert_eq!(shared.preedit().as_deref(), Some("にほ"));
    let marked =
        f.cx.update(|window, cx| view.update(cx, |view, cx| view.marked_text_range(window, cx)));
    assert_eq!(marked, Some(0..2));
    // a key while the input method composes is the input method's
    let composing = KeyStroke {
        composing: true,
        ..KeyStroke::new(&keystroke("a"), &Source::Keystroke, false)
    };
    let taken =
        f.cx.update(|window, cx| view.update(cx, |view, cx| view.key_down(&composing, window, cx)));
    assert!(!taken);
    // the candidate window sits at the cursor's cell
    let bounds = f.cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.bounds_for_range(
                0..1,
                gpui_kit::Bounds::new(point(px(0.), px(0.)), size(px(640.), px(480.))),
                window,
                cx,
            )
        })
    });
    assert_eq!(bounds.map(|bounds| bounds.size), Some(size(px(10.), px(21.))));
    f.cx.update(|window, cx| view.update(cx, |view, cx| view.replace_text_in_range(None, "日本", window, cx)));
    f.wait_painted("日本");
    assert_eq!(shared.preedit(), None);
    f.cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(None, "x", None, window, cx);
            view.unmark_text(window, cx);
        });
    });
    assert_eq!(shared.preedit(), None);
    // a key after it reaches the program right behind the committed text: the abandoned `x` never went
    f.type_keys("z");
    f.wait_painted("z");
    let painted = f.painted();
    assert!(painted.contains("日本z"), "{painted}");
}

#[gpui_kit::test]
fn drags_and_double_and_triple_clicks_select_and_copy_to_primary(cx: &mut TestAppContext) {
    let mut f = fixture(cx, "printf 'ready\\r\\nhello world\\r\\n'; exec sleep 30", config(""));
    // a drag from the first cell past the middle of the fifth
    f.cx.simulate_mouse_down(
        point(px(3.), Fixture::at(0, 1).y),
        MouseButton::Left,
        Modifiers::default(),
    );
    f.cx.simulate_mouse_move(
        point(px(2. + 4. * 10. + 8.), Fixture::at(0, 1).y),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    f.cx.simulate_mouse_up(
        point(px(2. + 4. * 10. + 8.), Fixture::at(0, 1).y),
        MouseButton::Left,
        Modifiers::default(),
    );
    assert_eq!(f.selection().as_deref(), Some("hello"));
    let primary = f.cx.update(|_, cx| cx.read_from_primary().and_then(|item| item.text()));
    assert_eq!(primary.as_deref(), Some("hello"));
    // a single click clears it; a double click takes the word, a triple the line
    std::thread::sleep(Duration::from_millis(600));
    f.click(8, 1, Modifiers::default());
    assert_eq!(f.selection(), None);
    f.click(8, 1, Modifiers::default());
    assert_eq!(f.selection().as_deref(), Some("world"));
    f.click(8, 1, Modifiers::default());
    assert_eq!(f.selection().as_deref(), Some("hello world"));
    // Shift extends the selection a click sequence made, once the repeat interval passed (word by word after a
    // double click)
    std::thread::sleep(Duration::from_millis(600));
    f.click(1, 1, Modifiers::default());
    f.click(1, 1, Modifiers::default());
    assert_eq!(f.selection().as_deref(), Some("hello"));
    std::thread::sleep(Duration::from_millis(600));
    f.click(8, 1, Modifiers::shift());
    assert_eq!(f.selection().as_deref(), Some("hello world"));
}

#[gpui_kit::test]
fn a_double_click_selects_a_whole_url_only_while_urls_are_links(cx: &mut TestAppContext) {
    let script = "printf 'ready\\r\\nsee https://example.com/p\\r\\n'; exec sleep 30";
    let mut f = fixture(cx, script, config("link-url = false\n"));
    f.wait_painted("example.com");
    // `link-url = false`: the word under the pointer, as Ghostty's double click finds no link
    f.click(14, 1, Modifiers::default());
    f.click(14, 1, Modifiers::default());
    assert_eq!(f.selection().as_deref(), Some("//example.com/p"));
    f.terminal
        .update(f.cx, |terminal, cx| terminal.set_config(config(""), cx));
    std::thread::sleep(Duration::from_millis(600));
    f.click(14, 1, Modifiers::default());
    f.click(14, 1, Modifiers::default());
    assert_eq!(f.selection().as_deref(), Some("https://example.com/p"));
}

#[gpui_kit::test]
fn a_program_with_the_mouse_gets_clicks_and_shift_keeps_the_mouse_for_selecting(cx: &mut TestAppContext) {
    let script = "stty -echo -icanon; printf '\\033[?1000hready\\r\\nhello\\r\\n'; exec cat -v";
    let mut f = fixture(cx, script, config(""));
    f.click(0, 1, Modifiers::default());
    // X10-compatible encoding: ESC [ M, the button, then column and row + 33
    f.wait_painted("^[[M !\"");
    assert_eq!(f.selection(), None);
    std::thread::sleep(Duration::from_millis(600));
    let shift = Modifiers::shift();
    f.cx.simulate_mouse_down(Fixture::at(0, 1), MouseButton::Left, shift);
    f.cx.simulate_mouse_move(
        point(px(2. + 4. * 10. + 8.), Fixture::at(0, 1).y),
        Some(MouseButton::Left),
        shift,
    );
    f.cx.simulate_mouse_up(
        point(px(2. + 4. * 10. + 8.), Fixture::at(0, 1).y),
        MouseButton::Left,
        shift,
    );
    assert_eq!(f.selection().as_deref(), Some("hello"));
}

#[gpui_kit::test]
fn the_wheel_scrolls_the_viewport_back(cx: &mut TestAppContext) {
    let script = "i=0; while [ $i -lt 60 ]; do echo line-$i; i=$((i+1)); done; printf ready; exec sleep 30";
    let mut f = fixture(cx, script, config(""));
    assert!(!f.painted().contains("line-30"));
    f.cx.simulate_event(ScrollWheelEvent {
        position: Fixture::at(3, 3),
        // one notch up, as GPUI's Linux backends hand it (three lines)
        delta: ScrollDelta::Lines(point(0., 3. * 4.)),
        ..ScrollWheelEvent::default()
    });
    f.wait_painted("line-30");
}

#[gpui_kit::test]
fn a_ctrl_click_on_a_link_opens_it_through_the_link_route(cx: &mut TestAppContext) {
    let script = "printf 'ready\\r\\nsee \\033]8;;https://osc8.test/x\\033\\\\here\\033]8;;\\033\\\\ or https://example.com/p'; exec sleep 30";
    let mut f = fixture(cx, script, config(""));
    f.wait_painted("example.com/p");
    // hovering without Ctrl shows nothing; with Ctrl the hyperlink is live and underlined
    f.cx.simulate_mouse_move(Fixture::at(5, 1), None, Modifiers::default());
    assert!(f.view.read_with(f.cx, |view, _| view.hovered_link().is_none()));
    f.cx.simulate_mouse_move(Fixture::at(5, 1), None, Modifiers::control());
    let hovered = f
        .view
        .read_with(f.cx, |view, _| view.hovered_link().cloned())
        .expect("a live link");
    assert_eq!(
        (hovered.url.as_str(), hovered.cells.as_slice()),
        ("https://osc8.test/x", &[(1, 4, 7)][..])
    );
    assert_eq!(f.view.read_with(f.cx, |view, _| view.link.clone()), [(1, 4, 7)]);
    // the hovered link is what `copy_url_to_clipboard` copies
    let shared = f.view.read_with(f.cx, |view, _| Rc::clone(view.shared()));
    assert_eq!(shared.hovered_url().as_deref(), Some("https://osc8.test/x"));
    let mut surface = super::TerminalSurface::new(Rc::clone(&shared));
    assert!(agterm_host::Surface::binding_action(
        &mut surface,
        "copy_url_to_clipboard"
    ));
    f.cx.run_until_parked();
    let copied = f.cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("https://osc8.test/x"));
    f.click(5, 1, Modifiers::control());
    f.cx.run_until_parked();
    assert!(
        f.events
            .borrow()
            .contains(&TerminalEvent::OpenUrl("https://osc8.test/x".into()))
    );
    // a detected URL, the same way
    f.cx.simulate_mouse_move(Fixture::at(20, 1), None, Modifiers::control());
    f.click(20, 1, Modifiers::control());
    f.cx.run_until_parked();
    assert!(
        f.events
            .borrow()
            .contains(&TerminalEvent::OpenUrl("https://example.com/p".into()))
    );
    // the default route hands a web link to the platform's opener (the test platform only records it)
    f.cx.update(|_, cx| {
        links::follow_link("https://example.com/p", LinkOpenMode::Browser, ClickOrigin::Quick, cx);
        links::follow_link("javascript:alert(1)", LinkOpenMode::Browser, ClickOrigin::Quick, cx);
    });
    assert_eq!(f.cx.opened_url().as_deref(), Some("https://example.com/p"));
}

#[gpui_kit::test]
fn read_only_mode_keeps_the_users_input_from_the_program(cx: &mut TestAppContext) {
    let script = "stty -echo -icanon; printf '\\033[?1000hready\\r\\nhello\\r\\n'; exec cat -v";
    let mut f = fixture(cx, script, config(""));
    f.focus();
    let shared = f.view.read_with(f.cx, |view, _| Rc::clone(view.shared()));
    shared.set_readonly(true);
    // keys, input-method text and the primary selection's paste stay out; a click selects instead of reporting
    f.type_keys("x");
    let view = f.view.clone();
    f.cx.update(|window, cx| view.update(cx, |view, cx| view.replace_text_in_range(None, "y", window, cx)));
    f.cx.update(|_, cx| cx.write_to_primary(ClipboardItem::new_string("p".into())));
    f.cx.simulate_mouse_down(Fixture::at(1, 1), MouseButton::Middle, Modifiers::default());
    f.cx.simulate_mouse_up(Fixture::at(1, 1), MouseButton::Middle, Modifiers::default());
    f.click(1, 1, Modifiers::default());
    f.click(1, 1, Modifiers::default());
    assert_eq!(f.selection().as_deref(), Some("hello"));
    shared.set_readonly(false);
    f.type_keys("z");
    f.wait_painted("z");
    let painted = f.painted();
    // only the key typed after read-only mode ended reached `cat -v`
    assert!(painted.trim_end().ends_with("hello\nz"), "{painted}");
    assert!(!painted.contains("^[[M"), "no mouse report: {painted}");
}

#[gpui_kit::test]
fn focus_reports_follow_the_view_and_its_window(cx: &mut TestAppContext) {
    let script = "stty -echo -icanon; printf '\\033[?1004hready\\r\\n'; exec cat -v";
    let mut f = fixture(cx, script, config(""));
    // the state the terminal starts in is no change: nothing is reported until the focus moves
    f.activate();
    f.focus();
    f.wait_painted("^[[I");
    assert!(!f.painted().contains("^[[O"), "{}", f.painted());
    f.cx.update(Window::blur);
    f.wait_painted("^[[I^[[O");
}

/// `printf` of an OSC 52 write of `text` (already base64).
fn osc52_write(base64: &str) -> String {
    format!("printf '\\033]52;c;{base64}\\007'")
}

#[gpui_kit::test]
fn clipboard_writes_ask_and_a_session_choice_is_remembered(cx: &mut TestAppContext) {
    // each write waits for a line, so each gets its own prompt
    let script = format!(
        "stty -echo; printf 'ready'; read _; {}; read _; {}; read _; {}; exec sleep 30",
        osc52_write("Zmlyc3Q="),
        osc52_write("c2Vjb25k"),
        osc52_write("dGhpcmQ=")
    );
    let mut f = fixture(cx, &script, config("clipboard-write = ask\n"));
    f.cx.write_to_clipboard(ClipboardItem::new_string("before".into()));
    let clipboard = |f: &mut Fixture<'_>| f.cx.read_from_clipboard().and_then(|item| item.text());

    f.pane().write(b"\n").unwrap();
    f.settle(|f| f.cx.has_pending_prompt());
    f.cx.simulate_prompt_answer("Deny");
    f.cx.run_until_parked();
    assert_eq!(clipboard(&mut f).as_deref(), Some("before"));

    f.pane().write(b"\n").unwrap();
    f.settle(|f| f.cx.has_pending_prompt());
    f.cx.simulate_prompt_answer("Always Allow This Session");
    f.settle(|f| clipboard(f).as_deref() == Some("second"));

    f.pane().write(b"\n").unwrap();
    f.settle(|f| clipboard(f).as_deref() == Some("third"));
    assert!(!f.cx.has_pending_prompt(), "remembered for the session");
    let decision =
        f.cx.update(|_, cx| cx.global::<super::ClipboardPrompts>().decision(ClipboardAccess::Write));
    assert_eq!(decision, ClipboardDecision::Allow);
}

/// A shell loop reading the program's own OSC 52 read reply, up to its BEL, into `$DIR/<name>`.
fn osc52_read(name: &str) -> String {
    format!(
        "printf '\\033]52;c;?\\007'; : > {name}; bel=$(printf '\\007'); \
         while c=$(dd bs=1 count=1 2>/dev/null); do printf %s \"$c\" >> {name}; [ \"$c\" = \"$bel\" ] && break; done"
    )
}

#[gpui_kit::test]
fn clipboard_reads_ask_by_default_and_are_answered_either_way(cx: &mut TestAppContext) {
    // the second request's answer, if any, is shown by `cat -v` after `done`: the pane reads nothing past a request
    // until it is answered
    let script = format!(
        "stty -echo -icanon min 1; printf 'ready'; dd bs=1 count=1 >/dev/null 2>&1; {}; \
         dd bs=1 count=1 >/dev/null 2>&1; printf '\\033]52;c;?\\007'; printf done; exec cat -v",
        osc52_read("allowed"),
    );
    let mut f = fixture(cx, &script, config(""));
    let dir = f.dir.path().to_owned();
    f.cx.write_to_clipboard(ClipboardItem::new_string("hi".into()));
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();

    f.pane().write(b"x").unwrap();
    f.settle(|f| f.cx.has_pending_prompt());
    let (heading, _) = super::prompt_text(ClipboardAccess::Read);
    assert_eq!(
        f.cx.pending_prompt().map(|(message, _)| message).as_deref(),
        Some(heading)
    );
    f.cx.simulate_prompt_answer("Allow");
    f.settle(|_| read("allowed").ends_with('\u{7}'));
    assert!(read("allowed").contains("aGk="), "{:?}", read("allowed"));

    f.pane().write(b"x").unwrap();
    f.settle(|f| f.cx.has_pending_prompt());
    f.cx.simulate_prompt_answer("Deny");
    f.wait_painted("done");
    std::thread::sleep(Duration::from_millis(200));
    f.cx.run_until_parked();
    let painted = f.painted();
    assert!(!painted.contains("aGk="), "{painted}");
}

#[gpui_kit::test]
fn releasing_a_lone_modifier_puts_out_the_link_it_lit(cx: &mut TestAppContext) {
    let script = "printf 'ready\\r\\nsee https://example.com/p'; exec sleep 30";
    let mut f = fixture(cx, script, config(""));
    f.wait_painted("example.com/p");
    f.focus();
    f.cx.simulate_mouse_move(Fixture::at(8, 1), None, Modifiers::default());
    f.cx.simulate_modifiers_change(Modifiers::control());
    assert!(
        f.view.read_with(f.cx, |view, _| view.hovered_link().is_some()),
        "Ctrl lit it"
    );
    // GPUI hands the lone Ctrl's release out as a `control` keystroke: it must reach the modifier listeners
    f.cx.simulate_modifiers_change(Modifiers::default());
    assert!(
        f.view.read_with(f.cx, |view, _| view.hovered_link().is_none()),
        "released"
    );
    f.click(8, 1, Modifiers::default());
    f.cx.run_until_parked();
    assert!(
        !f.events
            .borrow()
            .iter()
            .any(|event| matches!(event, TerminalEvent::OpenUrl(_))),
        "a plain click opens nothing"
    );
}

#[gpui_kit::test]
fn links_open_with_ctrl_shift_while_the_program_has_the_mouse(cx: &mut TestAppContext) {
    let script = "stty -echo -icanon; printf '\\033[?1000hready\\r\\nsee https://example.com/p\\r\\n'; exec cat -v";
    let mut f = fixture(cx, script, config(""));
    let ctrl_shift = Modifiers {
        control: true,
        shift: true,
        ..Modifiers::default()
    };
    f.cx.simulate_mouse_move(Fixture::at(8, 1), None, ctrl_shift);
    let hovered = f
        .view
        .read_with(f.cx, |view, _| view.hovered_link().map(|link| link.url.clone()));
    assert_eq!(hovered.as_deref(), Some("https://example.com/p"));
    f.click(8, 1, ctrl_shift);
    f.cx.run_until_parked();
    assert!(
        f.events
            .borrow()
            .contains(&TerminalEvent::OpenUrl("https://example.com/p".into()))
    );
    assert!(!f.painted().contains("^[[M"), "Shift kept the click from the program");
}

#[gpui_kit::test]
fn right_click_follows_right_click_action(cx: &mut TestAppContext) {
    let script = "stty -echo -icanon; printf 'ready\\r\\nhello world\\r\\n'; exec cat";
    // paste (agterm's default setting writes it): the selection goes, the clipboard is pasted
    let mut f = fixture(cx, script, config("right-click-action = paste\n"));
    f.cx.write_to_clipboard(ClipboardItem::new_string("from-clipboard".into()));
    f.click(1, 1, Modifiers::default());
    f.click(1, 1, Modifiers::default());
    assert_eq!(f.selection().as_deref(), Some("hello"));
    f.cx.simulate_mouse_down(Fixture::at(1, 1), MouseButton::Right, Modifiers::default());
    f.cx.simulate_mouse_up(Fixture::at(1, 1), MouseButton::Right, Modifiers::default());
    f.wait_painted("from-clipboard");
    assert_eq!(f.selection(), None);
}

#[gpui_kit::test]
fn right_click_copy_and_ignore(cx: &mut TestAppContext) {
    let script = "printf 'ready\\r\\nhello world\\r\\n'; exec sleep 30";
    let mut f = fixture(cx, script, config("right-click-action = copy\n"));
    f.click(7, 1, Modifiers::default());
    f.click(7, 1, Modifiers::default());
    f.cx.simulate_mouse_down(Fixture::at(1, 1), MouseButton::Right, Modifiers::default());
    f.cx.simulate_mouse_up(Fixture::at(1, 1), MouseButton::Right, Modifiers::default());
    f.cx.run_until_parked();
    let copied = f.cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("world"));
    assert_eq!(f.selection(), None, "copied and dropped");
}

#[gpui_kit::test]
fn right_click_ignore_and_context_menu(cx: &mut TestAppContext) {
    let script = "printf 'ready\\r\\nhello world\\r\\n'; exec sleep 30";
    let f = fixture(cx, script, config("right-click-action = ignore\n"));
    f.cx.update(|_, cx| cx.write_to_primary(ClipboardItem::new_string("untouched".into())));
    f.cx.simulate_mouse_down(Fixture::at(1, 1), MouseButton::Right, Modifiers::default());
    f.cx.simulate_mouse_up(Fixture::at(1, 1), MouseButton::Right, Modifiers::default());
    assert_eq!(f.selection(), None);
    // context-menu (Ghostty's default): the word, copied per copy-on-select (the primary selection by default)
    f.terminal.update(f.cx, |terminal, cx| {
        terminal.set_config(config("right-click-action = context-menu\n"), cx);
    });
    f.cx.simulate_mouse_down(Fixture::at(8, 1), MouseButton::Right, Modifiers::default());
    f.cx.simulate_mouse_up(Fixture::at(8, 1), MouseButton::Right, Modifiers::default());
    f.cx.run_until_parked();
    assert_eq!(f.selection().as_deref(), Some("world"));
    let primary = f.cx.update(|_, cx| cx.read_from_primary().and_then(|item| item.text()));
    assert_eq!(primary.as_deref(), Some("world"));
}

#[gpui_kit::test]
fn middle_click_follows_its_action_and_copy_on_select(cx: &mut TestAppContext) {
    let script = "stty -echo -icanon; printf 'ready\\r\\n'; exec cat";
    let mut f = fixture(cx, script, config("middle-click-action = ignore\n"));
    f.cx.update(|_, cx| cx.write_to_primary(ClipboardItem::new_string("primary-text".into())));
    f.cx.write_to_clipboard(ClipboardItem::new_string("clipboard-text".into()));
    let middle = |f: &mut Fixture<'_>| {
        f.cx.simulate_mouse_down(Fixture::at(1, 1), MouseButton::Middle, Modifiers::default());
        f.cx.simulate_mouse_up(Fixture::at(1, 1), MouseButton::Middle, Modifiers::default());
    };
    middle(&mut f);
    // under copy-on-select = clipboard, primary-paste reads the clipboard (Ghostty at GHOSTTY_REV)
    f.terminal.update(f.cx, |terminal, cx| {
        terminal.set_config(config("copy-on-select = clipboard\n"), cx);
    });
    middle(&mut f);
    f.wait_painted("clipboard-text");
    f.terminal
        .update(f.cx, |terminal, cx| terminal.set_config(config(""), cx));
    middle(&mut f);
    f.wait_painted("clipboard-textprimary-text");
    // the first click, under `ignore`, pasted nothing
    let painted = f.painted();
    assert_eq!(
        painted.lines().nth(1).map(str::trim_end),
        Some("clipboard-textprimary-text"),
        "{painted}"
    );
}

#[gpui_kit::test]
fn copy_on_select_false_leaves_the_primary_selection_alone(cx: &mut TestAppContext) {
    let script = "printf 'ready\\r\\nhello world\\r\\n'; exec sleep 30";
    let mut f = fixture(cx, script, config("copy-on-select = false\n"));
    f.cx.update(|_, cx| cx.write_to_primary(ClipboardItem::new_string("untouched".into())));
    f.click(1, 1, Modifiers::default());
    f.click(1, 1, Modifiers::default());
    f.cx.run_until_parked();
    assert_eq!(f.selection().as_deref(), Some("hello"));
    let primary = f.cx.update(|_, cx| cx.read_from_primary().and_then(|item| item.text()));
    assert_eq!(primary.as_deref(), Some("untouched"));
}

#[gpui_kit::test]
fn toggling_mouse_reporting_lasts_past_the_next_event(cx: &mut TestAppContext) {
    let script = "stty -echo -icanon; printf '\\033[?1000hready\\r\\nhello\\r\\n'; exec cat -v";
    let mut f = fixture(cx, script, config(""));
    let shared = f.view.read_with(f.cx, |view, _| Rc::clone(view.shared()));
    let mut surface = super::TerminalSurface::new(Rc::clone(&shared));
    assert!(agterm_host::Surface::binding_action(
        &mut surface,
        "toggle_mouse_reporting"
    ));
    f.click(1, 1, Modifiers::default());
    f.click(1, 1, Modifiers::default());
    assert_eq!(f.selection().as_deref(), Some("hello"), "the mouse selects");
    assert!(!f.painted().contains("^[[M"), "nothing reported");
}

#[gpui_kit::test]
fn the_keymaps_chords_release_never_reaches_a_kitty_program(cx: &mut TestAppContext) {
    // Kitty flags 3: disambiguate and report event types (releases too)
    let script = "stty -echo -icanon; printf '\\033[>3uready\\r\\n'; exec cat -v";
    let mut f = fixture(cx, script, config(""));
    let (keymap, _) = parse_keymap("command \"e\" ctrl+e true\n", &linux_defaults());
    keymap_dispatch::load_keymap(&keymap);
    let window = f.cx.update(|window, _| window.window_handle());
    keymap_dispatch::register_window(window, KeyOrigin::Window(uuid::Uuid::from_u128(2)));
    f.focus();
    f.type_keys("ctrl-e x");
    // x's press and its release (event type 3) arrive; nothing of Ctrl+E (code 101)
    f.wait_painted("120;1:3u");
    let painted = f.painted();
    assert!(!painted.contains("101"), "{painted}");
    keymap_dispatch::unregister_window(window);
}

#[gpui_kit::test]
fn an_all_binding_changes_every_terminals_key_tables(cx: &mut TestAppContext) {
    let binds = "keybind = vim/j=text:J\nkeybind = all:ctrl+t=activate_key_table:vim\n";
    let mut f = fixture(cx, "printf 'ready'; exec sleep 30", config(binds));
    // a second terminal with an owner view in the same app
    let dir = tempfile::tempdir().unwrap();
    let command = command("printf 'second'; exec sleep 30", dir.path());
    let spawn: super::Spawn = Box::new(move |config: PaneConfig| Pane::spawn(&command, &config));
    let spec = TerminalSpec {
        token: "second".into(),
        live: false,
        font_size: None,
    };
    let second =
        f.cx.update(|_, cx| cx.new(|cx| Terminal::new(spec, config(binds), spawn, cx)));
    let other =
        f.cx.update(|_, cx| cx.new(|cx| TerminalView::new(&second, ViewRole::Owner, cx)));
    f.focus();
    f.type_keys("ctrl-t");
    let tables = |view: &Entity<TerminalView>, f: &mut Fixture<'_>| {
        view.read_with(f.cx, |view, _| {
            view.keys
                .keyboard
                .active_tables()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
    };
    let own = f.view.clone();
    assert_eq!(tables(&own, &mut f), ["vim"]);
    assert_eq!(tables(&other, &mut f), ["vim"], "the other terminal's keyboard too");
}

/// A terminal with `config` running `script`, for a window of its own making.
fn new_terminal(
    cx: &mut TestAppContext,
    token: &str,
    script: &str,
    dir: &Path,
    config: TerminalConfig,
) -> Entity<Terminal> {
    let command = command(script, dir);
    let spawn: super::Spawn = Box::new(move |config: PaneConfig| Pane::spawn(&command, &config));
    let spec = TerminalSpec {
        token: token.into(),
        live: false,
        font_size: None,
    };
    cx.new(|cx| Terminal::new(spec, config, spawn, cx))
}

/// Runs the app until `view` paints `needle` (the pane's threads run meanwhile), or fails; what it painted.
fn wait_view(view: &Entity<TerminalView>, needle: &str, cx: &mut VisualTestContext) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        cx.run_until_parked();
        let text = view.read_with(cx, |view, _| {
            view.painted().map(|frame| frame.lines().join("\n")).unwrap_or_default()
        });
        if text.contains(needle) {
            return text;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {needle}:\n{text}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Two terminals' owner views side by side in one window.
struct Pair {
    left: Entity<TerminalView>,
    right: Entity<TerminalView>,
}

impl Render for Pair {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .child(div().w(px(300.)).h(px(200.)).child(self.left.clone()))
            .child(div().w(px(300.)).h(px(200.)).child(self.right.clone()))
    }
}

#[gpui_kit::test]
fn an_all_end_key_sequence_flushes_each_terminals_keys_before_its_chained_action(cx: &mut TestAppContext) {
    let binds = "keybind = a>b=ignore\nkeybind = all:ctrl+t=end_key_sequence\nkeybind = chain=text:X\n";
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let mut terminal = |token: &str| {
        let script = format!("stty -echo -icanon; printf '{token}\\r\\n'; exec cat");
        new_terminal(cx, token, &script, dir.path(), config(binds))
    };
    let (first, second) = (terminal("first"), terminal("second"));
    let (pair, cx) = cx.add_window_view(|_, cx| Pair {
        left: cx.new(|cx| TerminalView::new(&first, ViewRole::Owner, cx)),
        right: cx.new(|cx| TerminalView::new(&second, ViewRole::Owner, cx)),
    });
    cx.simulate_scale_factor_change(1.0);
    cx.simulate_resize(size(px(640.), px(480.)));
    let (left, right) = pair.read_with(cx, |pair, _| (pair.left.clone(), pair.right.clone()));
    wait_view(&left, "first", cx);
    wait_view(&right, "second", cx);
    let press = |view: &Entity<TerminalView>, key: &str, cx: &mut VisualTestContext| {
        let view = view.clone();
        let keystroke = keystroke(key);
        cx.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
            window.dispatch_keystroke(keystroke.clone(), cx);
        });
        cx.simulate_event(gpui_kit::KeyUpEvent { keystroke });
        cx.run_until_parked();
    };
    // `a` starts a sequence in the right terminal and waits there
    press(&right, "a", cx);
    // Ctrl+T in the left one ends every sequence, then types X everywhere: the held `a` comes first, as in Ghostty
    press(&left, "ctrl-t", cx);
    wait_view(&left, "X", cx);
    let text = wait_view(&right, "X", cx);
    assert!(text.contains("aX"), "{text}");
}

/// A terminal with an opaque panel drawn over its top left corner, as an overlay covers it.
struct Covered {
    view: Entity<TerminalView>,
}

impl Render for Covered {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .relative()
            .child(self.view.clone())
            .child(div().absolute().top_0().left_0().w(px(200.)).h(px(100.)).occlude())
    }
}

#[gpui_kit::test]
fn focus_follows_mouse_asks_nothing_through_an_overlay(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let terminal = new_terminal(
        cx,
        "covered",
        "printf 'ready'; exec sleep 30",
        dir.path(),
        config("focus-follows-mouse = true\n"),
    );
    let events: Events = Rc::default();
    let seen = Rc::clone(&events);
    cx.update(|cx| {
        cx.subscribe(&terminal, move |_, event: &TerminalEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let (covered, cx) = cx.add_window_view(|_, cx| Covered {
        view: cx.new(|cx| TerminalView::new(&terminal, ViewRole::Owner, cx)),
    });
    cx.simulate_scale_factor_change(1.0);
    cx.simulate_resize(size(px(640.), px(480.)));
    let view = covered.read_with(cx, |covered, _| covered.view.clone());
    wait_view(&view, "ready", cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let asked = || {
        events
            .borrow()
            .iter()
            .filter(|event| **event == TerminalEvent::FocusFollowsMouse)
            .count()
    };
    // under the overlay, over the terminal's grid
    cx.simulate_mouse_move(point(px(50.), px(50.)), None, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(asked(), 0, "the overlay holds the pointer");
    cx.simulate_mouse_move(point(px(300.), px(300.)), None, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(asked(), 1, "the terminal itself asks");
}

#[gpui_kit::test]
fn adjacent_hyperlinks_stay_apart_and_a_short_repeat_interval_counts_no_double_click(cx: &mut TestAppContext) {
    let script = "printf 'ready\\r\\n\\033]8;;https://a.test\\033\\\\aaa\\033]8;;https://b.test\\033\\\\bbb\\033]8;;\\033\\\\ x y'; \
                  exec sleep 30";
    let mut f = fixture(cx, script, config("click-repeat-interval = 1\n"));
    f.wait_painted("aaabbb");
    f.cx.simulate_mouse_move(Fixture::at(4, 1), None, Modifiers::control());
    let link = f
        .view
        .read_with(f.cx, |view, _| view.hovered_link().cloned())
        .expect("a link");
    assert_eq!(
        (link.url.as_str(), link.cells.as_slice()),
        ("https://b.test", &[(1, 3, 5)][..])
    );
    // two clicks 50 ms apart are two single clicks under a 1 ms repeat interval
    f.click(7, 1, Modifiers::default());
    std::thread::sleep(Duration::from_millis(50));
    f.click(7, 1, Modifiers::default());
    assert_eq!(f.selection(), None, "no word selected");
}

#[test]
fn a_native_key_carries_the_lock_keys() {
    let native = NativeKey {
        keycode: 44,
        locks: agterm_core::ghostty_config::binds::Mods::CAPS_LOCK | agterm_core::ghostty_config::binds::Mods::NUM_LOCK,
        text: "J".into(),
        unshifted: Some('j'),
        ascii_layout: true,
        ..NativeKey::default()
    };
    let stroke = KeyStroke::new(&keystroke("j"), &Source::Native(native), false);
    let mods = agterm_core::ghostty_config::binds::Mods::CAPS_LOCK | agterm_core::ghostty_config::binds::Mods::NUM_LOCK;
    assert!(stroke.mods.contains(mods));
    let repeat = NativeKey {
        keycode: 44,
        repeat: true,
        ..NativeKey::default()
    };
    assert_eq!(
        KeyStroke::new(&keystroke("j"), &Source::Native(repeat), false).action,
        agterm_core::ghostty_config::binds::KeyAction::Repeat
    );
}

#[gpui_kit::test]
fn the_pointer_asks_for_focus_under_focus_follows_mouse(cx: &mut TestAppContext) {
    let script = "printf 'ready\\r\\none;two'; exec sleep 30";
    let mut f = fixture(
        cx,
        script,
        config("focus-follows-mouse = true\nselection-word-chars = \" \"\n"),
    );
    f.wait_painted("one;two");
    let asked = |f: &Fixture<'_>| {
        f.events
            .borrow()
            .iter()
            .filter(|event| **event == TerminalEvent::FocusFollowsMouse)
            .count()
    };
    // an inactive window asks for nothing
    f.cx.simulate_mouse_move(Fixture::at(2, 1), None, Modifiers::default());
    f.cx.run_until_parked();
    assert_eq!(asked(&f), 0);
    f.activate();
    // a button held down (a drag begun in another view) asks for nothing
    f.cx.simulate_mouse_move(Fixture::at(2, 1), Some(MouseButton::Left), Modifiers::default());
    f.cx.run_until_parked();
    assert_eq!(asked(&f), 0);
    f.cx.simulate_mouse_move(Fixture::at(3, 1), None, Modifiers::default());
    f.cx.simulate_mouse_move(Fixture::at(4, 1), None, Modifiers::default());
    f.cx.run_until_parked();
    assert_eq!(asked(&f), 1, "once per entry");
    // focused, it asks no more
    f.focus();
    f.cx.simulate_mouse_move(Fixture::at(5, 1), None, Modifiers::default());
    f.cx.run_until_parked();
    assert_eq!(asked(&f), 1);
    // selection-word-chars reaches the double click
    f.click(1, 1, Modifiers::default());
    f.click(1, 1, Modifiers::default());
    assert_eq!(f.selection().as_deref(), Some("one;two"));
}
