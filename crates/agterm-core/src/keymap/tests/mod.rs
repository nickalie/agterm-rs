//! Ported from upstream's keymap suites (`KeybindTests`, `KeybindMatcherTests`, `KeymapTests`, `CustomCommandTests`,
//! `CustomCommandEngineTests`, `BuiltinActionTests`, `ControlKeymapTests`, `CommandFailureTests` and the keymap half
//! of `ConfigPathsTests`), run against [`ShippedDefaults::upstream`] so upstream's expectations stay verbatim. The
//! `AppKit` key-code and glyph tests stay with the frontend.

mod builtin;
mod command;
mod engine;
mod failure;
mod keybind;
mod matcher;
mod parse_command;
mod parse_cross;
mod parse_map;
mod parse_misc;
mod project;
mod starter;
mod store;

use std::collections::BTreeMap;

use super::*;

const CMD: Modifiers = Modifiers::COMMAND;
const CTRL: Modifiers = Modifiers::CONTROL;
const OPT: Modifiers = Modifiers::OPTION;
const SHIFT: Modifiers = Modifiers::SHIFT;
const NONE: Modifiers = Modifiers::empty();

fn chord(mods: Modifiers, key: &str) -> Chord {
    Chord::new(mods, key)
}

/// A keybind the grammar must accept.
fn keybind(text: &str) -> Keybind {
    parse_keybind(text).unwrap_or_else(|| panic!("'{text}' must parse"))
}

fn parse(text: &str) -> (Keymap, Vec<KeymapDiagnostic>) {
    parse_keymap(text, &ShippedDefaults::upstream())
}

fn default_chord(action: BuiltinAction) -> Option<Chord> {
    ShippedDefaults::upstream().chord(action).cloned()
}

/// Upstream's `Keymap(builtinOverrides:commands:)`.
fn keymap_with_overrides(overrides: impl IntoIterator<Item = (BuiltinAction, Chord)>) -> Keymap {
    Keymap {
        builtin_overrides: overrides.into_iter().collect(),
        ..Keymap::empty(&ShippedDefaults::upstream())
    }
}

fn overrides(pairs: impl IntoIterator<Item = (BuiltinAction, Chord)>) -> BTreeMap<BuiltinAction, Chord> {
    pairs.into_iter().collect()
}

fn messages(diagnostics: &[KeymapDiagnostic]) -> Vec<&str> {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect()
}

fn shortcuts(keymap: &Keymap) -> Vec<&str> {
    keymap
        .commands()
        .iter()
        .map(|command| command.shortcut.as_str())
        .collect()
}

fn diagnostic(line: usize, message: &str) -> KeymapDiagnostic {
    KeymapDiagnostic::new(line, message)
}

/// A realistic `keymap.conf` holding no `|` and no multi-chord `map`: the shape the compatibility invariant covers.
const PIPE_FREE_KEYMAP_FIXTURE: &str = "# regression fixture: no `|` anywhere, no multi-chord map
map cmd+shift+e toggle_split
map t toggle_sidebar
map ctrl+cmd+left focus_left_pane
map cmd+w new_session

command \"Deploy\" cmd+shift+y ./deploy.sh
command \"Open Notes\" vim {AGT_SESSION_PWD}/notes.md
command \"Clash\" command+shift+e echo clash
command \"First\" control+shift+g echo one
command \"Second\" ctrl+shift+g echo two";
