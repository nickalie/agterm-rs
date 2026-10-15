//! GPUI key events as the terminal and the keymap read them (plan > Technical Details > Keyboard).
//!
//! A [`KeyStroke`] is one key event in toolkit-neutral terms: the physical key, the modifiers held and those the
//! layout used up, the text it types, the layout's unshifted character, and what the keymap's chord resolution needs.
//! On Linux GPUI's own `Keystroke` keeps neither the key's position nor the layout's level-0 character, and drops Shift
//! from symbols, so the platform's record of the native key (`gpui_linux::current_native_input`, a patch of ours)
//! is read during the dispatch and preferred: the XKB keycode gives the physical key through Ghostty's keycode table
//! (`keycodes.rs`), XKB gives the rest. Without one (GPUI's test platform, a replayed pending keystroke, another
//! platform) the key is read from GPUI's key name as a US keyboard would type it, Shift restored for its symbols;
//! text an input method committed as a key has no position at all.
//!
//! [`chord`] is the GTK build's `chord_of` (`agterm-gtk/src/keymap_dispatch.rs`): named keys by their key, other keys by
//! what the layout types on them with no modifier, or, on a layout that cannot type ASCII, by the Latin key at that
//! position (upstream's `chordKey`).

use agterm_core::ghostty_config::binds::{KeyAction, Mods, PhysicalKey};
use agterm_core::keymap::{Chord, Modifiers};
use agterm_term::KeyInput;
use gpui_kit::{Keystroke, Modifiers as GpuiModifiers};

use super::keycodes::XKB_CODES;

/// What the platform knew about a key event, as far as the terminal cares (the patched platform's `NativeKey`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeKey {
    /// The XKB keycode.
    pub keycode: u32,
    /// The modifiers held, Shift included.
    pub modifiers: GpuiModifiers,
    /// The modifiers the layout used up producing the key's symbol.
    pub consumed: GpuiModifiers,
    /// What the key types at level 0 of its layout.
    pub unshifted: Option<char>,
    /// Whether that layout types ASCII letters.
    pub ascii_layout: bool,
    /// What the key types before Ctrl transformations (empty while composing).
    pub text: String,
    pub composing: bool,
    pub repeat: bool,
    /// The lock keys on: [`Mods::CAPS_LOCK`] and [`Mods::NUM_LOCK`].
    pub locks: Mods,
}

/// Where a key event came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A keyboard key the platform described.
    Native(NativeKey),
    /// Text an input method committed, dispatched as a key.
    Text,
    /// Nothing but GPUI's keystroke.
    Keystroke,
}

/// The native input behind the key event being dispatched, when it describes `keystroke`.
#[cfg(target_os = "linux")]
#[must_use]
pub fn current_source(keystroke: &Keystroke) -> Source {
    match gpui_linux::current_native_input() {
        Some(gpui_linux::NativeInput::Key(native)) if native.keystroke == *keystroke => Source::Native(NativeKey {
            keycode: native.keycode,
            modifiers: native.modifiers,
            consumed: native.consumed,
            unshifted: native.unshifted,
            ascii_layout: native.ascii_layout,
            text: native.text,
            composing: native.composing,
            repeat: native.repeat,
            locks: {
                let mut locks = Mods::empty();
                locks.set(Mods::CAPS_LOCK, native.caps_lock);
                locks.set(Mods::NUM_LOCK, native.num_lock);
                locks
            },
        }),
        Some(gpui_linux::NativeInput::Text) => Source::Text,
        _ => Source::Keystroke,
    }
}

/// The native input behind the key event being dispatched (none off Linux).
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn current_source(_keystroke: &Keystroke) -> Source {
    Source::Keystroke
}

/// One key event, owned (module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStroke {
    pub action: KeyAction,
    pub key: PhysicalKey,
    pub mods: Mods,
    pub consumed: Mods,
    pub composing: bool,
    pub text: String,
    pub unshifted: Option<char>,
    /// The XKB keycode; 0 when unknown.
    pub keycode: u32,
    /// Whether the layout types ASCII letters (assumed without a native record).
    pub ascii_layout: bool,
    /// What identifies the physical key between its press and its release: the keycode, or GPUI's key name hashed.
    pub identity: u32,
}

impl KeyStroke {
    /// The event as `agterm-term`'s encoder takes it.
    #[must_use]
    pub fn input(&self) -> KeyInput<'_> {
        KeyInput {
            action: self.action,
            key: self.key,
            mods: self.mods,
            sides: agterm_term::ModSides::empty(),
            consumed: self.consumed,
            composing: self.composing,
            text: &self.text,
            unshifted: self.unshifted,
        }
    }

    /// The key is Escape.
    #[must_use]
    pub fn is_escape(&self) -> bool {
        self.key.name() == "escape"
    }

    /// GPUI's key event with what the platform said about it; `release` for a key-up.
    #[must_use]
    pub fn new(keystroke: &Keystroke, source: &Source, release: bool) -> Self {
        let mut stroke = match source {
            Source::Native(native) => from_native(keystroke, native),
            Source::Text => from_text(keystroke),
            Source::Keystroke => from_keystroke(keystroke),
        };
        if release {
            stroke.action = KeyAction::Release;
            stroke.composing = false;
        }
        stroke
    }
}

/// The ghostty modifiers of GPUI's (Super is GPUI's `platform` on Linux).
#[must_use]
pub fn mods(modifiers: &GpuiModifiers) -> Mods {
    let mut mods = Mods::empty();
    mods.set(Mods::SHIFT, modifiers.shift);
    mods.set(Mods::CTRL, modifiers.control);
    mods.set(Mods::ALT, modifiers.alt);
    mods.set(Mods::SUPER, modifiers.platform);
    mods
}

/// The physical key at an XKB keycode (Ghostty's keycode table); unidentified for a code it does not list.
#[must_use]
pub fn physical_key(keycode: u32) -> PhysicalKey {
    XKB_CODES
        .binary_search_by_key(&keycode, |(code, _)| *code)
        .ok()
        .and_then(|index| PhysicalKey::from_w3c(XKB_CODES[index].1))
        .unwrap_or(PhysicalKey::UNIDENTIFIED)
}

fn identity(name: &str) -> u32 {
    // FNV-1a, with the top bit set so it never collides with a keycode
    let hash = name.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    hash | 0x8000_0000
}

fn from_native(keystroke: &Keystroke, native: &NativeKey) -> KeyStroke {
    KeyStroke {
        action: if native.repeat {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        },
        key: physical_key(native.keycode),
        mods: mods(&native.modifiers) | (native.locks & (Mods::CAPS_LOCK | Mods::NUM_LOCK)),
        consumed: mods(&native.consumed),
        composing: native.composing,
        text: native.text.clone(),
        unshifted: native.unshifted,
        keycode: native.keycode,
        ascii_layout: native.ascii_layout,
        identity: if native.keycode == 0 {
            identity(&keystroke.key)
        } else {
            native.keycode
        },
    }
}

fn from_text(keystroke: &Keystroke) -> KeyStroke {
    KeyStroke {
        action: KeyAction::Press,
        key: PhysicalKey::UNIDENTIFIED,
        mods: Mods::empty(),
        consumed: Mods::empty(),
        composing: false,
        text: keystroke.key_char.clone().unwrap_or_else(|| keystroke.key.clone()),
        unshifted: None,
        keycode: 0,
        ascii_layout: true,
        identity: identity(&keystroke.key),
    }
}

fn from_keystroke(keystroke: &Keystroke) -> KeyStroke {
    let mut held = mods(&keystroke.modifiers);
    let (key, base) = us_key(&keystroke.key);
    let mut chars = keystroke.key.chars();
    let single = chars.next().filter(|_| chars.next().is_none());
    let shifted = single
        .zip(base)
        .is_some_and(|(ch, base)| ch != base && !base.is_alphabetic());
    if shifted {
        // GPUI drops Shift from a symbol it was needed for (`!` is shift+1)
        held |= Mods::SHIFT;
    }
    // what the key types before Ctrl transformations, as XKB's keysym gives it on the native path (GPUI keeps no
    // character for a Ctrl chord)
    let text = keystroke.key_char.clone().unwrap_or_else(|| {
        single
            .filter(char::is_ascii_graphic)
            .map(|ch| {
                if held.contains(Mods::SHIFT) {
                    ch.to_ascii_uppercase()
                } else {
                    ch
                }
            })
            .map(String::from)
            .unwrap_or_default()
    });
    let consumed = if held.contains(Mods::SHIFT) && !text.is_empty() {
        Mods::SHIFT
    } else {
        Mods::empty()
    };
    KeyStroke {
        action: KeyAction::Press,
        key,
        mods: held,
        consumed,
        composing: false,
        text,
        unshifted: base,
        keycode: 0,
        ascii_layout: true,
        identity: identity(&keystroke.key),
    }
}

/// The W3C code of a GPUI key name that is not a character.
fn named_code(name: &str) -> Option<&'static str> {
    Some(match name {
        "enter" => "Enter",
        "tab" => "Tab",
        "space" => "Space",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "escape" => "Escape",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        _ => return None,
    })
}

/// The US keyboard's key for a character: its W3C code and the character the key types unshifted.
fn us_char(ch: char) -> Option<(String, char)> {
    const PUNCTUATION: [(char, char, &str); 11] = [
        ('`', '~', "Backquote"),
        ('-', '_', "Minus"),
        ('=', '+', "Equal"),
        ('[', '{', "BracketLeft"),
        (']', '}', "BracketRight"),
        ('\\', '|', "Backslash"),
        (';', ':', "Semicolon"),
        ('\'', '"', "Quote"),
        (',', '<', "Comma"),
        ('.', '>', "Period"),
        ('/', '?', "Slash"),
    ];
    const DIGITS: &str = ")!@#$%^&*(";
    if ch.is_ascii_alphabetic() {
        let lower = ch.to_ascii_lowercase();
        return Some((format!("Key{}", lower.to_ascii_uppercase()), lower));
    }
    if let Some(digit) = ch.to_digit(10) {
        return Some((format!("Digit{digit}"), ch));
    }
    if let Some(index) = DIGITS.find(ch) {
        let digit = char::from_digit(u32::try_from(index).ok()?, 10)?;
        return Some((format!("Digit{digit}"), digit));
    }
    PUNCTUATION
        .iter()
        .find(|(base, shifted, _)| *base == ch || *shifted == ch)
        .map(|(base, _, code)| ((*code).to_owned(), *base))
}

/// The physical key of a GPUI key name on a US keyboard, and the character it types unshifted.
fn us_key(name: &str) -> (PhysicalKey, Option<char>) {
    if let Some(code) = named_code(name) {
        let base = (name == "space").then_some(' ');
        return (PhysicalKey::from_w3c(code).unwrap_or(PhysicalKey::UNIDENTIFIED), base);
    }
    if let Some(function) = name.strip_prefix('f').filter(|n| n.parse::<u8>().is_ok())
        && let Some(key) = PhysicalKey::from_w3c(&format!("F{function}"))
    {
        return (key, None);
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => match us_char(ch) {
            Some((code, base)) => (
                PhysicalKey::from_w3c(&code).unwrap_or(PhysicalKey::UNIDENTIFIED),
                Some(base),
            ),
            None => (PhysicalKey::UNIDENTIFIED, Some(ch)),
        },
        _ => (PhysicalKey::UNIDENTIFIED, None),
    }
}

/// The keymap modifiers of a key event: Control, Super (`cmd`), Alt (`opt`) and Shift.
#[must_use]
pub fn keymap_modifiers(mods: Mods) -> Modifiers {
    [
        (Mods::CTRL, Modifiers::CONTROL),
        (Mods::SUPER, Modifiers::COMMAND),
        (Mods::ALT, Modifiers::OPTION),
        (Mods::SHIFT, Modifiers::SHIFT),
    ]
    .into_iter()
    .filter(|(held, _)| mods.contains(*held))
    .fold(Modifiers::empty(), |acc, (_, bit)| acc | bit)
}

/// The keymap's name of a named key (`return`, `tab`, `space`, `delete` for Backspace, the arrows, `f1`–`f20`).
#[must_use]
pub fn named_key(key: PhysicalKey) -> Option<&'static str> {
    let name = match key.name() {
        "enter" | "numpad_enter" => "return",
        "tab" => "tab",
        "space" => "space",
        "backspace" => "delete",
        "arrow_left" => "left",
        "arrow_right" => "right",
        "arrow_up" => "up",
        "arrow_down" => "down",
        other => return agterm_core::keymap::FUNCTION_KEYS.into_iter().find(|key| *key == other),
    };
    Some(name)
}

/// The chord a key press spells for agterm's keymap; `None` for a bare modifier or a key with no usable base.
#[must_use]
pub fn chord(stroke: &KeyStroke) -> Option<Chord> {
    if stroke.key.is_modifier() {
        return None;
    }
    let key = named_key(stroke.key)
        .map(str::to_owned)
        .or_else(|| chord_key(stroke.keycode, stroke.unshifted, stroke.ascii_layout))?;
    Some(Chord::new(keymap_modifiers(stroke.mods), key))
}

/// The base key for a press (upstream's `chordKey`), decided per layout: a layout that can type ASCII binds by what
/// it types on the key (lowercased; a space is no base key); one that cannot binds by the Latin key at that position
/// ([`latin_key`]), else by what a key outside that table types, except the ISO section key, whose
/// layout-dependent character would alias a table key. (The GTK build's `chord_key`.)
#[must_use]
pub fn chord_key(keycode: u32, produced: Option<char>, ascii_layout: bool) -> Option<String> {
    let base = produced
        .filter(|ch| *ch != ' ')
        .map(|ch| ch.to_lowercase().collect::<String>());
    if ascii_layout {
        return base;
    }
    if let Some(latin) = latin_key(keycode) {
        return Some(latin.to_owned());
    }
    if keycode == KEYCODE_ISO_SECTION { None } else { base }
}

/// The XKB keycode of the ISO-only key left of Z (`LSGT`).
const KEYCODE_ISO_SECTION: u32 = 94;

/// The Latin character at an XKB keycode on a US (ANSI) keyboard, `None` elsewhere (upstream's `latinKey`).
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
