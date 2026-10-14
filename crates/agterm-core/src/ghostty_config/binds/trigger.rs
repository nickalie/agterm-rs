//! Triggers, ported from `Binding.Trigger` (`src/input/Binding.zig`), the physical keys of `input/key.zig` and the
//! modifiers of `input/key_mods.zig` at `GHOSTTY_REV`.

use std::fmt;
use std::hash::{Hash, Hasher};

use bitflags::bitflags;

use super::BindError;

/// ghostty's physical keys (`input.Key`, W3C `KeyboardEvent.code` values in snake case), in its enum order. A trigger
/// names one of these to bind a key position whatever the layout puts on it (`key_c` is the key `c` sits on in a US
/// layout, `с` in a Russian one).
pub const PHYSICAL_KEYS: &[&str] = &[
    "unidentified",
    // writing system keys
    "backquote",
    "backslash",
    "bracket_left",
    "bracket_right",
    "comma",
    "digit_0",
    "digit_1",
    "digit_2",
    "digit_3",
    "digit_4",
    "digit_5",
    "digit_6",
    "digit_7",
    "digit_8",
    "digit_9",
    "equal",
    "intl_backslash",
    "intl_ro",
    "intl_yen",
    "key_a",
    "key_b",
    "key_c",
    "key_d",
    "key_e",
    "key_f",
    "key_g",
    "key_h",
    "key_i",
    "key_j",
    "key_k",
    "key_l",
    "key_m",
    "key_n",
    "key_o",
    "key_p",
    "key_q",
    "key_r",
    "key_s",
    "key_t",
    "key_u",
    "key_v",
    "key_w",
    "key_x",
    "key_y",
    "key_z",
    "minus",
    "period",
    "quote",
    "semicolon",
    "slash",
    // functional keys
    "alt_left",
    "alt_right",
    "backspace",
    "caps_lock",
    "context_menu",
    "control_left",
    "control_right",
    "enter",
    "meta_left",
    "meta_right",
    "shift_left",
    "shift_right",
    "space",
    "tab",
    "convert",
    "kana_mode",
    "non_convert",
    // control pad and arrows
    "delete",
    "end",
    "help",
    "home",
    "insert",
    "page_down",
    "page_up",
    "arrow_down",
    "arrow_left",
    "arrow_right",
    "arrow_up",
    // numpad
    "num_lock",
    "numpad_0",
    "numpad_1",
    "numpad_2",
    "numpad_3",
    "numpad_4",
    "numpad_5",
    "numpad_6",
    "numpad_7",
    "numpad_8",
    "numpad_9",
    "numpad_add",
    "numpad_backspace",
    "numpad_clear",
    "numpad_clear_entry",
    "numpad_comma",
    "numpad_decimal",
    "numpad_divide",
    "numpad_enter",
    "numpad_equal",
    "numpad_memory_add",
    "numpad_memory_clear",
    "numpad_memory_recall",
    "numpad_memory_store",
    "numpad_memory_subtract",
    "numpad_multiply",
    "numpad_paren_left",
    "numpad_paren_right",
    "numpad_subtract",
    "numpad_separator",
    "numpad_up",
    "numpad_down",
    "numpad_right",
    "numpad_left",
    "numpad_begin",
    "numpad_home",
    "numpad_end",
    "numpad_insert",
    "numpad_delete",
    "numpad_page_up",
    "numpad_page_down",
    // function keys
    "escape",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "f13",
    "f14",
    "f15",
    "f16",
    "f17",
    "f18",
    "f19",
    "f20",
    "f21",
    "f22",
    "f23",
    "f24",
    "f25",
    "fn",
    "fn_lock",
    "print_screen",
    "scroll_lock",
    "pause",
    // media keys
    "browser_back",
    "browser_favorites",
    "browser_forward",
    "browser_home",
    "browser_refresh",
    "browser_search",
    "browser_stop",
    "eject",
    "launch_app_1",
    "launch_app_2",
    "launch_mail",
    "media_play_pause",
    "media_select",
    "media_stop",
    "media_track_next",
    "media_track_previous",
    "power",
    "sleep",
    "audio_volume_down",
    "audio_volume_mute",
    "audio_volume_up",
    "wake_up",
    // legacy keys
    "copy",
    "cut",
    "paste",
];

/// A physical key: one of [`PHYSICAL_KEYS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalKey(&'static str);

impl PhysicalKey {
    /// A key the frontend could not identify; a trigger with no key carries it too.
    pub const UNIDENTIFIED: Self = Self("unidentified");

    /// The key named `name` exactly, `unidentified` included.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        PHYSICAL_KEYS.iter().find(|key| **key == name).map(|key| Self(key))
    }

    /// A key of [`PHYSICAL_KEYS`] by a name known to be one (the built-in binds').
    pub(super) const fn named(name: &'static str) -> Self {
        Self(name)
    }

    /// `Key.fromW3C`: a W3C code (`KeyA`, `ArrowUp`, `Digit1`) or a key name in any ASCII case.
    #[must_use]
    pub fn from_w3c(code: &str) -> Option<Self> {
        const BUFFER: usize = 128;
        if code.len() > BUFFER {
            return None;
        }
        if let Some(key) = Self::new(&code.to_ascii_lowercase()) {
            return Some(key);
        }
        let mut snake = String::with_capacity(BUFFER);
        for (index, byte) in code.bytes().enumerate() {
            match byte {
                b'a'..=b'z' => snake.push(char::from(byte)),
                b'A'..=b'Z' | b'0'..=b'9' => {
                    if index > 0 {
                        snake.push('_');
                    }
                    snake.push(char::from(byte.to_ascii_lowercase()));
                }
                _ => return None,
            }
        }
        (snake.len() <= BUFFER).then(|| Self::new(&snake)).flatten()
    }

    /// The key's name.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.0
    }

    /// Whether this is a modifier key (shift, control, alt or meta, either side).
    #[must_use]
    pub fn is_modifier(self) -> bool {
        matches!(
            self.0,
            "shift_left"
                | "shift_right"
                | "control_left"
                | "control_right"
                | "alt_left"
                | "alt_right"
                | "meta_left"
                | "meta_right"
        )
    }
}

bitflags! {
    /// Key modifiers (`key_mods.Mods`); a trigger and a binding match only the first four.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub struct Mods: u8 {
        const SHIFT = 1;
        const CTRL = 1 << 1;
        const ALT = 1 << 2;
        const SUPER = 1 << 3;
        const CAPS_LOCK = 1 << 4;
        const NUM_LOCK = 1 << 5;
    }
}

impl Mods {
    /// `Mods.binding`: the modifiers a binding looks at, without the locks.
    #[must_use]
    pub fn binding(self) -> Self {
        self & (Self::SHIFT | Self::CTRL | Self::ALT | Self::SUPER)
    }
}

/// The modifier names a trigger takes: `Mods`' own boolean fields, then `key_mods.alias`.
const MOD_NAMES: &[(&str, Mods)] = &[
    ("shift", Mods::SHIFT),
    ("ctrl", Mods::CTRL),
    ("alt", Mods::ALT),
    ("super", Mods::SUPER),
    ("caps_lock", Mods::CAPS_LOCK),
    ("num_lock", Mods::NUM_LOCK),
    ("cmd", Mods::SUPER),
    ("command", Mods::SUPER),
    ("opt", Mods::ALT),
    ("option", Mods::ALT),
    ("control", Mods::CTRL),
];

/// What a trigger's key matches.
#[derive(Debug, Clone, Copy)]
pub enum TriggerKey {
    /// A key position, whatever the layout.
    Physical(PhysicalKey),
    /// The character a key produces (or produces unshifted), compared case-folded.
    Unicode(char),
    /// Any key no other binding takes.
    CatchAll,
}

/// A key and modifiers that fire a binding. Unicode keys compare case-folded (`Trigger.foldedEqual`), so `ctrl+A`
/// and `ctrl+a` are one trigger.
#[derive(Debug, Clone, Copy)]
pub struct Trigger {
    pub key: TriggerKey,
    pub mods: Mods,
}

impl Trigger {
    /// A trigger on `key` with `mods`.
    #[must_use]
    pub fn new(key: TriggerKey, mods: Mods) -> Self {
        Self { key, mods }
    }

    /// `Trigger.parse`: one trigger of a sequence, `+`-separated modifiers and at most one key.
    ///
    /// # Errors
    ///
    /// [`BindError::InvalidFormat`] for an empty trigger, a repeated modifier, a second key or an unknown name.
    pub fn parse(input: &str) -> Result<Self, BindError> {
        if input.is_empty() {
            return Err(BindError::InvalidFormat);
        }
        let mut trigger = Self::new(TriggerKey::Physical(PhysicalKey::UNIDENTIFIED), Mods::empty());
        let mut rest = input;
        while !rest.is_empty() {
            let (part, after) = rest.split_once('+').unwrap_or((rest, ""));
            rest = after;
            if let Some((_, mods)) = MOD_NAMES.iter().find(|(name, _)| *name == part) {
                if trigger.mods.intersects(*mods) {
                    return Err(BindError::InvalidFormat);
                }
                trigger.mods |= *mods;
                continue;
            }
            if !trigger.is_key_unset() {
                return Err(BindError::InvalidFormat);
            }
            trigger.key = key_of(part).ok_or(BindError::InvalidFormat)?;
        }
        Ok(trigger)
    }

    /// Whether no key is set (`Trigger.isKeyUnset`): a trigger of modifiers only, or an `unidentified` key.
    #[must_use]
    pub fn is_key_unset(&self) -> bool {
        matches!(self.key, TriggerKey::Physical(key) if key == PhysicalKey::UNIDENTIFIED)
    }

    /// The key's identity for comparison: a unicode key case-folded.
    fn identity(&self) -> (u8, &'static str, [char; 3]) {
        match self.key {
            TriggerKey::Physical(key) => (0, key.name(), ['\0'; 3]),
            TriggerKey::Unicode(character) => (1, "", fold(character)),
            TriggerKey::CatchAll => (2, "", ['\0'; 3]),
        }
    }
}

impl PartialEq for Trigger {
    fn eq(&self, other: &Self) -> bool {
        self.mods == other.mods && self.identity() == other.identity()
    }
}

impl Eq for Trigger {}

impl Hash for Trigger {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.mods.hash(state);
        self.identity().hash(state);
    }
}

/// As `Trigger.format`: modifiers in ghostty's order, then the key.
impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (mods, name) in [
            (Mods::SUPER, "super"),
            (Mods::CTRL, "ctrl"),
            (Mods::ALT, "alt"),
            (Mods::SHIFT, "shift"),
        ] {
            if self.mods.contains(mods) {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            TriggerKey::Physical(key) => f.write_str(key.name()),
            TriggerKey::Unicode(character) => write!(f, "{character}"),
            TriggerKey::CatchAll => f.write_str("catch_all"),
        }
    }
}

/// `foldedCodepoint`: an ASCII letter lowercased; any other character through its case mappings (lower, upper, then
/// lower again), which joins the characters full case folding joins for a single key's character (`ß` and `ẞ`, `ς`
/// and `σ`, `ſ` and `s`).
fn fold(character: char) -> [char; 3] {
    let mut folded = ['\0'; 3];
    if character.is_ascii() {
        folded[0] = character.to_ascii_lowercase();
        return folded;
    }
    let mapped = character
        .to_lowercase()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase);
    for (slot, lower) in folded.iter_mut().zip(mapped) {
        *slot = lower;
    }
    folded
}

/// The key one trigger part names, in `Trigger.parse`'s order: a literal `+` (an empty part), a physical key name,
/// one character, a W3C code, `catch_all`, then the names ghostty 1.1 took.
fn key_of(part: &str) -> Option<TriggerKey> {
    if part.is_empty() {
        return Some(TriggerKey::Unicode('+'));
    }
    if let Some(key) = PhysicalKey::new(part).filter(|key| *key != PhysicalKey::UNIDENTIFIED) {
        return Some(TriggerKey::Physical(key));
    }
    let mut characters = part.chars();
    if let (Some(character), None) = (characters.next(), characters.next()) {
        return Some(TriggerKey::Unicode(character));
    }
    if let Some(key) = PhysicalKey::from_w3c(part) {
        return Some(TriggerKey::Physical(key));
    }
    if part == "catch_all" {
        return Some(TriggerKey::CatchAll);
    }
    legacy_key(part)
}

/// `backwards_compatible_keys`: the key names of ghostty 1.1.x, and their `physical:` forms.
fn legacy_key(part: &str) -> Option<TriggerKey> {
    const DIGITS: [&str; 10] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    ];
    const RENAMED: &[(&str, &str)] = &[
        ("grave_accent", "backquote"),
        ("left_bracket", "bracket_left"),
        ("right_bracket", "bracket_right"),
        ("up", "arrow_up"),
        ("down", "arrow_down"),
        ("left", "arrow_left"),
        ("right", "arrow_right"),
        ("kp_add", "numpad_add"),
        ("kp_subtract", "numpad_subtract"),
        ("kp_multiply", "numpad_multiply"),
        ("kp_divide", "numpad_divide"),
        ("kp_decimal", "numpad_decimal"),
        ("kp_enter", "numpad_enter"),
        ("kp_equal", "numpad_equal"),
        ("kp_separator", "numpad_separator"),
        ("kp_left", "numpad_left"),
        ("kp_right", "numpad_right"),
        ("kp_up", "numpad_up"),
        ("kp_down", "numpad_down"),
        ("kp_page_up", "numpad_page_up"),
        ("kp_page_down", "numpad_page_down"),
        ("kp_home", "numpad_home"),
        ("kp_end", "numpad_end"),
        ("kp_insert", "numpad_insert"),
        ("kp_delete", "numpad_delete"),
        ("kp_begin", "numpad_begin"),
        ("left_shift", "shift_left"),
        ("right_shift", "shift_right"),
        ("left_control", "control_left"),
        ("right_control", "control_right"),
        ("left_alt", "alt_left"),
        ("right_alt", "alt_right"),
        ("left_super", "meta_left"),
        ("right_super", "meta_right"),
    ];
    let (physical, name) = part
        .strip_prefix("physical:")
        .map_or((false, part), |name| (true, name));
    if let Some(digit) = DIGITS.iter().position(|word| *word == name) {
        let digit = char::from_digit(u32::try_from(digit).ok()?, 10)?;
        return Some(if physical {
            TriggerKey::Physical(PhysicalKey::new(&format!("digit_{digit}"))?)
        } else {
            TriggerKey::Unicode(digit)
        });
    }
    let renamed = match name {
        "plus" if !physical => return Some(TriggerKey::Unicode('+')),
        "apostrophe" if !physical => return Some(TriggerKey::Unicode('\'')),
        "apostrophe" => "quote",
        name => {
            if let Some(digit) = name.strip_prefix("kp_").filter(|digit| digit.len() == 1) {
                return PhysicalKey::new(&format!("numpad_{digit}")).map(TriggerKey::Physical);
            }
            RENAMED.iter().find(|(old, _)| *old == name)?.1
        }
    };
    PhysicalKey::new(renamed).map(TriggerKey::Physical)
}
