//! The chord grammar (upstream's `Keybind.swift`, host-free half): modifiers, chords, keybinds, the `|`/`>`/`+`
//! parser, the reserved monitor chords and the prefix-conflict relation. The `AppKit` key-code tables stay with the
//! frontend; only the set of keys a physical position produces ([`has_key_position`]) is kept, for the
//! `global-hotkey` check.

use bitflags::bitflags;
use uuid::Uuid;

use super::BuiltinAction;

bitflags! {
    /// The keyboard modifiers a chord may require (upstream's `Modifier`), spelled in `keymap.conf` as
    /// `ctrl`/`control`, `cmd`/`command`, `opt`/`option`/`alt` and `shift`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Modifiers: u8 {
        const CONTROL = 1;
        const COMMAND = 1 << 1;
        const OPTION = 1 << 2;
        const SHIFT = 1 << 3;
    }
}

/// Every modifier with its canonical word, in the fixed display order `ctrl+cmd+opt+shift`.
const MODIFIER_WORDS: [(Modifiers, &str); 4] = [
    (Modifiers::CONTROL, "ctrl"),
    (Modifiers::COMMAND, "cmd"),
    (Modifiers::OPTION, "opt"),
    (Modifiers::SHIFT, "shift"),
];

/// The canonical words of `mods` in display order: the one place a modifier's spelling is rendered, so changing
/// the displayed vocabulary is a single edit.
pub fn modifier_words(mods: Modifiers) -> impl Iterator<Item = &'static str> {
    MODIFIER_WORDS
        .into_iter()
        .filter(move |(modifier, _)| mods.contains(*modifier))
        .map(|(_, word)| word)
}

/// The modifier a lowercased token names, every accepted spelling included.
fn modifier_for(token: &str) -> Option<Modifiers> {
    match token {
        "ctrl" | "control" => Some(Modifiers::CONTROL),
        "cmd" | "command" => Some(Modifiers::COMMAND),
        "opt" | "option" | "alt" => Some(Modifiers::OPTION),
        "shift" => Some(Modifiers::SHIFT),
        _ => None,
    }
}

/// The four arrow keys. Separate from the other named keys for one rule: a built-in `map` may not put a
/// modifier-less arrow on the menu.
pub const ARROW_KEYS: [&str; 4] = ["left", "right", "up", "down"];

/// The bindable function keys, permitted to start a shortcut without modifiers.
pub const FUNCTION_KEYS: [&str; 20] = [
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13", "f14", "f15", "f16", "f17",
    "f18", "f19", "f20",
];

/// The named keys besides the arrows and the function keys. `esc` is the leader abort and is not bindable.
const OTHER_NAMED_KEYS: [&str; 4] = ["tab", "space", "return", "delete"];

/// The Latin characters an ANSI key position produces (upstream's `latinKey` table): what a `global-hotkey` may
/// name besides the named keys.
const LATIN_POSITION_KEYS: &str = "abcdefghijklmnopqrstuvwxyz0123456789=-][';\\,/.`";

/// Whether `key` is a function key `f1` through `f20`.
#[must_use]
pub fn is_function_key(key: &str) -> bool {
    FUNCTION_KEYS.contains(&key)
}

/// Whether `key` is an arrow key.
#[must_use]
pub fn is_arrow_key(key: &str) -> bool {
    ARROW_KEYS.contains(&key)
}

/// Whether `key` is one of the multi-character base keys the grammar accepts (upstream's `bindableNamedKeys`).
#[must_use]
pub fn is_bindable_named_key(key: &str) -> bool {
    OTHER_NAMED_KEYS.contains(&key) || is_arrow_key(key) || is_function_key(key)
}

/// Every bindable named key: tab, space, return, delete, the arrows and the function keys.
pub fn bindable_named_keys() -> impl Iterator<Item = &'static str> {
    OTHER_NAMED_KEYS.into_iter().chain(ARROW_KEYS).chain(FUNCTION_KEYS)
}

/// Whether a physical key position produces `key`: every named key and the Latin ANSI characters. A system-wide
/// hotkey is registered by position, so a base key no position produces (`é`, `!`) cannot be registered.
#[must_use]
pub fn has_key_position(key: &str) -> bool {
    if is_bindable_named_key(key) {
        return true;
    }
    let mut chars = key.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if LATIN_POSITION_KEYS.contains(c))
}

/// A single key press: modifiers plus a base key (a lowercased single character or a bindable named key).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub mods: Modifiers,
    pub key: String,
}

impl Chord {
    #[must_use]
    pub fn new(mods: Modifiers, key: impl Into<String>) -> Self {
        Self { mods, key: key.into() }
    }

    /// Whether the chord may start a monitor-bound shortcut: it carries a modifier or is a bare function key.
    #[must_use]
    pub fn can_start_shortcut(&self) -> bool {
        !self.mods.is_empty() || is_function_key(&self.key)
    }

    /// The chord in kitty syntax (`cmd+shift+e`), modifiers in the fixed `ctrl+cmd+opt+shift` order.
    #[must_use]
    pub fn display_string(&self) -> String {
        let mut parts: Vec<&str> = modifier_words(self.mods).collect();
        parts.push(&self.key);
        parts.join("+")
    }
}

/// An ordered sequence of chords: one chord is a simple shortcut, more is a leader sequence (`ctrl+a>b`).
pub type Keybind = Vec<Chord>;

/// A keybind in kitty syntax, chords joined with `>` (`ctrl+a>g`).
#[must_use]
pub fn keybind_display_string(keybind: &[Chord]) -> String {
    keybind.iter().map(Chord::display_string).collect::<Vec<_>>().join(">")
}

/// Whether `c` is horizontal whitespace (upstream's `CharacterSet.whitespaces`: space separators and tab).
pub(crate) fn is_blank(c: char) -> bool {
    c == '\t'
        || (c.is_whitespace()
            && !matches!(
                c,
                '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'
            ))
}

/// `s` with horizontal whitespace trimmed from both ends.
pub(crate) fn trim_blank(s: &str) -> &str {
    s.trim_matches(is_blank)
}

/// The leading run of `s` up to the first whitespace character.
pub(crate) fn first_word(s: &str) -> &str {
    s.find(char::is_whitespace).map_or(s, |end| &s[..end])
}

/// Parses a keybind: `>`-separated chords, each a `+`-joined list of modifier words and one base key,
/// case-insensitive, whitespace around separators ignored. `None` for an empty input or chord, a chord with no or
/// several base keys, or a multi-character key outside the bindable named keys.
#[must_use]
pub fn parse_keybind(s: &str) -> Option<Keybind> {
    s.split('>').map(parse_chord).collect()
}

fn parse_chord(s: &str) -> Option<Chord> {
    let tokens: Vec<String> = s.split('+').map(|token| trim_blank(token).to_lowercase()).collect();
    if tokens.iter().any(String::is_empty) {
        return None;
    }
    let mut mods = Modifiers::empty();
    let mut key: Option<String> = None;
    for token in tokens {
        if let Some(modifier) = modifier_for(&token) {
            mods.insert(modifier);
        } else if key.replace(token).is_some() {
            return None;
        }
    }
    let key = key?;
    (key.chars().count() == 1 || is_bindable_named_key(&key)).then(|| Chord::new(mods, key))
}

/// Parses a binding token into its `|`-separated alternatives. `None` when any alternative is malformed, which
/// rejects the whole token so a typo cannot hide behind a half that parsed.
#[must_use]
pub fn parse_keybinds(s: &str) -> Option<Vec<Keybind>> {
    s.split('|').map(parse_keybind).collect()
}

/// Whether a token [`parse_keybinds`] rejected still reads as an intended binding: it offers `|` alternatives and
/// at least one parses. Tells a typo in one alternative from a shell pipeline leading a `command` line.
#[must_use]
pub fn has_malformed_alternative(s: &str) -> bool {
    let parts: Vec<&str> = s.split('|').collect();
    parts.len() > 1 && parts.iter().any(|part| parse_keybind(part).is_some())
}

/// One alternative of a binding token: the user's own spelling and what it parsed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Alternative {
    pub raw: String,
    pub keybind: Keybind,
}

/// A binding token's alternatives in file order with every repeat of an earlier keybind dropped; `None` when any
/// alternative is malformed. The raw spelling rides along so diagnostics and shortcuts quote what the user wrote.
pub(crate) fn alternative_keybinds(s: &str) -> Option<Vec<Alternative>> {
    let keybinds = parse_keybinds(s)?;
    let mut kept: Vec<Alternative> = Vec::new();
    for (raw, keybind) in s.split('|').zip(keybinds) {
        if !kept.iter().any(|alternative| alternative.keybind == keybind) {
            kept.push(Alternative {
                raw: raw.to_owned(),
                keybind,
            });
        }
    }
    Some(kept)
}

/// Whether a chord is owned by an always-on key monitor and so can never be bound: Control-Tab under any other
/// modifiers (the session switcher) and Control-1/Control-2 with Control alone (the pane shortcuts).
#[must_use]
pub fn is_reserved_monitor_chord(chord: &Chord) -> bool {
    if chord.mods.contains(Modifiers::CONTROL) && chord.key == "tab" {
        return true;
    }
    chord.mods == Modifiers::CONTROL && (chord.key == "1" || chord.key == "2")
}

/// What a keybind fires: a custom command by id, or a built-in action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeybindTarget {
    Command(Uuid),
    Builtin(BuiltinAction),
}

/// One side of a [`KeybindConflict`]: the target and the exact keybind involved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConflictSide {
    pub target: KeybindTarget,
    pub keybind: Keybind,
}

/// Two monitor-bound binds that conflict: identical, or one a sequence prefix of the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindConflict {
    pub first: ConflictSide,
    pub second: ConflictSide,
}

/// Every conflicting pair among `binds`, each pair once and in input order (`first` precedes `second`).
#[must_use]
pub fn keybind_conflicts(binds: &[(Keybind, KeybindTarget)]) -> Vec<KeybindConflict> {
    let keybinds: Vec<&[Chord]> = binds.iter().map(|(keybind, _)| keybind.as_slice()).collect();
    conflicting_pairs(&keybinds)
        .into_iter()
        .map(|(i, j)| {
            let side = |index: usize| ConflictSide {
                target: binds[index].1,
                keybind: binds[index].0.clone(),
            };
            KeybindConflict {
                first: side(i),
                second: side(j),
            }
        })
        .collect()
}

/// The index pairs `(i, j)`, `i < j`, whose keybinds are equal or one a prefix of the other.
pub(crate) fn conflicting_pairs(keybinds: &[&[Chord]]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for (i, a) in keybinds.iter().enumerate() {
        for (j, b) in keybinds.iter().enumerate().skip(i + 1) {
            if a == b || is_strict_keybind_prefix(a, b) || is_strict_keybind_prefix(b, a) {
                pairs.push((i, j));
            }
        }
    }
    pairs
}

/// Whether `prefix` is a strictly shorter leading part of `keybind`: the relation under which the matcher fires
/// the shorter bind and the longer can never run.
#[must_use]
pub fn is_strict_keybind_prefix(prefix: &[Chord], keybind: &[Chord]) -> bool {
    prefix.len() < keybind.len() && keybind.starts_with(prefix)
}
