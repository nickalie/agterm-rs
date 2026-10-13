//! GDK input translated into libghostty's: modifiers, key text, repeats, buttons and scroll deltas.
//!
//! The mapping is ghostty's own GTK runtime's (`src/apprt/gtk/key.zig`, `class/surface.zig`) carried onto the
//! embedding API, with upstream agterm's semantics on top (`GhosttySurfaceView+Input.swift`): repeats are reported as
//! repeats, and the Edit chords are physical-key bindings in the defaults config.

use std::collections::HashSet;

use agterm_core::keystrokes::KeystrokeSegment;
use agterm_ghostty::input::{KeyAction, KeyEvent, Mods, MouseButton};
use gtk::gdk;

/// GDK modifier state as libghostty's modifiers. Lock is taken as Caps Lock, as ghostty does.
#[must_use]
pub fn translate_mods(state: gdk::ModifierType) -> Mods {
    let pairs = [
        (gdk::ModifierType::SHIFT_MASK, Mods::SHIFT),
        (gdk::ModifierType::CONTROL_MASK, Mods::CTRL),
        (gdk::ModifierType::ALT_MASK, Mods::ALT),
        (gdk::ModifierType::SUPER_MASK, Mods::SUPER),
        (gdk::ModifierType::LOCK_MASK, Mods::CAPS),
    ];
    pairs
        .into_iter()
        .filter(|(mask, _)| state.contains(*mask))
        .fold(Mods::empty(), |mods, (_, bit)| mods | bit)
}

/// The modifier a bare modifier key holds and whether it is the right-hand one. Super is libghostty's `super`
/// (ghostty maps `Super_L`/`Super_R` to its meta keys); Meta and Hyper carry no side.
#[must_use]
pub fn modifier_key(keyval: gdk::Key) -> Option<(Mods, bool)> {
    let held = match keyval {
        gdk::Key::Shift_L => (Mods::SHIFT, false),
        gdk::Key::Shift_R => (Mods::SHIFT, true),
        gdk::Key::Control_L => (Mods::CTRL, false),
        gdk::Key::Control_R => (Mods::CTRL, true),
        gdk::Key::Alt_L => (Mods::ALT, false),
        gdk::Key::Alt_R => (Mods::ALT, true),
        gdk::Key::Super_L => (Mods::SUPER, false),
        gdk::Key::Super_R => (Mods::SUPER, true),
        _ => return None,
    };
    Some(held)
}

/// The right-hand side bit of a held modifier.
fn right_bit(modifier: Mods) -> Mods {
    if modifier == Mods::SHIFT {
        Mods::SHIFT_RIGHT
    } else if modifier == Mods::CTRL {
        Mods::CTRL_RIGHT
    } else if modifier == Mods::ALT {
        Mods::ALT_RIGHT
    } else {
        Mods::SUPER_RIGHT
    }
}

/// The modifiers of a key event. GDK reports the state before the event, so a bare modifier key's own bit is set on
/// press and cleared on release here (ghostty's `eventMods`); its side comes from which key it is.
#[must_use]
pub fn event_mods(state: gdk::ModifierType, keyval: gdk::Key, action: KeyAction, num_lock: bool) -> Mods {
    let mut mods = translate_mods(state);
    mods.set(Mods::NUM, num_lock);
    if let Some((modifier, right)) = modifier_key(keyval) {
        mods.set(modifier, action != KeyAction::Release);
        mods.set(right_bit(modifier), right);
    }
    mods
}

/// The text a key produces when the input method committed none: its character, unless it is a control character
/// (the encoder handles those from the key). GTK leaves no text for `Ctrl+Shift+1` but keeps the keyval, so this is
/// also where such chords get theirs.
#[must_use]
pub fn keyval_text(keyval: gdk::Key) -> Option<String> {
    keyval.to_unicode().filter(|c| *c >= ' ').map(String::from)
}

/// The XKB keycode libghostty takes for each keyval ghostty's GTK runtime maps to a key (`key.zig`'s `keymap`, with
/// each key's Linux code from `input/keycodes.zig`). Keys with no Linux code there (F25, the keypad's navigation
/// keyvals) are left out: the embedding API cannot name them, so they keep their physical code.
#[rustfmt::skip]
const KEYVAL_KEYCODES: &[(gdk::Key, u32)] = &[
    (gdk::Key::a, 38), (gdk::Key::b, 56), (gdk::Key::c, 54), (gdk::Key::d, 40), (gdk::Key::e, 26), (gdk::Key::f, 41),
    (gdk::Key::g, 42), (gdk::Key::h, 43), (gdk::Key::i, 31), (gdk::Key::j, 44), (gdk::Key::k, 45), (gdk::Key::l, 46),
    (gdk::Key::m, 58), (gdk::Key::n, 57), (gdk::Key::o, 32), (gdk::Key::p, 33), (gdk::Key::q, 24), (gdk::Key::r, 27),
    (gdk::Key::s, 39), (gdk::Key::t, 28), (gdk::Key::u, 30), (gdk::Key::v, 55), (gdk::Key::w, 25), (gdk::Key::x, 53),
    (gdk::Key::y, 29), (gdk::Key::z, 52),
    (gdk::Key::_0, 19), (gdk::Key::_1, 10), (gdk::Key::_2, 11), (gdk::Key::_3, 12), (gdk::Key::_4, 13),
    (gdk::Key::_5, 14), (gdk::Key::_6, 15), (gdk::Key::_7, 16), (gdk::Key::_8, 17), (gdk::Key::_9, 18),
    (gdk::Key::semicolon, 47), (gdk::Key::space, 65), (gdk::Key::apostrophe, 48), (gdk::Key::comma, 59),
    (gdk::Key::grave, 49), (gdk::Key::period, 60), (gdk::Key::slash, 61), (gdk::Key::minus, 20), (gdk::Key::equal, 21),
    (gdk::Key::bracketleft, 34), (gdk::Key::bracketright, 35), (gdk::Key::backslash, 51),
    (gdk::Key::Up, 111), (gdk::Key::Down, 116), (gdk::Key::Right, 114), (gdk::Key::Left, 113), (gdk::Key::Home, 110),
    (gdk::Key::End, 115), (gdk::Key::Insert, 118), (gdk::Key::Delete, 119), (gdk::Key::Caps_Lock, 66),
    (gdk::Key::Scroll_Lock, 78), (gdk::Key::Num_Lock, 77), (gdk::Key::Page_Up, 112), (gdk::Key::Page_Down, 117),
    (gdk::Key::Escape, 9), (gdk::Key::Return, 36), (gdk::Key::Tab, 23), (gdk::Key::BackSpace, 22),
    (gdk::Key::Print, 107), (gdk::Key::Pause, 127),
    (gdk::Key::F1, 67), (gdk::Key::F2, 68), (gdk::Key::F3, 69), (gdk::Key::F4, 70), (gdk::Key::F5, 71),
    (gdk::Key::F6, 72), (gdk::Key::F7, 73), (gdk::Key::F8, 74), (gdk::Key::F9, 75), (gdk::Key::F10, 76),
    (gdk::Key::F11, 95), (gdk::Key::F12, 96), (gdk::Key::F13, 191), (gdk::Key::F14, 192), (gdk::Key::F15, 193),
    (gdk::Key::F16, 194), (gdk::Key::F17, 195), (gdk::Key::F18, 196), (gdk::Key::F19, 197), (gdk::Key::F20, 198),
    (gdk::Key::F21, 199), (gdk::Key::F22, 200), (gdk::Key::F23, 201), (gdk::Key::F24, 202),
    (gdk::Key::KP_0, 90), (gdk::Key::KP_1, 87), (gdk::Key::KP_2, 88), (gdk::Key::KP_3, 89), (gdk::Key::KP_4, 83),
    (gdk::Key::KP_5, 84), (gdk::Key::KP_6, 85), (gdk::Key::KP_7, 79), (gdk::Key::KP_8, 80), (gdk::Key::KP_9, 81),
    (gdk::Key::KP_Decimal, 91), (gdk::Key::KP_Divide, 106), (gdk::Key::KP_Multiply, 63), (gdk::Key::KP_Subtract, 82),
    (gdk::Key::KP_Add, 86), (gdk::Key::KP_Enter, 104), (gdk::Key::KP_Equal, 125),
    (gdk::Key::Copy, 141), (gdk::Key::Cut, 145), (gdk::Key::Paste, 143),
    (gdk::Key::Shift_L, 50), (gdk::Key::Control_L, 37), (gdk::Key::Alt_L, 64), (gdk::Key::Super_L, 133),
    (gdk::Key::Shift_R, 62), (gdk::Key::Control_R, 105), (gdk::Key::Alt_R, 108), (gdk::Key::Super_R, 134),
];

/// Whether an XKB keycode is a writing-system key (ghostty's `Key.shouldBeRemappable` is false for these): letters,
/// digits and punctuation, including the international backslash, ro and yen keys.
fn writing_system_key(keycode: u32) -> bool {
    matches!(keycode, 10..=21 | 24..=35 | 38..=49 | 51..=61 | 94 | 97 | 132)
}

/// A keyval with modifier semantics GDK may not flag as a modifier event (ghostty's `isModifierKeyval`).
fn modifier_keyval(keyval: gdk::Key) -> bool {
    use gdk::Key;
    [
        Key::Shift_L,
        Key::Shift_R,
        Key::Shift_Lock,
        Key::Control_L,
        Key::Control_R,
        Key::Caps_Lock,
        Key::Meta_L,
        Key::Meta_R,
        Key::Alt_L,
        Key::Alt_R,
        Key::Super_L,
        Key::Super_R,
        Key::Hyper_L,
        Key::Hyper_R,
        Key::Num_Lock,
        Key::Mode_switch,
        Key::ISO_Lock,
        Key::ISO_Level2_Latch,
        Key::ISO_Level3_Shift,
        Key::ISO_Level3_Latch,
        Key::ISO_Level3_Lock,
        Key::ISO_Level5_Shift,
        Key::ISO_Level5_Latch,
        Key::ISO_Level5_Lock,
        Key::ISO_Group_Latch,
        Key::ISO_Group_Lock,
    ]
    .contains(&keyval)
}

/// The keycode to give libghostty for a key event, after XKB remapping (ghostty's GTK `remapKey`): libghostty knows a
/// key only by its code, so a remap such as Caps Lock to Escape must arrive as the remapped key's code. Remaps
/// between two writing-system keys (another layout) keep the physical key, and a modifier with no key of its own
/// becomes 0, no key, rather than the unrelated key that produced it.
#[must_use]
pub fn remapped_keycode(keycode: u32, keyval: gdk::Key, is_modifier: bool) -> u32 {
    let remapped = KEYVAL_KEYCODES
        .iter()
        .find(|(key, _)| *key == keyval)
        .map(|(_, code)| *code);
    if let Some(remapped) = remapped
        && (!writing_system_key(keycode) || !writing_system_key(remapped))
    {
        return remapped;
    }
    if is_modifier || modifier_keyval(keyval) {
        return 0;
    }
    keycode
}

/// The XKB keycode of Return (`KEYVAL_KEYCODES`), the key a synthetic Return presses.
pub const RETURN_KEYCODE: u32 = 36;

/// A text run typed as one key press with no physical key (keycode 0): libghostty writes the text as typed, with no
/// key encoding. An input method's commit outside a key event and `session.type`'s runs go in this way.
#[must_use]
pub fn text_key(text: &str) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        mods: Mods::empty(),
        consumed_mods: Mods::empty(),
        keycode: 0,
        text: Some(text.to_owned()),
        unshifted_codepoint: 0,
        composing: false,
    }
}

/// A synthetic Return keypress, press then release, through the keyboard's own path so the program sees Enter
/// (upstream's `sendReturn`, with Linux's code for the key).
#[must_use]
pub fn return_keys() -> [KeyEvent; 2] {
    let press = KeyEvent {
        action: KeyAction::Press,
        mods: Mods::empty(),
        consumed_mods: Mods::empty(),
        keycode: RETURN_KEYCODE,
        text: None,
        unshifted_codepoint: 0,
        composing: false,
    };
    let release = KeyEvent {
        action: KeyAction::Release,
        ..press.clone()
    };
    [press, release]
}

/// The key events `session.type` sends for one segment of its payload.
#[must_use]
pub fn segment_keys(segment: &KeystrokeSegment) -> Vec<KeyEvent> {
    match segment {
        KeystrokeSegment::Text(run) => vec![text_key(run)],
        KeystrokeSegment::Return => return_keys().into(),
    }
}

/// Which keys are down, to tell a repeat from a press: GTK delivers both as `key-pressed`.
#[derive(Debug, Default)]
pub struct PressedKeys {
    down: HashSet<u32>,
}

impl PressedKeys {
    /// A press of `keycode`: a repeat when it is already down.
    pub fn press(&mut self, keycode: u32) -> KeyAction {
        if self.down.insert(keycode) {
            KeyAction::Press
        } else {
            KeyAction::Repeat
        }
    }

    pub fn release(&mut self, keycode: u32) {
        self.down.remove(&keycode);
    }

    /// Record a key event before anything may consume it, so the set follows the physical keys even when the input
    /// method swallows a release; the press or repeat it is, or the release.
    pub fn track(&mut self, action: KeyAction, keycode: u32) -> KeyAction {
        if action == KeyAction::Release {
            self.release(keycode);
            KeyAction::Release
        } else {
            self.press(keycode)
        }
    }

    /// Forget every key, when the surface loses focus and their releases go elsewhere.
    pub fn clear(&mut self) {
        self.down.clear();
    }
}

/// The libghostty button for a GDK button number; others (back, forward) are not forwarded, as upstream.
#[must_use]
pub fn mouse_button(button: u32) -> Option<MouseButton> {
    match button {
        gdk::BUTTON_PRIMARY => Some(MouseButton::Left),
        gdk::BUTTON_MIDDLE => Some(MouseButton::Middle),
        gdk::BUTTON_SECONDARY => Some(MouseButton::Right),
        _ => None,
    }
}

/// Whether any mouse button is held in `state` (a drag leaving the surface keeps its position).
#[must_use]
pub fn any_button_held(state: gdk::ModifierType) -> bool {
    state.intersects(
        gdk::ModifierType::BUTTON1_MASK
            | gdk::ModifierType::BUTTON2_MASK
            | gdk::ModifierType::BUTTON3_MASK
            | gdk::ModifierType::BUTTON4_MASK
            | gdk::ModifierType::BUTTON5_MASK,
    )
}

/// A GTK scroll delta as libghostty's: inverted (GTK's positive is down/right, libghostty's up/left), in device
/// pixels, and ten times larger for a touchpad's precise scroll (ghostty's GTK runtime).
#[must_use]
pub fn scroll_delta(dx: f64, dy: f64, scale: f64, precision: bool) -> (f64, f64) {
    let factor = -scale * if precision { 10.0 } else { 1.0 };
    (dx * factor, dy * factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_state_translates() {
        let state = gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::SUPER_MASK
            | gdk::ModifierType::LOCK_MASK
            | gdk::ModifierType::BUTTON1_MASK;
        assert_eq!(
            translate_mods(state),
            Mods::SHIFT | Mods::CTRL | Mods::ALT | Mods::SUPER | Mods::CAPS
        );
        assert_eq!(translate_mods(gdk::ModifierType::empty()), Mods::empty());
        assert_eq!(translate_mods(gdk::ModifierType::HYPER_MASK), Mods::empty());
    }

    #[test]
    fn a_bare_modifier_press_holds_its_own_bit_and_side() {
        let none = gdk::ModifierType::empty();
        assert_eq!(
            event_mods(none, gdk::Key::Control_L, KeyAction::Press, false),
            Mods::CTRL
        );
        assert_eq!(
            event_mods(none, gdk::Key::Control_R, KeyAction::Press, false),
            Mods::CTRL | Mods::CTRL_RIGHT
        );
        assert_eq!(
            event_mods(none, gdk::Key::Super_R, KeyAction::Repeat, false),
            Mods::SUPER | Mods::SUPER_RIGHT
        );
    }

    #[test]
    fn a_bare_modifier_release_drops_its_bit_but_keeps_its_side() {
        let held = gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::CONTROL_MASK;
        assert_eq!(
            event_mods(held, gdk::Key::Shift_R, KeyAction::Release, false),
            Mods::CTRL | Mods::SHIFT_RIGHT
        );
        assert_eq!(
            event_mods(held, gdk::Key::Shift_L, KeyAction::Release, false),
            Mods::CTRL
        );
    }

    #[test]
    fn ordinary_keys_keep_the_state_and_num_lock_shows() {
        let state = gdk::ModifierType::CONTROL_MASK;
        assert_eq!(event_mods(state, gdk::Key::c, KeyAction::Press, false), Mods::CTRL);
        assert_eq!(
            event_mods(state, gdk::Key::KP_1, KeyAction::Press, true),
            Mods::CTRL | Mods::NUM
        );
        assert_eq!(modifier_key(gdk::Key::Meta_L), None);
        assert_eq!(modifier_key(gdk::Key::Alt_R), Some((Mods::ALT, true)));
    }

    #[test]
    fn printable_keys_carry_text_and_control_keys_do_not() {
        assert_eq!(keyval_text(gdk::Key::a).as_deref(), Some("a"));
        assert_eq!(keyval_text(gdk::Key::exclam).as_deref(), Some("!"));
        assert_eq!(keyval_text(gdk::Key::Cyrillic_es).as_deref(), Some("с"));
        assert_eq!(keyval_text(gdk::Key::space).as_deref(), Some(" "));
        assert_eq!(keyval_text(gdk::Key::Return), None);
        assert_eq!(keyval_text(gdk::Key::Escape), None);
        assert_eq!(keyval_text(gdk::Key::Left), None);
        assert_eq!(keyval_text(gdk::Key::Control_L), None);
    }

    #[test]
    fn a_held_key_repeats_until_released() {
        let mut keys = PressedKeys::default();
        assert_eq!(keys.press(38), KeyAction::Press);
        assert_eq!(keys.press(38), KeyAction::Repeat);
        assert_eq!(keys.press(39), KeyAction::Press);
        keys.release(38);
        assert_eq!(keys.press(38), KeyAction::Press);
        keys.clear();
        assert_eq!(keys.press(39), KeyAction::Press);
    }

    #[test]
    fn a_release_the_input_method_swallows_still_lifts_the_key() {
        let mut keys = PressedKeys::default();
        assert_eq!(keys.track(KeyAction::Press, 50), KeyAction::Press);
        assert_eq!(keys.track(KeyAction::Press, 50), KeyAction::Repeat);
        // the release is tracked before the input method sees it, whatever it does with the event
        assert_eq!(keys.track(KeyAction::Release, 50), KeyAction::Release);
        assert_eq!(keys.track(KeyAction::Press, 50), KeyAction::Press);
    }

    #[test]
    fn special_key_remaps_reach_libghostty_as_the_remapped_key() {
        // caps:escape — the Caps Lock key (66) types Escape (9)
        assert_eq!(remapped_keycode(66, gdk::Key::Escape, false), 9);
        // caps:swapescape the other way round
        assert_eq!(remapped_keycode(9, gdk::Key::Caps_Lock, true), 66);
        // ctrl:nocaps — Caps Lock is Left Control
        assert_eq!(remapped_keycode(66, gdk::Key::Control_L, true), 37);
        // a writing-system key typing a special key is remapped too
        assert_eq!(remapped_keycode(38, gdk::Key::Escape, false), 9);
        // an unmapped special key keeps its code
        assert_eq!(remapped_keycode(9, gdk::Key::Escape, false), 9);
    }

    #[test]
    fn layouts_keep_the_physical_writing_system_key() {
        // Dvorak: the physical A key types `a` as well, and the physical S key types `o`
        assert_eq!(remapped_keycode(38, gdk::Key::a, false), 38);
        assert_eq!(remapped_keycode(39, gdk::Key::o, false), 39);
        // Russian: a keyval ghostty has no key for keeps the physical key
        assert_eq!(remapped_keycode(54, gdk::Key::Cyrillic_es, false), 54);
        // shifted keyvals are not in the map
        assert_eq!(remapped_keycode(38, gdk::Key::A, false), 38);
        // the keypad's navigation keyvals have no Linux code of their own
        assert_eq!(remapped_keycode(87, gdk::Key::KP_End, false), 87);
    }

    #[test]
    fn a_modifier_with_no_key_of_its_own_is_no_key() {
        assert_eq!(remapped_keycode(119, gdk::Key::Mode_switch, false), 0);
        assert_eq!(remapped_keycode(119, gdk::Key::VoidSymbol, true), 0);
        assert_eq!(remapped_keycode(108, gdk::Key::ISO_Level3_Shift, true), 0);
    }

    #[test]
    fn the_keycode_table_matches_writing_system_keys() {
        for (keyval, code) in KEYVAL_KEYCODES {
            if keyval.name().is_some_and(|name| name.starts_with("KP_")) {
                assert!(!writing_system_key(*code), "{keyval:?} -> {code}");
                continue;
            }
            let printable = keyval.to_unicode().is_some_and(|c| c.is_ascii_graphic());
            assert_eq!(writing_system_key(*code), printable, "{keyval:?} -> {code}");
        }
        let mut codes: Vec<u32> = KEYVAL_KEYCODES.iter().map(|(_, code)| *code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), KEYVAL_KEYCODES.len(), "each key has one code");
    }

    #[test]
    fn only_the_three_main_buttons_are_forwarded() {
        assert_eq!(mouse_button(1), Some(MouseButton::Left));
        assert_eq!(mouse_button(2), Some(MouseButton::Middle));
        assert_eq!(mouse_button(3), Some(MouseButton::Right));
        assert_eq!(mouse_button(8), None);
        assert_eq!(mouse_button(0), None);
    }

    #[test]
    fn held_buttons_are_detected() {
        assert!(any_button_held(
            gdk::ModifierType::BUTTON1_MASK | gdk::ModifierType::SHIFT_MASK
        ));
        assert!(!any_button_held(gdk::ModifierType::SHIFT_MASK));
    }

    #[test]
    fn scroll_deltas_invert_and_scale() {
        assert_eq!(scroll_delta(0.0, 1.0, 1.0, false), (-0.0, -1.0));
        assert_eq!(scroll_delta(-1.0, 0.0, 2.0, false), (2.0, -0.0));
        assert_eq!(scroll_delta(0.5, -2.0, 2.0, true), (-10.0, 40.0));
    }

    #[test]
    fn a_synthetic_return_is_the_return_key_pressed_and_released() {
        assert!(KEYVAL_KEYCODES.contains(&(gdk::Key::Return, RETURN_KEYCODE)));
        let [press, release] = return_keys();
        assert_eq!(press.action, KeyAction::Press);
        assert_eq!(release.action, KeyAction::Release);
        for key in [&press, &release] {
            assert_eq!(key.keycode, RETURN_KEYCODE);
            assert_eq!(key.text, None);
            assert_eq!(key.mods, Mods::empty());
            assert_eq!(key.consumed_mods, Mods::empty());
            assert_eq!(key.unshifted_codepoint, 0);
            assert!(!key.composing);
        }
    }

    #[test]
    fn typed_text_is_one_keyless_press_per_run() {
        let key = text_key("echo hi");
        assert_eq!(key.action, KeyAction::Press);
        assert_eq!(key.keycode, 0);
        assert_eq!(key.text.as_deref(), Some("echo hi"));
        assert_eq!(key.mods, Mods::empty());
        assert!(!key.composing);
    }

    #[test]
    fn segments_map_onto_their_key_events() {
        let keys: Vec<KeyEvent> = agterm_core::keystrokes::split("ls\npwd")
            .iter()
            .flat_map(segment_keys)
            .collect();
        let [press, release] = return_keys();
        assert_eq!(keys, [text_key("ls"), press, release, text_key("pwd")]);
    }
}
