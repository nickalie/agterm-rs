//! The key monitor's matcher over custom commands and the built-in binds a menu cannot carry (upstream's
//! `CustomCommandEngine.swift`). Each alternative of a shortcut is a separate matcher entry sharing one target.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use super::BuiltinAction;
use super::command::CustomCommand;
use super::keybind::{Chord, Keybind, KeybindTarget, parse_keybinds};
use super::matcher::{KeybindMatcher, MatchResult};
use super::parsed::Keymap;

/// What one chord resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineOutcome {
    /// A custom command's shortcut completed.
    Fired(CustomCommand),
    /// A built-in's monitor-bound bind completed.
    FiredBuiltin(BuiltinAction),
    /// A leader is armed, awaiting the next chord.
    Armed,
    /// Nothing matched; the chord passes through.
    Unmatched,
}

/// Resolves each chord the frontend's key monitor sees to what should run.
#[derive(Debug, Clone)]
pub struct CustomCommandEngine {
    matcher: KeybindMatcher,
    commands_by_id: HashMap<Uuid, CustomCommand>,
}

impl CustomCommandEngine {
    /// An engine over `commands` (each non-empty shortcut's alternatives; an unparseable one binds nothing) and the
    /// keymap's built-in sequences, registered sorted by action name so the order does not depend on map layout.
    #[must_use]
    pub fn new(
        commands: &[CustomCommand],
        builtin_sequences: &BTreeMap<BuiltinAction, Vec<Keybind>>,
        builtin_repeating: &BTreeSet<BuiltinAction>,
    ) -> Self {
        let mut commands_by_id = HashMap::new();
        let mut binds: Vec<(Keybind, KeybindTarget)> = Vec::new();
        let mut repeating: HashSet<KeybindTarget> = builtin_repeating
            .iter()
            .map(|&action| KeybindTarget::Builtin(action))
            .collect();
        for command in commands {
            commands_by_id.entry(command.id).or_insert_with(|| command.clone());
            if command.shortcut.is_empty() {
                continue;
            }
            let Some(keybinds) = parse_keybinds(&command.shortcut) else {
                continue;
            };
            let target = KeybindTarget::Command(command.id);
            binds.extend(keybinds.into_iter().map(|keybind| (keybind, target)));
            if command.repeats {
                repeating.insert(target);
            }
        }
        let mut sequences: Vec<(&BuiltinAction, &Vec<Keybind>)> = builtin_sequences.iter().collect();
        sequences.sort_by_key(|(action, _)| action.name());
        for (&action, keybinds) in sequences {
            binds.extend(
                keybinds
                    .iter()
                    .map(|keybind| (keybind.clone(), KeybindTarget::Builtin(action))),
            );
        }
        Self {
            matcher: KeybindMatcher::new(binds, repeating),
            commands_by_id,
        }
    }

    /// The engine for a parsed keymap.
    #[must_use]
    pub fn from_keymap(keymap: &Keymap) -> Self {
        Self::new(
            keymap.commands(),
            keymap.builtin_sequences(),
            keymap.builtin_repeating(),
        )
    }

    /// Feeds one chord.
    pub fn advance(&mut self, chord: &Chord) -> EngineOutcome {
        match self.matcher.advance(chord) {
            MatchResult::Fired(KeybindTarget::Command(id)) => self
                .commands_by_id
                .get(&id)
                .cloned()
                .map_or(EngineOutcome::Unmatched, EngineOutcome::Fired),
            MatchResult::Fired(KeybindTarget::Builtin(action)) => EngineOutcome::FiredBuiltin(action),
            MatchResult::Armed => EngineOutcome::Armed,
            MatchResult::Unmatched => EngineOutcome::Unmatched,
        }
    }

    /// Whether a leader is armed.
    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.matcher.is_armed()
    }

    /// Whether a repeatable sequence's repeat window is open.
    #[must_use]
    pub fn is_repeating(&self) -> bool {
        self.matcher.is_repeating()
    }

    /// Whether `chord` would fire again inside the open repeat window.
    #[must_use]
    pub fn is_repeat_tail(&self, chord: &Chord) -> bool {
        self.matcher.is_repeat_tail(chord)
    }

    /// Abandons a half-typed sequence and closes the repeat window.
    pub fn reset(&mut self) {
        self.matcher.reset();
    }
}
