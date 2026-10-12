//! Input handed to a surface: modifiers, key events, mouse buttons and scroll flags, as `ghostty.h` spells them.

use agterm_ghostty_sys as sys;

bitflags::bitflags! {
    /// Modifier state (`ghostty_input_mods_e`). The `*_RIGHT` bits only say which side a held modifier is on; the
    /// plain bit is what makes it held.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Mods: u32 {
        const SHIFT = sys::GHOSTTY_MODS_SHIFT;
        const CTRL = sys::GHOSTTY_MODS_CTRL;
        const ALT = sys::GHOSTTY_MODS_ALT;
        const SUPER = sys::GHOSTTY_MODS_SUPER;
        const CAPS = sys::GHOSTTY_MODS_CAPS;
        const NUM = sys::GHOSTTY_MODS_NUM;
        const SHIFT_RIGHT = sys::GHOSTTY_MODS_SHIFT_RIGHT;
        const CTRL_RIGHT = sys::GHOSTTY_MODS_CTRL_RIGHT;
        const ALT_RIGHT = sys::GHOSTTY_MODS_ALT_RIGHT;
        const SUPER_RIGHT = sys::GHOSTTY_MODS_SUPER_RIGHT;
    }
}

/// What happened to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyAction {
    Release,
    Press,
    /// The key is held and the platform auto-repeated it.
    Repeat,
}

impl KeyAction {
    pub(crate) fn raw(self) -> sys::ghostty_input_action_e {
        match self {
            Self::Release => sys::GHOSTTY_ACTION_RELEASE,
            Self::Press => sys::GHOSTTY_ACTION_PRESS,
            Self::Repeat => sys::GHOSTTY_ACTION_REPEAT,
        }
    }
}

/// One key event (`ghostty_input_key_s`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub action: KeyAction,
    pub mods: Mods,
    /// Modifiers the platform already applied to produce `text` (Shift for `A`), so libghostty does not encode them
    /// a second time.
    pub consumed_mods: Mods,
    /// The platform's native code of the physical key: the XKB keycode on Linux, the virtual key code on macOS.
    pub keycode: u32,
    /// The text the key produced, if any. A string with a NUL byte is sent without text.
    pub text: Option<String>,
    /// The key's character with no modifier applied in the active layout, 0 when it has none.
    pub unshifted_codepoint: u32,
    /// An input method is composing: the key edits the composition, not the terminal.
    pub composing: bool,
}

/// A mouse button libghostty reports. Other buttons are not forwarded, as upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    pub(crate) fn raw(self) -> sys::ghostty_input_mouse_button_e {
        match self {
            Self::Left => sys::GHOSTTY_MOUSE_LEFT,
            Self::Right => sys::GHOSTTY_MOUSE_RIGHT,
            Self::Middle => sys::GHOSTTY_MOUSE_MIDDLE,
        }
    }
}

/// Whether a mouse button went down or up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseState {
    Release,
    Press,
}

impl MouseState {
    pub(crate) fn raw(self) -> sys::ghostty_input_mouse_state_e {
        match self {
            Self::Release => sys::GHOSTTY_MOUSE_RELEASE,
            Self::Press => sys::GHOSTTY_MOUSE_PRESS,
        }
    }
}

/// Scroll flags (`ghostty_input_scroll_mods_t`): bit 0 marks a precise (pixel) scroll from a touchpad rather than
/// wheel notches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ScrollMods {
    pub precision: bool,
}

impl ScrollMods {
    pub(crate) fn raw(self) -> sys::ghostty_input_scroll_mods_t {
        sys::ghostty_input_scroll_mods_t::from(self.precision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mods_carry_the_header_bits() {
        assert_eq!(Mods::SHIFT.bits(), 1);
        assert_eq!(Mods::CTRL.bits(), 2);
        assert_eq!(Mods::ALT.bits(), 4);
        assert_eq!(Mods::SUPER.bits(), 8);
        assert_eq!(Mods::CAPS.bits(), 16);
        assert_eq!(Mods::NUM.bits(), 32);
        assert_eq!((Mods::CTRL | Mods::CTRL_RIGHT).bits(), 2 | 128);
    }

    #[test]
    fn enums_map_onto_the_header_values() {
        assert_eq!(KeyAction::Release.raw(), sys::GHOSTTY_ACTION_RELEASE);
        assert_eq!(KeyAction::Repeat.raw(), sys::GHOSTTY_ACTION_REPEAT);
        assert_eq!(MouseButton::Middle.raw(), sys::GHOSTTY_MOUSE_MIDDLE);
        assert_eq!(MouseState::Press.raw(), sys::GHOSTTY_MOUSE_PRESS);
        assert_eq!(ScrollMods { precision: true }.raw(), 1);
        assert_eq!(ScrollMods::default().raw(), 0);
    }
}
