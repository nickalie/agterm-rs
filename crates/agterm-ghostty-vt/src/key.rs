//! Key and focus encoding (`key/encoder.h`, `key/event.h`, `focus.h`): a key event in, the bytes the running program
//! expects for it out, per the terminal's current modes (legacy, cursor/keypad application, modifyOtherKeys, the
//! Kitty keyboard protocol).

use bitflags::bitflags;

use crate::{Error, Handle, Terminal, ffi};

/// A physical key (`GhosttyKey`, W3C `KeyboardEvent.code` positions), layout-independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key(ffi::GhosttyKey);

impl Key {
    pub const UNIDENTIFIED: Self = Self(ffi::GHOSTTY_KEY_UNIDENTIFIED);
    pub const A: Self = Self(ffi::GHOSTTY_KEY_A);
    pub const C: Self = Self(ffi::GHOSTTY_KEY_C);
    pub const BACKSPACE: Self = Self(ffi::GHOSTTY_KEY_BACKSPACE);
    pub const ENTER: Self = Self(ffi::GHOSTTY_KEY_ENTER);
    pub const ESCAPE: Self = Self(ffi::GHOSTTY_KEY_ESCAPE);
    pub const TAB: Self = Self(ffi::GHOSTTY_KEY_TAB);
    pub const ARROW_UP: Self = Self(ffi::GHOSTTY_KEY_ARROW_UP);
    pub const ARROW_DOWN: Self = Self(ffi::GHOSTTY_KEY_ARROW_DOWN);
    pub const ARROW_LEFT: Self = Self(ffi::GHOSTTY_KEY_ARROW_LEFT);
    pub const ARROW_RIGHT: Self = Self(ffi::GHOSTTY_KEY_ARROW_RIGHT);
    pub const F1: Self = Self(ffi::GHOSTTY_KEY_F1);
    pub const F5: Self = Self(ffi::GHOSTTY_KEY_F5);
    pub const F12: Self = Self(ffi::GHOSTTY_KEY_F12);
    pub const SHIFT_LEFT: Self = Self(ffi::GHOSTTY_KEY_SHIFT_LEFT);

    /// How many keys the enum has: `GhosttyKey` runs from `UNIDENTIFIED` (0) to `PASTE` without gaps.
    pub const COUNT: usize = ffi::GHOSTTY_KEY_PASTE as usize + 1;

    /// The key at position `index` of `GhosttyKey` (Ghostty's `input.Key` order, which its config's key names
    /// follow too), or `None` past the last key.
    #[must_use]
    pub fn from_index(index: usize) -> Option<Self> {
        (index < Self::COUNT)
            .then(|| ffi::GhosttyKey::try_from(index).ok())
            .flatten()
            .map(Self)
    }

    /// The key's position in `GhosttyKey`.
    #[must_use]
    pub fn index(self) -> usize {
        usize::try_from(self.0).unwrap_or(0)
    }
}

bitflags! {
    /// Modifier keys (`GhosttyMods`): which are held, the lock states, and which side of a held one is pressed.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub struct KeyMods: u16 {
        const SHIFT = 1;
        const CTRL = 1 << 1;
        const ALT = 1 << 2;
        const SUPER = 1 << 3;
        const CAPS_LOCK = 1 << 4;
        const NUM_LOCK = 1 << 5;
        /// The right shift is the one held (meaningful with `SHIFT`).
        const SHIFT_RIGHT = 1 << 6;
        const CTRL_RIGHT = 1 << 7;
        const ALT_RIGHT = 1 << 8;
        const SUPER_RIGHT = 1 << 9;
    }
}

/// A key event's kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum KeyAction {
    #[default]
    Press,
    Repeat,
    Release,
}

impl KeyAction {
    fn raw(self) -> ffi::GhosttyKeyAction {
        match self {
            Self::Press => ffi::GHOSTTY_KEY_ACTION_PRESS,
            Self::Repeat => ffi::GHOSTTY_KEY_ACTION_REPEAT,
            Self::Release => ffi::GHOSTTY_KEY_ACTION_RELEASE,
        }
    }
}

/// A key event to encode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyEvent<'a> {
    pub action: KeyAction,
    pub key: Key,
    pub mods: KeyMods,
    /// The modifiers the layout used up producing `text` (shift for `A`), which the encoder then ignores.
    pub consumed_mods: KeyMods,
    /// Part of an input-method composition (a dead key, a preedit): such a key types nothing itself.
    pub composing: bool,
    /// What the key types under the current layout, before Ctrl/Meta transformations; empty for none. Control
    /// characters are dropped (the encoder derives them from the key and the modifiers).
    pub text: &'a str,
    /// The character the key types with no modifier, under the current layout.
    pub unshifted: Option<char>,
}

impl Default for Key {
    fn default() -> Self {
        Self::UNIDENTIFIED
    }
}

/// Text the encoder may be given: no C0 control characters or DEL (`event.h`: pass none and let the encoder use the
/// logical key), and no platform function-key codes (`U+F700`–`U+F8FF`).
fn encodable_text(text: &str) -> &str {
    let control = |c: char| c.is_ascii_control() || ('\u{f700}'..='\u{f8ff}').contains(&c);
    if text.chars().any(control) { "" } else { text }
}

/// A key encoder with its reusable event: owned handles, `Send` and not `Sync` (crate docs).
#[derive(Debug)]
pub struct KeyEncoder {
    // the event goes first: it never refers to the encoder, but nothing should outlive what it was made for
    event: Handle<ffi::GhosttyKeyEventImpl>,
    encoder: Handle<ffi::GhosttyKeyEncoderImpl>,
}

// SAFETY: both handles are plain library allocations with no thread affinity (crate docs); `&mut self` serializes
// every call on them.
unsafe impl Send for KeyEncoder {}

impl KeyEncoder {
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] when an allocation fails.
    pub fn new() -> Result<Self, Error> {
        let mut encoder: ffi::GhosttyKeyEncoder = std::ptr::null_mut();
        // SAFETY: a null allocator selects the default one; `encoder` is a valid out-pointer.
        Error::check(unsafe { ffi::ghostty_key_encoder_new(std::ptr::null(), &raw mut encoder) })?;
        let encoder = Handle::new(encoder, ffi::ghostty_key_encoder_free)?;
        let mut event: ffi::GhosttyKeyEvent = std::ptr::null_mut();
        // SAFETY: as above.
        Error::check(unsafe { ffi::ghostty_key_event_new(std::ptr::null(), &raw mut event) })?;
        let event = Handle::new(event, ffi::ghostty_key_event_free)?;
        Ok(Self { event, encoder })
    }

    /// The bytes `event` sends to the program under `terminal`'s current modes; empty when the key sends nothing (a
    /// lone modifier, a release outside the Kitty protocol's event reporting).
    ///
    /// # Errors
    ///
    /// The library's result when encoding fails.
    pub fn encode(&mut self, terminal: &mut Terminal, event: &KeyEvent<'_>) -> Result<Vec<u8>, Error> {
        // SAFETY: both handles are live and `&mut` borrows serialize the call with the terminal's other users; the
        // call reads the terminal's modes into the encoder.
        unsafe { ffi::ghostty_key_encoder_setopt_from_terminal(self.encoder.ptr(), terminal.as_raw()) };
        self.fill(event);
        let text = encodable_text(event.text);
        // SAFETY: the event is live; the text is borrowed for this call only and reset below, before it can dangle.
        unsafe { ffi::ghostty_key_event_set_utf8(self.event.ptr(), text.as_ptr().cast(), text.len()) };
        let encoded = encode_with(|buffer, len, written| {
            // SAFETY: both handles are live; `buffer` is valid for `len` bytes of writes (or null with zero) and
            // `written` is a valid out-pointer.
            unsafe { ffi::ghostty_key_encoder_encode(self.encoder.ptr(), self.event.ptr(), buffer, len, written) }
        });
        // SAFETY: the event is live; null with zero length is "no text", so the event keeps no pointer to `text`.
        unsafe { ffi::ghostty_key_event_set_utf8(self.event.ptr(), std::ptr::null(), 0) };
        encoded
    }

    fn fill(&mut self, event: &KeyEvent<'_>) {
        let raw = self.event.ptr();
        // SAFETY: the event is live and owned by `self`.
        unsafe { ffi::ghostty_key_event_set_action(raw, event.action.raw()) };
        // SAFETY: as above; `Key` only holds values of the enum.
        unsafe { ffi::ghostty_key_event_set_key(raw, event.key.0) };
        // SAFETY: as above.
        unsafe { ffi::ghostty_key_event_set_mods(raw, event.mods.bits()) };
        // SAFETY: as above.
        unsafe { ffi::ghostty_key_event_set_consumed_mods(raw, event.consumed_mods.bits()) };
        // SAFETY: as above.
        unsafe { ffi::ghostty_key_event_set_composing(raw, event.composing) };
        // SAFETY: as above; 0 is "none".
        unsafe { ffi::ghostty_key_event_set_unshifted_codepoint(raw, event.unshifted.map_or(0, u32::from)) };
    }
}

/// Run an encoder call that writes into a caller buffer: a stack buffer first, then exactly the size it asked for.
pub(crate) fn encode_with(
    mut call: impl FnMut(*mut std::os::raw::c_char, usize, *mut usize) -> ffi::GhosttyResult,
) -> Result<Vec<u8>, Error> {
    let mut buffer = [0_u8; 128];
    let mut written = 0;
    match Error::check(call(buffer.as_mut_ptr().cast(), buffer.len(), &raw mut written)) {
        Ok(()) => return Ok(buffer[..written.min(buffer.len())].to_vec()),
        Err(Error::OutOfSpace) => {}
        Err(err) => return Err(err),
    }
    let mut buffer = vec![0_u8; written];
    let mut written = 0;
    Error::check(call(buffer.as_mut_ptr().cast(), buffer.len(), &raw mut written))?;
    buffer.truncate(written);
    Ok(buffer)
}

/// The focus report (`CSI I` gained, `CSI O` lost) a program in focus-reporting mode (1004) expects.
#[must_use]
pub fn encode_focus(gained: bool) -> Vec<u8> {
    let event = if gained {
        ffi::GHOSTTY_FOCUS_GAINED
    } else {
        ffi::GHOSTTY_FOCUS_LOST
    };
    encode_with(|buffer, len, written| {
        // SAFETY: `buffer` is valid for `len` bytes of writes and `written` is a valid out-pointer.
        unsafe { ffi::ghostty_focus_encode(event, buffer, len, written) }
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mode;

    fn press(key: Key, mods: KeyMods, text: &str) -> KeyEvent<'_> {
        KeyEvent {
            key,
            mods,
            text,
            ..KeyEvent::default()
        }
    }

    fn encode(terminal: &mut Terminal, event: &KeyEvent<'_>) -> Vec<u8> {
        KeyEncoder::new().unwrap().encode(terminal, event).unwrap()
    }

    #[test]
    fn keys_index_like_the_c_enum() {
        assert_eq!(Key::from_index(0), Some(Key::UNIDENTIFIED));
        assert_eq!(Key::from_index(Key::A.index()), Some(Key::A));
        assert_eq!(Key::from_index(Key::COUNT - 1).map(Key::index), Some(Key::COUNT - 1));
        assert_eq!(Key::from_index(Key::COUNT), None);
    }

    #[test]
    fn legacy_keys_follow_the_cursor_key_mode() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        assert_eq!(
            encode(&mut terminal, &press(Key::ARROW_UP, KeyMods::empty(), "")),
            b"\x1b[A"
        );
        terminal.vt_write(b"\x1b[?1h");
        assert_eq!(
            encode(&mut terminal, &press(Key::ARROW_UP, KeyMods::empty(), "")),
            b"\x1bOA"
        );
        assert_eq!(encode(&mut terminal, &press(Key::A, KeyMods::empty(), "a")), b"a");
        assert_eq!(encode(&mut terminal, &press(Key::C, KeyMods::CTRL, "c")), b"\x03");
        // Alt prefixes ESC; on macOS Alt is Option, which sends its text as is unless `macos-option-as-alt` says
        // otherwise (`GHOSTTY_KEY_ENCODER_OPT_MACOS_OPTION_AS_ALT`, which `setopt_from_terminal` resets to false)
        let alt: &[u8] = if cfg!(target_os = "macos") { b"a" } else { b"\x1ba" };
        assert_eq!(encode(&mut terminal, &press(Key::A, KeyMods::ALT, "a")), alt);
        // a control character in the text is the encoder's to derive, never passed through
        assert_eq!(encode(&mut terminal, &press(Key::ENTER, KeyMods::empty(), "\r")), b"\r");
        let release = KeyEvent {
            action: KeyAction::Release,
            ..press(Key::A, KeyMods::empty(), "a")
        };
        assert!(encode(&mut terminal, &release).is_empty());
    }

    #[test]
    fn the_kitty_protocol_follows_the_terminal() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        // push the disambiguate flag
        terminal.vt_write(b"\x1b[>1u");
        assert_eq!(
            encode(&mut terminal, &press(Key::ESCAPE, KeyMods::empty(), "")),
            b"\x1b[27u"
        );
        let ctrl_c = KeyEvent {
            unshifted: Some('c'),
            ..press(Key::C, KeyMods::CTRL, "c")
        };
        assert_eq!(encode(&mut terminal, &ctrl_c), b"\x1b[99;5u");
        assert!(!terminal.mode(Mode::dec(1)));
    }

    #[test]
    fn focus_reports() {
        assert_eq!(encode_focus(true), b"\x1b[I");
        assert_eq!(encode_focus(false), b"\x1b[O");
    }

    #[test]
    fn control_text_is_dropped() {
        assert_eq!(encodable_text("\x1b"), "");
        assert_eq!(encodable_text("\u{f700}"), "");
        assert_eq!(encodable_text("é"), "é");
    }
}
