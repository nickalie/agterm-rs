//! The `keymap.conf` parser (upstream's `parseKeymap` and its passes in `Keymap.swift`).
//!
//! Line-based and kitty-flavored: blank and `#`-comment lines are skipped, the first word is the verb (`map`,
//! `command` or `global-hotkey`) and anything else is diagnosed. A bad line becomes a diagnostic and never discards
//! the rest of the file. Then, over the whole file: [`resolve`] folds the `map` lines last-wins per action, vacates
//! the shipped defaults an existing file already uses, and settles built-in menu chord collisions to a fixpoint;
//! [`validate`] settles every monitor-bound alternative of both verbs against the resulting chord set and hands
//! its default back to an action left with nothing.

mod lines;
mod resolve;
mod validate;

use std::collections::BTreeSet;
use std::sync::Arc;

use super::BuiltinAction;
use super::KeymapDiagnostic;
use super::command::CustomCommand;
use super::defaults::ShippedDefaults;
use super::keybind::{Alternative, Chord};
use super::parsed::Keymap;

/// Parses the text of a `keymap.conf` against a shipped default table. Never fails: each problem is a diagnostic,
/// in file order for the per-line ones, then the built-in collisions by line, then the cross-section drops.
#[must_use]
pub fn parse_keymap(text: &str, defaults: &ShippedDefaults) -> (Keymap, Vec<KeymapDiagnostic>) {
    // a CRLF leaves a `\r` blank-trimming keeps (`toggle_split\r` reads as an unknown action), and a lone-CR file
    // would collapse into one line.
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut collected = Collected::default();
    for (index, raw_line) in normalized.split('\n').enumerate() {
        collected.parse_line(index + 1, raw_line);
    }
    collected.finish(Arc::new(defaults.clone()))
}

/// What the per-line pass gathered, in file order, before any cross-line resolution.
#[derive(Default)]
struct Collected {
    map_lines: Vec<ParsedMapLine>,
    command_lines: Vec<ParsedCommandLine>,
    diagnostics: Vec<KeymapDiagnostic>,
    /// Last line wins; an OS-registered chord collides with nothing ngterm owns, so nothing resolves it further.
    global_hotkey: Option<Chord>,
}

impl Collected {
    fn diagnose(&mut self, line: usize, message: impl Into<String>) {
        self.diagnostics.push(KeymapDiagnostic::new(line, message));
    }

    /// Runs the whole-file passes and assembles the keymap.
    fn finish(self, defaults: Arc<ShippedDefaults>) -> (Keymap, Vec<KeymapDiagnostic>) {
        let Self {
            map_lines,
            command_lines,
            mut diagnostics,
            global_hotkey,
        } = self;
        let resolved = resolve::resolve_map_lines(map_lines);
        let unbound = resolve::compatibility_unbound(&resolved, &command_lines, &defaults);
        let overrides = resolve::resolve_builtin_overrides(&resolved, &unbound, &defaults, &mut diagnostics);
        let menu = MenuChords {
            overrides: &overrides,
            unbound: &unbound,
            defaults: &defaults,
        };
        let reverted: BTreeSet<BuiltinAction> = resolved
            .overrides
            .iter()
            .map(|parsed| parsed.action)
            .filter(|action| !overrides.contains_key(action))
            .collect();
        let alternatives = validate::monitor_alternatives(&command_lines, &resolved.alternatives);
        let survivors = validate::validate_bindings(alternatives, &menu.owners(), &reverted, &mut diagnostics);
        let sequences = validate::surviving_alternatives(&survivors);
        let builtin_repeating = sequences
            .iter()
            .filter(|(action, keybinds)| {
                resolved.alternatives.get(action).is_some_and(|entry| entry.repeats)
                    && keybinds.iter().any(|keybind| keybind.len() > 1)
            })
            .map(|(action, _)| *action)
            .collect();
        let builtin_unbound = validate::unbound_after_restoring_stranded_defaults(&menu, &survivors);
        let commands = validate::apply_surviving_shortcuts(command_lines, &survivors);
        let keymap = Keymap {
            builtin_overrides: overrides,
            commands,
            builtin_sequences: sequences,
            builtin_unbound,
            builtin_repeating,
            global_hotkey,
            ..Keymap::with_defaults(defaults)
        };
        (keymap, diagnostics)
    }
}

/// The menu chord set a parse resolves to: overrides, else shipped defaults, minus the unbound actions.
struct MenuChords<'a> {
    overrides: &'a std::collections::BTreeMap<BuiltinAction, Chord>,
    unbound: &'a BTreeSet<BuiltinAction>,
    defaults: &'a ShippedDefaults,
}

impl MenuChords<'_> {
    /// The menu chord an action resolves to: the parse-time spelling of [`Keymap::equivalent`].
    fn chord(&self, action: BuiltinAction) -> Option<&Chord> {
        resolve::resolved_menu_chord(action, self.overrides, self.unbound, self.defaults)
    }

    /// Each active menu chord and the action carrying it.
    fn owners(&self) -> std::collections::HashMap<Chord, BuiltinAction> {
        BuiltinAction::all()
            .filter_map(|action| self.chord(action).map(|chord| (chord.clone(), action)))
            .collect()
    }
}

/// Whether a diagnostic is about a binding as a whole or one alternative of several: the only thing separating the
/// two wordings, so a single-alternative binding's text stays byte-identical to the pre-alternatives parser's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DropScope {
    WholeBinding,
    Alternative,
}

impl DropScope {
    fn new(has_siblings: bool) -> Self {
        if has_siblings {
            Self::Alternative
        } else {
            Self::WholeBinding
        }
    }

    /// A parse-time or menu-collision rejection on a `map` line.
    fn map_skipped(self) -> &'static str {
        match self {
            Self::WholeBinding => "map skipped",
            Self::Alternative => "alternative skipped",
        }
    }

    /// A cross-section drop, on either verb.
    fn dropped(self) -> &'static str {
        match self {
            Self::WholeBinding => "keybind dropped",
            Self::Alternative => "alternative dropped",
        }
    }

    /// A `command` line rejection, whose whole-binding form leaves the command in the palette unkeyed.
    fn command_skipped(self) -> &'static str {
        match self {
            Self::WholeBinding => "treating the line as palette-only",
            Self::Alternative => "alternative skipped",
        }
    }
}

/// A valid `map` line: its menu-bindable alternative if it has one, plus the monitor-bound rest.
struct ParsedMapLine {
    action: BuiltinAction,
    chord: Option<Chord>,
    alternatives: Vec<Alternative>,
    repeats: bool,
    line: usize,
}

/// A `command` line's surviving parse-time alternatives held beside the command they key, so the shortcut string
/// is produced in exactly one place once the cross-section passes ran.
struct ParsedCommandLine {
    command: CustomCommand,
    alternatives: Vec<Alternative>,
}
