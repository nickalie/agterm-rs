//! The key monitor's router without a window (the GTK build's `keymap_dispatch` tests, by GPUI keystrokes): chords and
//! commands consumed, autorepeats of consumed keys, `--repeat` tails, leaders and their timeouts, the menu's chords.

use std::time::{Duration, Instant};

use gpui_kit::Keystroke;
use ngterm_core::keymap::{CustomCommandEngine, linux_defaults, parse_keymap};

use super::{Fired, Press, Router};
use crate::terminal::input::{KeyStroke, Source};

fn stroke(text: &str) -> KeyStroke {
    let keystroke = Keystroke::parse(text).unwrap().with_simulated_ime();
    KeyStroke::new(&keystroke, &Source::Keystroke, false)
}

fn router(keymap: &str) -> Router {
    let (keymap, _) = parse_keymap(keymap, &linux_defaults());
    let mut router = Router::default();
    router.load(&keymap);
    router
}

/// Presses `text` at `now`; whether it was consumed, recording what fired.
fn press(router: &mut Router, text: &str, now: Instant) -> bool {
    match router.press(&stroke(text), now) {
        Press::Passed => false,
        Press::Consumed => true,
        Press::Fired(fired) => {
            router.fired.push(fired);
            true
        }
    }
}

fn release(router: &mut Router, text: &str, now: Instant) -> bool {
    router.release(stroke(text).identity, now)
}

fn names(router: &Router) -> Vec<String> {
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
    let now = Instant::now();
    let mut router = router("command \"x\" ctrl+shift+b true\n");
    assert!(press(&mut router, "ctrl-shift-b", now));
    assert!(press(&mut router, "ctrl-shift-t", now));
    assert!(!press(&mut router, "a", now));
    assert_eq!(names(&router), ["x", "new_session"]);
    assert!(
        release(&mut router, "ctrl-shift-t", now),
        "the keymap owns the release of its press"
    );
    assert!(
        release(&mut router, "ctrl-shift-t", now),
        "told twice (the window's and the terminal's listener)"
    );
    assert!(!router.consumed.contains(&stroke("t").identity));
    assert!(
        !release(&mut router, "a", now),
        "a passed press's release is the terminal's"
    );
}

#[test]
fn an_autorepeat_of_a_leader_prefix_is_swallowed_without_rearming() {
    let now = Instant::now();
    let mut router = router("command \"g\" ctrl+a>g true\n");
    assert!(press(&mut router, "ctrl-a", now));
    assert!(press(&mut router, "ctrl-a", now));
    assert!(router.fired.is_empty());
    assert!(router.engine.as_ref().is_some_and(CustomCommandEngine::is_armed));
    assert!(press(&mut router, "g", now));
    assert_eq!(names(&router), ["g"]);
}

#[test]
fn an_autorepeat_of_a_held_repeat_tail_fires_again() {
    let now = Instant::now();
    let mut router = router("command \"n\" ctrl+a>n --repeat true\n");
    assert!(press(&mut router, "ctrl-a", now));
    release(&mut router, "ctrl-a", now);
    assert!(press(&mut router, "n", now));
    assert!(press(&mut router, "n", now));
    assert_eq!(names(&router), ["n", "n"]);
    // released, the window stays open for a fresh press of the tail, until it lapses
    release(&mut router, "n", now);
    assert!(press(&mut router, "n", now + Duration::from_millis(100)));
    assert_eq!(router.fired.len(), 3);
    release(&mut router, "n", now + Duration::from_millis(100));
    assert!(
        !press(&mut router, "n", now + Duration::from_secs(1)),
        "the repeat window lapsed"
    );
}

#[test]
fn a_leader_lapses_after_its_timeout() {
    let now = Instant::now();
    let mut router = router("command \"g\" ctrl+a>g true\n");
    assert!(press(&mut router, "ctrl-a", now));
    release(&mut router, "ctrl-a", now);
    assert!(
        !press(&mut router, "g", now + Duration::from_secs(2)),
        "the leader lapsed"
    );
    assert!(router.fired.is_empty());
}

#[test]
fn a_menu_chord_ending_a_leader_still_fires_but_full_screen_does_not() {
    let now = Instant::now();
    let mut router = router("command \"g\" ctrl+a>g true\n");
    assert!(press(&mut router, "ctrl-a", now));
    release(&mut router, "ctrl-a", now);
    assert!(press(&mut router, "ctrl-shift-t", now));
    assert_eq!(names(&router), ["new_session"]);
    assert!(press(&mut router, "ctrl-a", now));
    release(&mut router, "ctrl-a", now);
    assert!(!press(&mut router, "f11", now));
    assert!(press(&mut router, "f11", now));
    assert_eq!(names(&router), ["new_session", "toggle_fullscreen"]);
}

#[test]
fn leaving_ngterm_forgets_consumed_keys_and_closes_the_repeat_window() {
    let now = Instant::now();
    let mut router = router("command \"n\" ctrl+a>n --repeat true\n");
    assert!(press(&mut router, "ctrl-shift-t", now));
    assert!(press(&mut router, "ctrl-a", now));
    assert!(press(&mut router, "n", now));
    // neither release arrives: the keyboard went to another application
    router.keyboard_left();
    assert!(press(&mut router, "ctrl-shift-t", now));
    assert_eq!(names(&router), ["new_session", "n", "new_session"]);
    assert!(!press(&mut router, "n", now));
}

#[test]
fn escape_cancels_an_armed_leader_and_is_consumed_only_then() {
    let now = Instant::now();
    let mut router = router("command \"g\" ctrl+a>g true\n");
    assert!(!press(&mut router, "escape", now));
    assert!(press(&mut router, "ctrl-a", now));
    release(&mut router, "ctrl-a", now);
    assert!(press(&mut router, "escape", now));
    assert!(press(&mut router, "escape", now), "its autorepeat is the keymap's too");
    assert!(
        release(&mut router, "escape", now),
        "the keymap owns the release of the cancelling press"
    );
    assert!(!press(&mut router, "g", now));
    assert!(
        !press(&mut router, "escape", now),
        "a fresh Escape is the terminal's again"
    );
    assert!(router.fired.is_empty());
}

#[test]
fn a_shifted_symbol_spells_its_base_key() {
    // GPUI hands `ctrl-!` for ctrl+shift+1; the keymap's chord is ctrl+shift+1
    let now = Instant::now();
    let mut router = router("command \"one\" ctrl+shift+1 true\n");
    assert!(press(&mut router, "ctrl-!", now));
    assert_eq!(names(&router), ["one"]);
    assert_eq!(
        super::input::chord(&stroke("ctrl-!")).map(|chord| chord.key),
        Some("1".to_owned())
    );
}
