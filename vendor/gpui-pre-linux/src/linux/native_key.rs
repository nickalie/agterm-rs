//! The native key behind a key event (an agterm-rs patch): what XKB said about the key GPUI turned into the
//! [`gpui::Keystroke`] it dispatches, which keeps neither the key's position nor the layout's unshifted character, and
//! drops Shift from symbols. A terminal needs both for physical-key bindings and the Kitty keyboard protocol.
//!
//! The platform installs it for exactly the dispatch of one event ([`NativeScope`]): a press, its autorepeats (each
//! with the press's own record) and its release carry [`NativeInput::Key`]; text an input method committed as a
//! synthetic key carries [`NativeInput::Text`]; anything else (a test's simulated key, a replayed pending keystroke)
//! carries nothing. A listener reads it with [`current_native_input`] while handling the event, and should check that
//! the record's keystroke is the event's.

use std::cell::RefCell;

use gpui::{Keystroke, Modifiers};

/// What the platform knew about a key event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeKey {
    /// The XKB keycode (the evdev code plus 8).
    pub keycode: u32,
    /// The modifiers held, Shift included.
    pub modifiers: Modifiers,
    /// The modifiers XKB used up producing the key's symbol (Shift for `A`).
    pub consumed: Modifiers,
    /// The character the key types at level 0 of its layout (no modifier).
    pub unshifted: Option<char>,
    /// Whether that layout types ASCII letters, judged over the Latin key positions (keycodes 10 to 61).
    pub ascii_layout: bool,
    /// What the key types under the layout before Ctrl transformations; the composed text when a dead key or a
    /// compose sequence completes; empty while composing.
    pub text: String,
    /// The key is part of an unfinished composition.
    pub composing: bool,
    /// An autorepeat of a held key.
    pub repeat: bool,
    /// Caps Lock is on (effective state).
    pub caps_lock: bool,
    /// Num Lock is on (effective state).
    pub num_lock: bool,
    /// The keystroke GPUI dispatched for it.
    pub keystroke: Keystroke,
}

/// What is behind the key event being dispatched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeInput {
    /// A key of the keyboard.
    Key(NativeKey),
    /// Text an input method committed, dispatched as a key.
    Text,
}

thread_local! {
    static CURRENT: RefCell<Option<NativeInput>> = const { RefCell::new(None) };
}

/// The native input behind the key event being dispatched on this thread; `None` outside a platform key dispatch.
pub fn current_native_input() -> Option<NativeInput> {
    CURRENT.with_borrow(Clone::clone)
}

/// Installs a native input for one dispatch and restores the previous one when dropped.
pub(crate) struct NativeScope(Option<NativeInput>);

impl NativeScope {
    pub(crate) fn enter(input: Option<NativeInput>) -> Self {
        Self(CURRENT.replace(input))
    }
}

impl Drop for NativeScope {
    fn drop(&mut self) {
        CURRENT.set(self.0.take());
    }
}

#[cfg(any(feature = "wayland", feature = "x11"))]
mod xkb_key {
    use gpui::{Keystroke, Modifiers};
    use xkbcommon::xkb::{self, Keycode, State};

    use super::NativeKey;

    /// The record of `keycode` under `state`, for the `keystroke` GPUI built from it.
    pub(crate) fn native_key(
        state: &State,
        modifiers: Modifiers,
        keycode: Keycode,
        keystroke: &Keystroke,
        composing: bool,
    ) -> NativeKey {
        let keymap = state.get_keymap();
        let layout = state.key_get_layout(keycode);
        let level0 = |code: Keycode| {
            keymap
                .key_get_syms_by_level(code, layout, 0)
                .first()
                .and_then(|sym| char::from_u32(xkb::keysym_to_utf32(*sym)))
                .filter(|ch| !ch.is_control() && *ch != '\0')
        };
        let ascii_layout = (10..=61).any(|code| level0(Keycode::new(code)).is_some_and(|ch| ch.is_ascii_alphabetic()));
        let consumed_by = |name: &str| {
            let index = keymap.mod_get_index(name);
            index != xkb::MOD_INVALID && state.mod_index_is_consumed(keycode, index)
        };
        let consumed = Modifiers {
            shift: consumed_by(xkb::MOD_NAME_SHIFT),
            control: consumed_by(xkb::MOD_NAME_CTRL),
            alt: consumed_by(xkb::MOD_NAME_ALT),
            platform: consumed_by(xkb::MOD_NAME_LOGO),
            function: false,
        };
        let text = if composing {
            String::new()
        } else {
            keystroke.key_char.clone().unwrap_or_else(|| {
                let sym = state.key_get_one_sym(keycode);
                char::from_u32(xkb::keysym_to_utf32(sym))
                    .filter(|ch| !ch.is_control() && *ch != '\0')
                    .map(String::from)
                    .unwrap_or_default()
            })
        };
        let locked = |name: &str| state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
        NativeKey {
            caps_lock: locked(xkb::MOD_NAME_CAPS),
            num_lock: locked(xkb::MOD_NAME_NUM),
            keycode: keycode.raw(),
            modifiers,
            consumed,
            unshifted: level0(keycode),
            ascii_layout,
            text,
            composing,
            repeat: false,
            keystroke: keystroke.clone(),
        }
    }
}

#[cfg(any(feature = "wayland", feature = "x11"))]
pub(crate) use xkb_key::native_key;
