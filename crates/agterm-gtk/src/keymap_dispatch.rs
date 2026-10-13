//! The keymap's key monitor (upstream's `CustomCommandRunner` local key monitor plus the menu's key equivalents): one
//! capture-phase key controller on every agterm window and on the quick terminal, so a chord is seen before the
//! focused terminal encodes it.
//!
//! A key press becomes a [`Chord`] (named keys by keyval; other keys by the character the layout puts on the key with
//! no modifier, or, on a layout that cannot type ASCII, by the Latin key at that physical position, as upstream's
//! `chordKey` resolves per layout). The custom-command matcher sees it first (custom commands and the `map`
//! alternatives the menu cannot carry, leaders timing out after 1.5 s, a `--repeat` tail live until 0.5 s after its
//! release); an unmatched chord then goes to the built-in menu chords, as the menu's key-equivalent pass takes it
//! upstream, except `toggle_fullscreen`'s after a half-typed leader (upstream's monitor matches that one itself).
//! Matched chords are consumed, as are their autorepeats and releases; the action itself runs as a main-loop task. A
//! focused text field (an inline rename, a picker's search) and a window's open dialog keep every key.
//!
//! GTK flags no autorepeat, so a consumed key's next press reads as its autorepeat until its release; a release that
//! lands outside agterm never arrives, so when no agterm window holds the keyboard any more the consumed keys are
//! forgotten and an open repeat window closes (upstream's fresh-press ownership and `closeRepeatWindow`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use adw::prelude::*;
use agterm_core::Core;
use agterm_core::keymap::{BuiltinAction, Chord, CustomCommand, CustomCommandEngine, EngineOutcome, Modifiers};
use gtk::{gdk, glib};
use uuid::Uuid;

use crate::state::{self, AppState, Poster};

/// How long a half-typed leader waits for its next chord.
const LEADER_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long a `--repeat` sequence's prefix stays live after its tail is released (tmux's `repeat-time`).
const REPEAT_TIMEOUT: Duration = Duration::from_millis(500);
/// How long after a window loses the keyboard the app is checked for another agterm window holding it: a switch
/// between agterm windows deactivates one before activating the other.
const LEAVE_CHECK_DELAY: Duration = Duration::from_millis(150);

/// Where a key controller sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOrigin {
    /// An agterm window.
    Window(Uuid),
    /// The quick terminal's panel.
    Quick,
}

/// Where a press landed: the controller's window and the widget holding that window's keyboard at key-down (upstream's
/// `keyWindow.firstResponder`).
struct Source {
    origin: KeyOrigin,
    focus: Option<gtk::Widget>,
}

/// What a chord fires, decided synchronously so the key is consumed or passed on at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fired {
    Command(CustomCommand),
    Builtin(BuiltinAction),
}

/// The matcher state shared by every window (one keyboard).
#[derive(Default)]
struct Router {
    engine: Option<CustomCommandEngine>,
    /// The resolved menu chord of each built-in.
    menu: HashMap<Chord, BuiltinAction>,
    generation: Option<u64>,
    /// Key codes whose press was consumed: their autorepeats and release stay consumed.
    consumed: HashSet<u32>,
    /// The fired `--repeat` tail still held down.
    held_repeat: Option<u32>,
    timer: Option<glib::SourceId>,
    poster: Option<Poster>,
    /// Whether the leader and repeat timeouts are armed on the main loop (off in unit tests, which run none).
    live_timers: bool,
    /// The windows carrying the monitor.
    windows: Vec<glib::WeakRef<gtk::Window>>,
    /// What fired, in order, for the unit tests (which post nothing).
    #[cfg(test)]
    fired: Vec<Fired>,
}

thread_local! {
    static ROUTER: RefCell<Router> = RefCell::new(Router::default());
}

/// Rebuilds the matcher and the menu chords when the keymap was loaded again (upstream's `rebuild` on
/// `.agtermKeymapChanged`).
pub fn sync(core: &Core, poster: &Poster) {
    ROUTER.with_borrow_mut(|router| {
        router.poster = Some(poster.clone());
        router.live_timers = true;
        if router.generation == Some(core.keymap_generation()) {
            return;
        }
        router.generation = Some(core.keymap_generation());
        let keymap = core.keymap();
        router.engine = Some(CustomCommandEngine::from_keymap(keymap));
        router.menu = BuiltinAction::all()
            .filter_map(|action| keymap.equivalent(action).map(|chord| (chord.clone(), action)))
            .collect();
        router.reset();
    });
}

/// Installs the monitor on `window`.
pub fn install(window: &impl IsA<gtk::Window>, origin: KeyOrigin) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = window.as_ref().downgrade();
    keys.connect_key_pressed(move |controller, keyval, keycode, state| {
        let Some(window) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if focus_is_text(&window) || presents_dialog(&window) {
            ROUTER.with_borrow_mut(Router::reset_if_live);
            return glib::Propagation::Proceed;
        }
        let chord = chord_of(controller, keyval, keycode, state);
        let source = Source {
            origin,
            focus: gtk::prelude::GtkWindowExt::focus(&window),
        };
        let consumed = ROUTER.with_borrow_mut(|router| router.press(&source, keyval, keycode, chord));
        if consumed {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    keys.connect_key_released(move |_, _, keycode, _| {
        ROUTER.with_borrow_mut(|router| router.release(keycode));
    });
    window.as_ref().add_controller(keys);
    ROUTER.with_borrow_mut(|router| {
        router.windows.retain(|window| window.upgrade().is_some());
        router.windows.push(window.as_ref().downgrade());
    });
    window.as_ref().connect_is_active_notify(|window| {
        if !window.is_active() {
            glib::timeout_add_local_once(LEAVE_CHECK_DELAY, || {
                ROUTER.with_borrow_mut(Router::check_keyboard_left);
            });
        }
    });
}

impl Router {
    /// One press; whether it is consumed.
    fn press(&mut self, source: &Source, keyval: gdk::Key, keycode: u32, chord: Option<Chord>) -> bool {
        let Some(engine) = self.engine.as_mut() else {
            return false;
        };
        // an autorepeat of a consumed press: swallowed, unless it is the live tail of a repeatable sequence
        if self.consumed.contains(&keycode) {
            let tail = chord.filter(|chord| engine.is_repeating() && engine.is_repeat_tail(chord));
            let fired = tail.and_then(|chord| match engine.advance(&chord) {
                EngineOutcome::Fired(command) => Some(Fired::Command(command)),
                EngineOutcome::FiredBuiltin(action) => Some(Fired::Builtin(action)),
                EngineOutcome::Armed | EngineOutcome::Unmatched => None,
            });
            if let Some(fired) = fired {
                self.fire(source, fired);
            }
            return true;
        }
        if keyval == gdk::Key::Escape {
            let armed = engine.is_armed();
            if armed || engine.is_repeating() {
                self.reset();
            }
            return armed;
        }
        // a bare modifier is the next chord being typed: the leader keeps waiting
        let Some(chord) = chord else { return false };
        let consumed = self.advance(source, keycode, &chord);
        if consumed {
            self.consumed.insert(keycode);
        }
        consumed
    }

    fn advance(&mut self, source: &Source, keycode: u32, chord: &Chord) -> bool {
        let Some(engine) = self.engine.as_mut() else {
            return false;
        };
        let was_armed = engine.is_armed();
        match engine.advance(chord) {
            EngineOutcome::Fired(command) => {
                self.hold_repeat_or_cancel(keycode);
                self.fire(source, Fired::Command(command));
                true
            }
            EngineOutcome::FiredBuiltin(action) => {
                self.hold_repeat_or_cancel(keycode);
                self.fire(source, Fired::Builtin(action));
                true
            }
            EngineOutcome::Armed => {
                self.arm_timer(LEADER_TIMEOUT);
                true
            }
            EngineOutcome::Unmatched => {
                self.cancel_timer();
                // the menu's key equivalents: consumed even when the item is disabled, as a menu consumes its own
                let Some(action) = self.menu.get(chord).copied() else {
                    return false;
                };
                if was_armed && action == BuiltinAction::ToggleFullscreen {
                    return false;
                }
                self.fire(source, Fired::Builtin(action));
                true
            }
        }
    }

    fn release(&mut self, keycode: u32) {
        if self.held_repeat == Some(keycode) {
            self.held_repeat = None;
            if self.engine.as_ref().is_some_and(CustomCommandEngine::is_repeating) {
                self.arm_timer(REPEAT_TIMEOUT);
            }
        }
        self.consumed.remove(&keycode);
    }

    fn hold_repeat_or_cancel(&mut self, keycode: u32) {
        self.cancel_timer();
        let repeating = self.engine.as_ref().is_some_and(CustomCommandEngine::is_repeating);
        self.held_repeat = repeating.then_some(keycode);
    }

    /// Posts what fired; a custom command carries the terminal typed in, read now (its selection included), as the
    /// keyboard may move before the task runs.
    fn fire(&mut self, source: &Source, fired: Fired) {
        #[cfg(test)]
        self.fired.push(fired.clone());
        let Some(poster) = self.poster.as_ref() else { return };
        let typing = match &fired {
            Fired::Command(_) => source.focus.as_ref().and_then(crate::surface::typing_surface),
            Fired::Builtin(_) => None,
        };
        let origin = source.origin;
        state::post(poster, move |state: &mut AppState| {
            state.keymap_fired(origin, fired, typing);
        });
    }

    fn check_keyboard_left(&mut self) {
        let held = self
            .windows
            .iter()
            .any(|window| window.upgrade().is_some_and(|window| window.is_active()));
        if !held {
            self.keyboard_left();
        }
    }

    /// No agterm window holds the keyboard: releases now land elsewhere, so the consumed keys are forgotten and a
    /// repeat window closes; a half-typed leader keeps its own timeout.
    fn keyboard_left(&mut self) {
        self.consumed.clear();
        self.held_repeat = None;
        if self.engine.as_ref().is_some_and(CustomCommandEngine::is_repeating) {
            self.reset();
        }
    }

    fn reset_if_live(&mut self) {
        if self
            .engine
            .as_ref()
            .is_some_and(|engine| engine.is_armed() || engine.is_repeating())
        {
            self.reset();
        }
    }

    fn reset(&mut self) {
        if let Some(engine) = self.engine.as_mut() {
            engine.reset();
        }
        self.held_repeat = None;
        self.cancel_timer();
    }

    fn arm_timer(&mut self, after: Duration) {
        self.cancel_timer();
        if !self.live_timers {
            return;
        }
        self.timer = Some(glib::timeout_add_local_once(after, || {
            ROUTER.with_borrow_mut(|router| {
                router.timer = None;
                if let Some(engine) = router.engine.as_mut() {
                    engine.reset();
                }
                router.held_repeat = None;
            });
        }));
    }

    fn cancel_timer(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
    }
}

/// The built-in whose menu chord a press spells, if any, for a text field that hands some menu chords on (a palette's
/// search field passes its launchers', so the same chord closes or switches the palette).
#[must_use]
pub fn menu_action(
    controller: &gtk::EventControllerKey,
    keyval: gdk::Key,
    keycode: u32,
    state: gdk::ModifierType,
) -> Option<BuiltinAction> {
    let chord = chord_of(controller, keyval, keycode, state)?;
    ROUTER.with_borrow(|router| router.menu.get(&chord).copied())
}

/// Whether `window`'s keyboard focus is a text field, which keeps its keystrokes.
fn focus_is_text(window: &gtk::Window) -> bool {
    gtk::prelude::GtkWindowExt::focus(window)
        .is_some_and(|widget| widget.is::<gtk::Text>() || widget.is::<gtk::TextView>() || widget.is::<gtk::Editable>())
}

/// Whether `window` presents an in-window dialog (a confirmation, a prompt), which owns the keyboard.
fn presents_dialog(window: &gtk::Window) -> bool {
    window
        .downcast_ref::<adw::ApplicationWindow>()
        .is_some_and(|window| window.visible_dialog().is_some())
}

/// The chord a press spells, `None` for a bare modifier or a key with no usable base.
fn chord_of(
    controller: &gtk::EventControllerKey,
    keyval: gdk::Key,
    keycode: u32,
    state: gdk::ModifierType,
) -> Option<Chord> {
    if is_modifier(keyval) {
        return None;
    }
    let key = named_key(keyval)
        .map(str::to_owned)
        .or_else(|| base_key(controller, keycode))?;
    Some(Chord::new(modifiers(state), key))
}

/// The keymap modifiers of a GDK state: Control, Super (`cmd`), Alt (`opt`) and Shift.
#[must_use]
pub fn modifiers(state: gdk::ModifierType) -> Modifiers {
    [
        (gdk::ModifierType::CONTROL_MASK, Modifiers::CONTROL),
        (gdk::ModifierType::SUPER_MASK, Modifiers::COMMAND),
        (gdk::ModifierType::ALT_MASK, Modifiers::OPTION),
        (gdk::ModifierType::SHIFT_MASK, Modifiers::SHIFT),
    ]
    .into_iter()
    .filter(|(mask, _)| state.contains(*mask))
    .fold(Modifiers::empty(), |mods, (_, bit)| mods | bit)
}

fn is_modifier(keyval: gdk::Key) -> bool {
    use gdk::Key;
    matches!(
        keyval,
        Key::Shift_L
            | Key::Shift_R
            | Key::Control_L
            | Key::Control_R
            | Key::Alt_L
            | Key::Alt_R
            | Key::Meta_L
            | Key::Meta_R
            | Key::Super_L
            | Key::Super_R
            | Key::Hyper_L
            | Key::Hyper_R
            | Key::Caps_Lock
            | Key::Shift_Lock
            | Key::ISO_Level3_Shift
            | Key::ISO_Level5_Shift
            | Key::Num_Lock
    )
}

/// The keymap's name of a named key (`return`, `tab`, `space`, `delete`, the arrows, `f1`–`f20`).
#[must_use]
pub fn named_key(keyval: gdk::Key) -> Option<&'static str> {
    use gdk::Key;
    let name = match keyval {
        Key::Return | Key::KP_Enter | Key::ISO_Enter => "return",
        Key::Tab | Key::ISO_Left_Tab | Key::KP_Tab => "tab",
        Key::space | Key::KP_Space => "space",
        Key::BackSpace => "delete",
        Key::Left | Key::KP_Left => "left",
        Key::Right | Key::KP_Right => "right",
        Key::Up | Key::KP_Up => "up",
        Key::Down | Key::KP_Down => "down",
        _ => return function_key(keyval),
    };
    Some(name)
}

fn function_key(keyval: gdk::Key) -> Option<&'static str> {
    let name = keyval.name()?.to_lowercase();
    agterm_core::keymap::FUNCTION_KEYS.into_iter().find(|key| *key == name)
}

/// The base key of a press: what the event's layout types on that key with no modifier when the layout can type
/// ASCII, else the Latin key at that physical position.
fn base_key(controller: &gtk::EventControllerKey, keycode: u32) -> Option<String> {
    let display = controller.widget()?.display();
    let group = controller
        .current_event()
        .and_then(|event| event.downcast::<gdk::KeyEvent>().ok())
        .map_or(0, |event| event.layout());
    let level0 = |code: u32| {
        display
            .map_keycode(code)?
            .into_iter()
            .find(|(key, _)| u32::try_from(key.group()).ok() == Some(group) && key.level() == 0)
            .and_then(|(_, keyval)| keyval.to_unicode())
    };
    // the layout types ASCII when the A key does
    let ascii_layout = level0(KEYCODE_A).is_some_and(|ch| ch.is_ascii_alphabetic());
    if ascii_layout {
        level0(keycode)
            .filter(|ch| !ch.is_whitespace())
            .map(|ch| ch.to_lowercase().collect())
    } else {
        latin_key(keycode).map(str::to_owned)
    }
}

/// The XKB keycode of the A key.
const KEYCODE_A: u32 = 38;

/// The Latin character at an XKB keycode on a US (ANSI) keyboard, `None` elsewhere (upstream's `latinKey`, by
/// Linux's key codes).
#[must_use]
pub fn latin_key(keycode: u32) -> Option<&'static str> {
    const ROWS: [(u32, &str); 4] = [
        (10, "1 2 3 4 5 6 7 8 9 0 - ="),
        (24, "q w e r t y u i o p [ ]"),
        (38, "a s d f g h j k l ; ' `"),
        (52, "z x c v b n m , . /"),
    ];
    if keycode == 51 {
        return Some("\\");
    }
    ROWS.iter().find_map(|(first, keys)| {
        let index = usize::try_from(keycode.checked_sub(*first)?).ok()?;
        keys.split(' ').nth(index)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_cover_the_grammar() {
        assert_eq!(named_key(gdk::Key::KP_Enter), Some("return"));
        assert_eq!(named_key(gdk::Key::ISO_Left_Tab), Some("tab"));
        assert_eq!(named_key(gdk::Key::BackSpace), Some("delete"));
        assert_eq!(named_key(gdk::Key::F11), Some("f11"));
        assert_eq!(named_key(gdk::Key::F20), Some("f20"));
        assert_eq!(named_key(gdk::Key::F21), None);
        assert_eq!(named_key(gdk::Key::a), None);
        for name in agterm_core::keymap::bindable_named_keys() {
            let found = [
                gdk::Key::Return,
                gdk::Key::Tab,
                gdk::Key::space,
                gdk::Key::BackSpace,
                gdk::Key::Left,
                gdk::Key::Right,
                gdk::Key::Up,
                gdk::Key::Down,
            ]
            .into_iter()
            .chain((1..=20).filter_map(|n| gdk::Key::from_name(format!("F{n}"))))
            .any(|key| named_key(key) == Some(name));
            assert!(found, "{name} has no key");
        }
    }

    #[test]
    fn latin_positions_follow_the_us_layout() {
        assert_eq!(latin_key(24), Some("q"));
        assert_eq!(latin_key(38), Some("a"));
        assert_eq!(latin_key(49), Some("`"));
        assert_eq!(latin_key(21), Some("="));
        assert_eq!(latin_key(61), Some("/"));
        assert_eq!(latin_key(51), Some("\\"));
        assert_eq!(latin_key(94), None);
        assert_eq!(latin_key(36), None);
    }

    #[test]
    fn super_is_cmd_and_alt_is_opt() {
        let state = gdk::ModifierType::SUPER_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::LOCK_MASK;
        assert_eq!(modifiers(state), Modifiers::COMMAND | Modifiers::OPTION);
    }

    fn press(router: &mut Router, keycode: u32, chord: &str) -> bool {
        let chord = agterm_core::keymap::parse_keybind(chord).unwrap().remove(0);
        let source = Source {
            origin: KeyOrigin::Quick,
            focus: None,
        };
        router.press(&source, gdk::Key::a, keycode, Some(chord))
    }

    fn router(keymap: &str) -> Router {
        let (keymap, _) = agterm_core::keymap::parse_keymap(keymap, &agterm_core::keymap::linux_defaults());
        Router {
            engine: Some(CustomCommandEngine::from_keymap(&keymap)),
            menu: BuiltinAction::all()
                .filter_map(|action| keymap.equivalent(action).map(|chord| (chord.clone(), action)))
                .collect(),
            ..Router::default()
        }
    }

    fn fired_names(router: &Router) -> Vec<String> {
        router
            .fired
            .iter()
            .map(|fired| match fired {
                Fired::Command(command) => command.name.clone(),
                Fired::Builtin(action) => action.name().to_owned(),
            })
            .collect()
    }

    #[test]
    fn menu_chords_and_commands_are_consumed_and_others_pass() {
        let mut router = router("command \"x\" ctrl+shift+b true\n");
        assert!(press(&mut router, 56, "ctrl+shift+b"));
        assert!(press(&mut router, 28, "ctrl+shift+t"));
        assert!(!press(&mut router, 38, "a"));
        assert_eq!(fired_names(&router), ["x", "new_session"]);
        router.release(28);
        assert!(!router.consumed.contains(&28));
    }

    #[test]
    fn an_autorepeat_of_a_leader_prefix_is_swallowed_without_rearming() {
        let mut router = router("command \"g\" ctrl+a>g true\n");
        assert!(press(&mut router, 38, "ctrl+a"));
        assert!(press(&mut router, 38, "ctrl+a"));
        assert!(router.fired.is_empty());
        assert!(router.engine.as_ref().is_some_and(CustomCommandEngine::is_armed));
        assert!(press(&mut router, 42, "g"));
        assert_eq!(fired_names(&router), ["g"]);
    }

    #[test]
    fn an_autorepeat_of_a_held_repeat_tail_fires_again() {
        let mut router = router("command \"n\" ctrl+a>n --repeat true\n");
        assert!(press(&mut router, 38, "ctrl+a"));
        router.release(38);
        assert!(press(&mut router, 57, "n"));
        assert!(press(&mut router, 57, "n"));
        assert_eq!(fired_names(&router), ["n", "n"]);
        // released, the window stays open for a fresh press of the tail
        router.release(57);
        assert!(press(&mut router, 57, "n"));
        assert_eq!(router.fired.len(), 3);
    }

    #[test]
    fn a_menu_chord_ending_a_leader_still_fires_but_full_screen_does_not() {
        let mut router = router("command \"g\" ctrl+a>g true\n");
        assert!(press(&mut router, 38, "ctrl+a"));
        router.release(38);
        assert!(press(&mut router, 28, "ctrl+shift+t"));
        assert_eq!(fired_names(&router), ["new_session"]);
        assert!(press(&mut router, 38, "ctrl+a"));
        router.release(38);
        assert!(!press(&mut router, 95, "f11"));
        assert!(press(&mut router, 95, "f11"));
        assert_eq!(fired_names(&router), ["new_session", "toggle_fullscreen"]);
    }

    #[test]
    fn leaving_agterm_forgets_consumed_keys_and_closes_the_repeat_window() {
        let mut router = router("command \"n\" ctrl+a>n --repeat true\n");
        assert!(press(&mut router, 28, "ctrl+shift+t"));
        assert!(press(&mut router, 38, "ctrl+a"));
        assert!(press(&mut router, 57, "n"));
        // neither release arrives: the keyboard went to another application
        router.keyboard_left();
        assert!(press(&mut router, 28, "ctrl+shift+t"));
        assert_eq!(fired_names(&router), ["new_session", "n", "new_session"]);
        assert!(!press(&mut router, 57, "n"));
    }

    #[test]
    fn a_leader_swallows_its_prefix_and_an_unmatched_tail_passes() {
        let mut router = router("command \"g\" ctrl+a>g true\n");
        assert!(press(&mut router, 38, "ctrl+a"));
        router.release(38);
        assert!(!press(&mut router, 39, "x"));
        assert!(press(&mut router, 38, "ctrl+a"));
        router.release(38);
        assert!(press(&mut router, 42, "g"));
    }
}
