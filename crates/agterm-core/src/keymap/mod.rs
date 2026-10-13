//! `keymap.conf`: the chord grammar, the leader matcher, the built-in actions, custom commands and the parser
//! (upstream's `Keybind.swift`, `KeybindMatcher.swift`, `BuiltinAction.swift`, `Keymap.swift`, `CustomCommand.swift`,
//! `CustomCommandEngine.swift`, `CommandFailure.swift`, `ControlKeymap.swift` and the keymap half of
//! `ConfigPaths.swift`).
//!
//! The shipped default chords are a [`ShippedDefaults`] table the parser takes as a parameter, so a build can ship
//! its own; every resolved keymap remembers the table it was resolved against.

mod builtin;
mod command;
mod defaults;
mod engine;
pub mod failure;
mod keybind;
mod linux;
mod matcher;
mod parse;
mod parsed;
mod project;
mod starter;
mod store;
pub mod usage;

pub use builtin::BuiltinAction;
pub use command::{CommandContext, CommandPane, CustomCommand, SESSION_SCOPED_TOKEN_BASES};
pub use defaults::ShippedDefaults;
pub use engine::{CustomCommandEngine, EngineOutcome};
pub use keybind::{
    ARROW_KEYS, Chord, ConflictSide, FUNCTION_KEYS, Keybind, KeybindConflict, KeybindTarget, Modifiers,
    bindable_named_keys, has_key_position, has_malformed_alternative, is_arrow_key, is_bindable_named_key,
    is_function_key, is_reserved_monitor_chord, is_strict_keybind_prefix, keybind_conflicts, keybind_display_string,
    modifier_words, parse_keybind, parse_keybinds,
};
pub use linux::linux_defaults;
pub use matcher::{KeybindMatcher, MatchResult};
pub use parse::parse_keymap;
pub use parsed::Keymap;
pub use project::project;
pub use starter::{editor_command, starter_keymap_conf};
pub use store::{ensure_starter_keymap, load_keymap};

/// A problem found while parsing `keymap.conf` or `hooks.conf` (upstream's `KeymapDiagnostic`). `line` is 1-based;
/// `0` is a whole-file or cross-section problem belonging to no single line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapDiagnostic {
    pub line: usize,
    pub message: String,
}

impl KeymapDiagnostic {
    #[must_use]
    pub fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests;
