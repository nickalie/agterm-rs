//! The keymap's key monitor and the terminal's keys (upstream's `CustomCommandRunner` local key monitor plus the
//! menu's key equivalents; port of `agterm-gtk/src/keymap_dispatch.rs`).
//!
//! One GPUI keystroke interceptor for the whole app ([`init`]) sees every key press before GPUI's own bindings and
//! element listeners, in agterm's resolution order (`agterm_core::ghostty_config::binds`, Technical Details >
//! Keyboard): first the keymap in a registered window ([`register_window`]) — custom commands, the `map` alternatives,
//! leaders timing out after 1.5 s, a `--repeat` tail live until 0.5 s after its release, then the built-in menu chords
//! (except `toggle_fullscreen`'s after a half-typed leader) — then, when a terminal's own view holds the keyboard,
//! ghostty's binds and the key encoder (`crate::terminal`). A key either takes stops there: GPUI's binding matcher and
//! pending-keystroke buffering never see it, and the platform's input handler does not type it again. A focused text
//! field (an element in an `Input` key context) or an open prompt keeps every key, as the GTK build's text fields and
//! dialogs did.
//!
//! A chord is the key's physical identity resolved per layout (`terminal::input::chord`). A consumed key's next press
//! reads as its autorepeat until its release, which reaches [`key_up`] from the window's (and the terminal's) key-up
//! listener; [`keyboard_left`] forgets them when no agterm window holds the keyboard any more. The leader and repeat
//! timeouts are deadlines checked at the next key, which is when they matter. Matched chords fire as main-loop tasks
//! (`AppState::keymap_fired`); a custom command carries the terminal typed in, read now (its selection included), as
//! the keyboard may move before the task runs.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use agterm_core::Core;
use agterm_core::keymap::{BuiltinAction, Chord, CustomCommand, CustomCommandEngine, EngineOutcome};
use gpui_kit::{AnyWindowHandle, App, Global, KeyUpEvent, KeystrokeEvent, Subscription, Window};
use uuid::Uuid;

use crate::state::{self, AppState, Poster};
use crate::terminal::input::{self, KeyStroke};

/// How long a half-typed leader waits for its next chord.
const LEADER_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long a `--repeat` sequence's prefix stays live after its tail is released (tmux's `repeat-time`).
const REPEAT_TIMEOUT: Duration = Duration::from_millis(500);

/// Where a key monitor sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOrigin {
    /// An agterm window.
    Window(Uuid),
    /// The quick terminal's panel.
    Quick,
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
    /// Keys whose press was consumed: their autorepeats and release stay consumed.
    consumed: HashSet<u32>,
    /// Consumed keys already released, until they are pressed again: a second key-up listener (the window's and the
    /// terminal's both report a release) still learns the release was the keymap's.
    released: HashSet<u32>,
    /// The fired `--repeat` tail still held down.
    held_repeat: Option<u32>,
    /// When the armed leader or the open repeat window lapses.
    deadline: Option<Instant>,
    poster: Option<Poster>,
    /// The windows carrying the monitor.
    windows: Vec<(AnyWindowHandle, KeyOrigin)>,
    /// What fired, in order, for the unit tests.
    #[cfg(test)]
    fired: Vec<Fired>,
}

thread_local! {
    static ROUTER: RefCell<Router> = RefCell::new(Router::default());
}

/// The app's one keystroke interceptor.
struct Interceptor(#[expect(dead_code, reason = "held for its drop, which removes the interceptor")] Subscription);

impl Global for Interceptor {}

/// Installs the key monitor on the app; once.
pub fn init(cx: &mut App) {
    if cx.has_global::<Interceptor>() {
        return;
    }
    let subscription = cx.intercept_keystrokes(intercept);
    cx.set_global(Interceptor(subscription));
}

/// Rebuilds the matcher and the menu chords when the keymap was loaded again (upstream's `rebuild` on
/// `.agtermKeymapChanged`).
pub fn sync(core: &Core, poster: &Poster) {
    ROUTER.with_borrow_mut(|router| {
        router.poster = Some(poster.clone());
        if router.generation == Some(core.keymap_generation()) {
            return;
        }
        router.generation = Some(core.keymap_generation());
        router.load(core.keymap());
    });
}

/// Puts window `handle` under the monitor as `origin`.
pub fn register_window(handle: AnyWindowHandle, origin: KeyOrigin) {
    ROUTER.with_borrow_mut(|router| {
        router.windows.retain(|(window, _)| *window != handle);
        router.windows.push((handle, origin));
    });
}

/// Takes window `handle` from the monitor.
pub fn unregister_window(handle: AnyWindowHandle) {
    ROUTER.with_borrow_mut(|router| router.windows.retain(|(window, _)| *window != handle));
}

/// The window of `origin`, while registered.
#[must_use]
pub fn window_of(origin: KeyOrigin) -> Option<AnyWindowHandle> {
    ROUTER.with_borrow(|router| {
        router
            .windows
            .iter()
            .find(|(_, registered)| *registered == origin)
            .map(|(handle, _)| *handle)
    })
}

/// A key went up in an agterm window: its consumed press and an open repeat window let go. Whether the keymap took
/// the press, so its release is the keymap's too (the terminal must not report it).
pub fn key_up(event: &KeyUpEvent, _window: &mut Window, _cx: &mut App) -> bool {
    let stroke = KeyStroke::new(&event.keystroke, &input::current_source(&event.keystroke), true);
    ROUTER.with_borrow_mut(|router| router.release(stroke.identity, Instant::now()))
}

/// No agterm window holds the keyboard: releases now land elsewhere, so the consumed keys are forgotten and a repeat
/// window closes; a half-typed leader keeps its own timeout.
pub fn keyboard_left() {
    ROUTER.with_borrow_mut(Router::keyboard_left);
}

/// Whether `active`, the window holding the keyboard, is one under the monitor.
#[must_use]
pub fn holds_keyboard(active: Option<AnyWindowHandle>) -> bool {
    active.is_some_and(|active| ROUTER.with_borrow(|router| router.windows.iter().any(|(window, _)| *window == active)))
}

/// What fired since the last call (tests).
#[cfg(test)]
pub(crate) fn take_fired() -> Vec<Fired> {
    ROUTER.with_borrow_mut(|router| std::mem::take(&mut router.fired))
}

/// Loads a keymap into the router directly, with no core and no poster (tests).
#[cfg(test)]
pub(crate) fn load_keymap(keymap: &agterm_core::keymap::Keymap) {
    ROUTER.with_borrow_mut(|router| {
        router.generation = None;
        router.load(keymap);
    });
}

/// The interceptor (module docs).
fn intercept(event: &KeystrokeEvent, window: &mut Window, cx: &mut App) {
    // GPUI's keystroke for a lone modifier pressed and released: the modifier change it stands for goes on to the
    // listeners (a link under the pointer goes dark), never to the keymap or the terminal
    if is_modifier_release(&event.keystroke) {
        return;
    }
    let source = input::current_source(&event.keystroke);
    let stroke = KeyStroke::new(&event.keystroke, &source, false);
    if keeps_keys(event) {
        ROUTER.with_borrow_mut(Router::reset_if_live);
        return;
    }
    let origin = ROUTER.with_borrow(|router| {
        router
            .windows
            .iter()
            .find(|(handle, _)| *handle == window.window_handle())
            .map(|(_, origin)| *origin)
    });
    if let Some(origin) = origin {
        let now = Instant::now();
        let fired = ROUTER.with_borrow_mut(|router| router.press(&stroke, now));
        match fired {
            Press::Passed => {}
            Press::Consumed => {
                cx.stop_propagation();
                return;
            }
            Press::Fired(fired) => {
                cx.stop_propagation();
                fire(origin, fired, window, cx);
                return;
            }
        }
    }
    if crate::terminal::key_down(&stroke, window, cx) {
        cx.stop_propagation();
    }
}

/// Whether `keystroke` is the one GPUI synthesizes when a lone modifier is released (`shift`, `control`, `alt`,
/// `platform` or `function`, with no modifier held and no text); a modifier key itself never comes as a key on Linux.
fn is_modifier_release(keystroke: &gpui_kit::Keystroke) -> bool {
    keystroke.key_char.is_none()
        && keystroke.modifiers == gpui_kit::Modifiers::default()
        && matches!(
            keystroke.key.as_str(),
            "shift" | "control" | "alt" | "platform" | "function"
        )
}

/// Whether the focused element keeps every key: a text field (an `Input` key context) or a prompt.
fn keeps_keys(event: &KeystrokeEvent) -> bool {
    event.context_stack.iter().any(|context| {
        ["Input", "Prompt", "PromptDialog"]
            .iter()
            .any(|name| context.contains(name))
    })
}

/// Posts what fired; a custom command carries the terminal typed in, read now.
fn fire(origin: KeyOrigin, fired: Fired, window: &Window, cx: &App) {
    #[cfg(test)]
    ROUTER.with_borrow_mut(|router| router.fired.push(fired.clone()));
    let Some(poster) = ROUTER.with_borrow(|router| router.poster.clone()) else {
        return;
    };
    let typing = match &fired {
        Fired::Command(_) => crate::terminal::typing_surface(window, cx),
        Fired::Builtin(_) => None,
    };
    state::post(&poster, move |state: &mut AppState| {
        state.keymap_fired(origin, fired, typing);
    });
}

/// What the router did with a press.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Press {
    Passed,
    Consumed,
    Fired(Fired),
}

impl Router {
    fn load(&mut self, keymap: &agterm_core::keymap::Keymap) {
        self.engine = Some(CustomCommandEngine::from_keymap(keymap));
        self.menu = BuiltinAction::all()
            .filter_map(|action| keymap.equivalent(action).map(|chord| (chord.clone(), action)))
            .collect();
        self.reset();
    }

    /// One press: whether it is consumed, and what it fired.
    fn press(&mut self, stroke: &KeyStroke, now: Instant) -> Press {
        self.released.remove(&stroke.identity);
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.lapse();
        }
        let chord = input::chord(stroke);
        let Some(engine) = self.engine.as_mut() else {
            return Press::Passed;
        };
        // an autorepeat of a consumed press: swallowed, unless it is the live tail of a repeatable sequence
        if self.consumed.contains(&stroke.identity) {
            let tail = chord.filter(|chord| engine.is_repeating() && engine.is_repeat_tail(chord));
            let fired = tail.and_then(|chord| match engine.advance(&chord) {
                EngineOutcome::Fired(command) => Some(Fired::Command(command)),
                EngineOutcome::FiredBuiltin(action) => Some(Fired::Builtin(action)),
                EngineOutcome::Armed | EngineOutcome::Unmatched => None,
            });
            return fired.map_or(Press::Consumed, Press::Fired);
        }
        if stroke.is_escape() {
            let armed = engine.is_armed();
            if armed || engine.is_repeating() {
                self.reset();
            }
            if !armed {
                return Press::Passed;
            }
            // the cancelling press is the keymap's: its autorepeats and its release stay off the terminal
            self.consumed.insert(stroke.identity);
            return Press::Consumed;
        }
        // a bare modifier is the next chord being typed: the leader keeps waiting
        let Some(chord) = chord else {
            return Press::Passed;
        };
        let outcome = self.advance(stroke.identity, &chord, now);
        if outcome != Press::Passed {
            self.consumed.insert(stroke.identity);
        }
        outcome
    }

    fn advance(&mut self, identity: u32, chord: &Chord, now: Instant) -> Press {
        let Some(engine) = self.engine.as_mut() else {
            return Press::Passed;
        };
        let was_armed = engine.is_armed();
        match engine.advance(chord) {
            EngineOutcome::Fired(command) => {
                self.hold_repeat_or_cancel(identity);
                Press::Fired(Fired::Command(command))
            }
            EngineOutcome::FiredBuiltin(action) => {
                self.hold_repeat_or_cancel(identity);
                Press::Fired(Fired::Builtin(action))
            }
            EngineOutcome::Armed => {
                self.deadline = Some(now + LEADER_TIMEOUT);
                Press::Consumed
            }
            EngineOutcome::Unmatched => {
                self.deadline = None;
                // the menu's key equivalents: consumed even when the item is disabled, as a menu consumes its own
                let Some(action) = self.menu.get(chord).copied() else {
                    return Press::Passed;
                };
                if was_armed && action == BuiltinAction::ToggleFullscreen {
                    return Press::Passed;
                }
                Press::Fired(Fired::Builtin(action))
            }
        }
    }

    /// A key went up; whether its press was consumed.
    fn release(&mut self, identity: u32, now: Instant) -> bool {
        if self.held_repeat == Some(identity) {
            self.held_repeat = None;
            if self.engine.as_ref().is_some_and(CustomCommandEngine::is_repeating) {
                self.deadline = Some(now + REPEAT_TIMEOUT);
            }
        }
        if self.consumed.remove(&identity) {
            self.released.insert(identity);
            return true;
        }
        self.released.contains(&identity)
    }

    fn hold_repeat_or_cancel(&mut self, identity: u32) {
        self.deadline = None;
        let repeating = self.engine.as_ref().is_some_and(CustomCommandEngine::is_repeating);
        self.held_repeat = repeating.then_some(identity);
    }

    fn keyboard_left(&mut self) {
        self.consumed.clear();
        self.released.clear();
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

    /// The leader or repeat window lapsed (the GTK build's timer).
    fn lapse(&mut self) {
        self.deadline = None;
        if let Some(engine) = self.engine.as_mut() {
            engine.reset();
        }
        self.held_repeat = None;
    }

    fn reset(&mut self) {
        if let Some(engine) = self.engine.as_mut() {
            engine.reset();
        }
        self.held_repeat = None;
        self.deadline = None;
    }
}

#[cfg(test)]
mod tests;
