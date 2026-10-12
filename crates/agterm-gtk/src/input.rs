//! GDK input translated into libghostty's: modifiers, key text, repeats, buttons and scroll deltas.
//!
//! The mapping is ghostty's own GTK runtime's (`src/apprt/gtk/key.zig`, `class/surface.zig`) carried onto the
//! embedding API, with upstream agterm's semantics on top (`GhosttySurfaceView+Input.swift`): repeats are reported as
//! repeats, and the Edit chords are physical-key bindings in the defaults config.

use std::collections::HashSet;

use agterm_ghostty::input::{KeyAction, Mods, MouseButton};
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
}
