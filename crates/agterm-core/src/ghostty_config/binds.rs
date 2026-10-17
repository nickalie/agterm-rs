//! ghostty's `keybind` config and its actions, ported from `src/input/Binding.zig`, `Config.Keybinds` and
//! `Surface.maybeHandleBinding` at `LIBGHOSTTY_VT_REV`: what libghostty resolves inside `ghostty_surface_key` for an
//! embedder, for a frontend that encodes keys itself with libghostty-vt.
//!
//! **Lines** (`keybind = …`, [`Keybinds::parse_cli`]): `[flags:]trigger[>trigger…]=action`, the flags `all:`,
//! `global:`, `unconsumed:` and `performable:`; a trigger is `+`-joined modifiers (`shift ctrl alt super`, the
//! aliases `cmd command opt option control`) and one key: a physical key ([`PHYSICAL_KEYS`]: `key_c`, `arrow_up`,
//! or its W3C code `KeyC`) bound by position whatever the layout, a single character bound by what the layout types,
//! or `catch_all`. `chain=action` adds an action to the binding before it, `name/binding` binds in key table `name`
//! (`name/` empties it), `unbind` removes a binding, an empty value restores ghostty's built-in binds and `clear`
//! drops them all. The built-in binds are ghostty's Linux ones ([`Keybinds::defaults`]); agterm's
//! `ghostty-defaults.conf` adds physical-key copy, paste and select all on top, so they work on any layout.
//!
//! **Actions** ([`Action`]): ghostty's whole action table, so any valid line loads clean; an unknown action or a
//! malformed parameter is a diagnostic as in libghostty (`unknown error error.InvalidAction` /
//! `error.InvalidFormat`). The frontend performs them ([`Performer`]); one it does not support is not performed. The
//! actions core sends through `Surface::binding_action` (`paste_from_clipboard`, `select_all`, `start_search`,
//! `search:TEXT`, `navigate_search:…`, `end_search`, `increase_font_size:1`, `decrease_font_size:1`,
//! `reset_font_size`, `set_font_size:N`) are strings of this table: a frontend parses them with [`Action`]'s
//! `FromStr` and performs them like a bound key's.
//!
//! **Resolution order** ([`Keyboard::route`]): a key press goes to agterm's keymap first (custom commands and the
//! built-in chords, which see the key in the window before the focused terminal — upstream's local key monitor and
//! menu, the GTK build's capture-phase controller), then to the terminal, where ghostty's binds take it
//! ([`Keyboard::handle`]: the half-typed sequence, the active key tables innermost first, then the root set; looked up
//! by physical key, the character typed, the unshifted character, then `catch_all`), then to the key encoder when no
//! binding kept it. A `performable:` binding whose action is not performed acts as if it did not exist; an
//! `unconsumed:` one performs and still encodes. Character triggers compare by Unicode full case folding, as
//! ghostty's do.

mod action;
mod defaults;
mod keyboard;
mod set;
mod trigger;

use std::fmt;

pub use action::{
    Action, ActionKind, CloseTabMode, CopyFormat, CrashThread, Direction, InspectorMode, OpenConfig, ResizeDirection,
    Scope, SelectionAdjust, SplitDirection, SplitFocus, WriteAction, WriteFormat, WriteScreen,
};
pub use keyboard::{KeyAction, KeyEffect, KeyEvent, KeyResult, KeyRoute, Keyboard, Performer, Queued};
pub use set::{BindFlags, BindSet, BindingLine, Entry, Leaf};
pub use trigger::{Mods, PHYSICAL_KEYS, PhysicalKey, Trigger, TriggerKey};

/// Every action's name, in ghostty's order.
pub fn action_names() -> impl Iterator<Item = &'static str> {
    <ActionKind as strum::IntoEnumIterator>::iter().map(<&'static str>::from)
}

/// Why a keybind line or action was refused: the Zig error `Binding` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindError {
    /// `error.InvalidFormat`: a malformed line, trigger or parameter.
    InvalidFormat,
    /// `error.InvalidAction`: no such action.
    InvalidAction,
}

impl BindError {
    /// The Zig error's name, as libghostty's diagnostic carries it.
    #[must_use]
    pub fn zig_name(self) -> &'static str {
        match self {
            Self::InvalidFormat => "error.InvalidFormat",
            Self::InvalidAction => "error.InvalidAction",
        }
    }
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.zig_name())
    }
}

impl std::error::Error for BindError {}

/// The `keybind` config (`Config.Keybinds`): the root set, the key tables in the order they were named, and where a
/// `chain=` line goes.
#[derive(Debug, Clone)]
pub struct Keybinds {
    pub set: BindSet,
    tables: Vec<(String, BindSet)>,
    /// The key table a `chain=` line extends; `None` for the root set.
    chain_target: Option<String>,
}

/// As ghostty's `Keybinds.equal`: the root set and the tables.
impl PartialEq for Keybinds {
    fn eq(&self, other: &Self) -> bool {
        self.set == other.set
            && self.tables.len() == other.tables.len()
            && self.tables.iter().all(|(name, set)| other.table(name) == Some(set))
    }
}

impl Default for Keybinds {
    fn default() -> Self {
        Self::defaults()
    }
}

impl Keybinds {
    /// ghostty's built-in Linux binds (`Keybinds.init`), what a config starts from and `keybind =` restores.
    #[must_use]
    pub fn defaults() -> Self {
        let mut set = BindSet::default();
        defaults::put_defaults(&mut set);
        Self { set, ..Self::cleared() }
    }

    /// No binds at all (`keybind = clear`).
    #[must_use]
    pub fn cleared() -> Self {
        Self {
            set: BindSet::default(),
            tables: Vec::new(),
            chain_target: None,
        }
    }

    /// The key table `name`, if a line defined it.
    #[must_use]
    pub fn table(&self, name: &str) -> Option<&BindSet> {
        self.tables.iter().find(|(table, _)| table == name).map(|(_, set)| set)
    }

    /// The key tables' names, in the order they were defined.
    pub fn table_names(&self) -> impl Iterator<Item = &str> {
        self.tables.iter().map(|(name, _)| name.as_str())
    }

    /// `Keybinds.parseCLI`: one `keybind` value.
    ///
    /// # Errors
    ///
    /// What [`BindSet::parse_and_put`] refuses, and a `chain=` line inside a key table; nothing changes then, but a
    /// table a refused `name/binding` names is defined, as in ghostty.
    pub fn parse_cli(&mut self, value: &str) -> Result<(), BindError> {
        match value {
            "" => *self = Self::defaults(),
            "clear" => *self = Self::cleared(),
            _ => {
                if let Some((name, binding)) = table_binding(value) {
                    return self.put_in_table(name, binding);
                }
                if value.starts_with("chain=") {
                    return self.chain(value);
                }
                self.set.parse_and_put(value)?;
                self.chain_target = None;
            }
        }
        Ok(())
    }

    fn put_in_table(&mut self, name: &str, binding: &str) -> Result<(), BindError> {
        let index = self
            .tables
            .iter()
            .position(|(table, _)| table == name)
            .unwrap_or_else(|| {
                self.tables.push((name.to_owned(), BindSet::default()));
                self.tables.len() - 1
            });
        let set = &mut self.tables[index].1;
        if binding.is_empty() {
            *set = BindSet::default();
            self.chain_target = None;
            return Ok(());
        }
        if binding.starts_with("chain=") {
            return Err(BindError::InvalidFormat);
        }
        set.parse_and_put(binding)?;
        self.chain_target = Some(name.to_owned());
        Ok(())
    }

    fn chain(&mut self, value: &str) -> Result<(), BindError> {
        let Some(name) = self.chain_target.clone() else {
            return self.set.parse_and_put(value);
        };
        if let Some((_, set)) = self.tables.iter_mut().find(|(table, _)| *table == name) {
            return set.parse_and_put(value);
        }
        self.chain_target = None;
        Err(BindError::InvalidFormat)
    }
}

/// `name/binding`: a key table's line. The `/` must come before the first `=` (`a=text:/x` is a root binding), and
/// a name that is empty or holds `+` or `>` is part of a trigger instead (`/=action`, `ctrl+/=action`).
fn table_binding(value: &str) -> Option<(&str, &str)> {
    let equals = value.find('=').unwrap_or(value.len());
    let slash = value[..equals].find('/')?;
    let name = &value[..slash];
    (!name.is_empty() && !name.contains(['+', '>'])).then(|| (name, &value[slash + 1..]))
}

/// Checks one `keybind` value on its own, as the config's key table does for every line: whatever
/// [`Keybinds::parse_cli`] would refuse in any context. Only a `chain=` with nothing before it to extend depends on
/// the lines before it, and is refused when the line is applied.
///
/// # Errors
///
/// The [`BindError`] libghostty reports for the line.
pub fn check(value: &str) -> Result<(), BindError> {
    if value.is_empty() || value == "clear" {
        return Ok(());
    }
    let binding = match table_binding(value) {
        Some((_, "")) => return Ok(()),
        Some((_, binding)) if binding.starts_with("chain=") => return Err(BindError::InvalidFormat),
        Some((_, binding)) => binding,
        None => value,
    };
    let line = BindingLine::parse(binding)?;
    if line.chain && line.action == Action::Unbind {
        return Err(BindError::InvalidFormat);
    }
    Ok(())
}
