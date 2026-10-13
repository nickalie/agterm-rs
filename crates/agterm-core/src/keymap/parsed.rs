//! The parsed `keymap.conf` (upstream's `Keymap` struct): built-in menu chords, the monitor-bound built-in binds,
//! custom commands and the global hotkey, resolved against the shipped default table it was parsed with.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::BuiltinAction;
use super::command::CustomCommand;
use super::defaults::ShippedDefaults;
use super::keybind::{Chord, Keybind};

/// One parse of `keymap.conf`. A built-in override is the single chord its menu item carries; every other built-in
/// bind (a leader sequence or a second chord) is monitor-bound in `builtin_sequences`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    pub(super) defaults: Arc<ShippedDefaults>,
    pub(super) builtin_overrides: BTreeMap<BuiltinAction, Chord>,
    pub(super) commands: Vec<CustomCommand>,
    pub(super) builtin_sequences: BTreeMap<BuiltinAction, Vec<Keybind>>,
    pub(super) builtin_unbound: BTreeSet<BuiltinAction>,
    pub(super) builtin_repeating: BTreeSet<BuiltinAction>,
    pub(super) global_hotkey: Option<Chord>,
}

impl Keymap {
    /// The keymap of no file at all: every built-in on its shipped default, no commands.
    #[must_use]
    pub fn empty(defaults: &ShippedDefaults) -> Self {
        Self::with_defaults(Arc::new(defaults.clone()))
    }

    pub(super) fn with_defaults(defaults: Arc<ShippedDefaults>) -> Self {
        Self {
            defaults,
            builtin_overrides: BTreeMap::new(),
            commands: Vec::new(),
            builtin_sequences: BTreeMap::new(),
            builtin_unbound: BTreeSet::new(),
            builtin_repeating: BTreeSet::new(),
            global_hotkey: None,
        }
    }

    /// The active menu chord of a built-in: the file's override, else the shipped default; `None` for a keyless
    /// action and for one a `map` line left with no menu chord.
    #[must_use]
    pub fn equivalent(&self, action: BuiltinAction) -> Option<&Chord> {
        if let Some(chord) = self.builtin_overrides.get(&action) {
            return Some(chord);
        }
        if self.builtin_unbound.contains(&action) {
            return None;
        }
        self.defaults.chord(action)
    }

    /// The monitor-bound binds of a built-in in file order, empty when it has none.
    #[must_use]
    pub fn sequences(&self, action: BuiltinAction) -> &[Keybind] {
        self.builtin_sequences.get(&action).map_or(&[], Vec::as_slice)
    }

    /// The table this keymap was resolved against.
    #[must_use]
    pub fn defaults(&self) -> &ShippedDefaults {
        &self.defaults
    }

    /// The menu chords the file moved built-ins to.
    #[must_use]
    pub fn builtin_overrides(&self) -> &BTreeMap<BuiltinAction, Chord> {
        &self.builtin_overrides
    }

    /// The custom commands in file order.
    #[must_use]
    pub fn commands(&self) -> &[CustomCommand] {
        &self.commands
    }

    /// Every built-in's monitor-bound binds: what a menu item cannot carry.
    #[must_use]
    pub fn builtin_sequences(&self) -> &BTreeMap<BuiltinAction, Vec<Keybind>> {
        &self.builtin_sequences
    }

    /// The actions left with no menu chord. Distinct from absent, which keeps the shipped default.
    #[must_use]
    pub fn builtin_unbound(&self) -> &BTreeSet<BuiltinAction> {
        &self.builtin_unbound
    }

    /// The actions whose `map` line carried `--repeat` and kept a leader sequence.
    #[must_use]
    pub fn builtin_repeating(&self) -> &BTreeSet<BuiltinAction> {
        &self.builtin_repeating
    }

    /// The system-wide chord summoning the quick terminal, `None` when the file binds none. Registered with the
    /// desktop rather than the key monitor, so it takes no part in the conflict model.
    #[must_use]
    pub fn global_hotkey(&self) -> Option<&Chord> {
        self.global_hotkey.as_ref()
    }
}
