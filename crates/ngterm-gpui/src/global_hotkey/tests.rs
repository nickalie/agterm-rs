//! The global hotkey's key positions, its portal trigger and its X grabs.

use ngterm_core::keymap::{Chord, Modifiers, parse_keybind};

use super::*;

fn chord(text: &str) -> Chord {
    parse_keybind(text).expect("a chord").remove(0)
}

/// Upstream's `keyCode(forChordKey:)`: the key position a chord key names, on the US layout's positions, whatever
/// layout is active, so the hotkey survives a layout switch.
#[test]
fn a_chord_key_names_its_us_position() {
    assert_eq!(keycode("`"), Some(49), "the key left of 1");
    assert_eq!(keycode("t"), Some(28));
    assert_eq!(keycode("1"), Some(10));
    assert_eq!(keycode("\\"), Some(51));
    assert_eq!(keycode("/"), Some(61));
    assert_eq!(keycode("return"), Some(36));
    assert_eq!(keycode("delete"), Some(22), "the keymap's delete is Backspace");
    assert_eq!(keycode("space"), Some(65));
    assert_eq!(keycode("f5"), Some(71));
    assert_eq!(keycode("f13"), Some(191));
    assert_eq!(keycode("up"), Some(111));
    assert_eq!(keycode("é"), None, "no position types it");
    for key in "abcdefghijklmnopqrstuvwxyz0123456789=-][';\\,/.`".chars() {
        let key = key.to_string();
        assert!(keycode(&key).is_some(), "{key} has a position");
    }
}

/// The XDG shortcuts specification's trigger: modifiers in its spelling, the key as its keysym name.
#[test]
fn a_chord_reads_as_a_portal_trigger() {
    assert_eq!(portal_trigger(&chord("ctrl+`")).as_deref(), Some("CTRL+grave"));
    assert_eq!(portal_trigger(&chord("cmd+shift+t")).as_deref(), Some("SHIFT+LOGO+t"));
    assert_eq!(
        portal_trigger(&chord("ctrl+alt+return")).as_deref(),
        Some("CTRL+ALT+Return")
    );
    assert_eq!(portal_trigger(&chord("f12")).as_deref(), Some("F12"));
    assert_eq!(portal_trigger(&chord("opt+space")).as_deref(), Some("ALT+space"));
    assert_eq!(
        portal_trigger(&Chord::new(Modifiers::CONTROL, "é")),
        None,
        "no keysym name"
    );
}

/// The X grab: the chord's modifiers as X's, under every Caps Lock and Num Lock state.
#[test]
fn an_x_grab_covers_every_lock_state() {
    use x11rb::protocol::xproto::ModMask;

    let grabs = x11::grabs(49, Modifiers::CONTROL | Modifiers::COMMAND);
    let base = ModMask::CONTROL | ModMask::M4;
    assert_eq!(
        grabs,
        vec![
            (49, base),
            (49, base | ModMask::LOCK),
            (49, base | ModMask::M2),
            (49, base | ModMask::LOCK | ModMask::M2),
        ]
    );
    assert_eq!(
        x11::modifier_mask(Modifiers::OPTION | Modifiers::SHIFT),
        ModMask::M1 | ModMask::SHIFT
    );
}
