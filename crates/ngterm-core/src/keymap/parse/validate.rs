//! The cross-section passes over every monitor-bound alternative of both verbs (upstream's `validateBindings`,
//! `dropShadowedAlternatives`, `dropConflictingAlternatives`, `survivingAlternatives`, `applySurvivingShortcuts`,
//! `unboundAfterRestoringStrandedDefaults`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use super::super::BuiltinAction;
use super::super::KeymapDiagnostic;
use super::super::command::CustomCommand;
use super::super::keybind::{
    Chord, Keybind, KeybindTarget, conflicting_pairs, is_reserved_monitor_chord, is_strict_keybind_prefix,
};
use super::resolve::MapLineAlternatives;
use super::{DropScope, MenuChords, ParsedCommandLine};

/// One monitor-bound alternative entering the passes: its owner, the raw spelling to quote, the parsed keybind and
/// the line to report a drop on (`0` for a custom command, whose line is not tracked).
pub(super) struct MonitorAlternative {
    target: KeybindTarget,
    owner_name: String,
    raw: String,
    keybind: Keybind,
    scope: DropScope,
    line: usize,
}

impl MonitorAlternative {
    fn owner(&self) -> String {
        match self.target {
            KeybindTarget::Command(_) => format!("custom command '{}'", self.owner_name),
            KeybindTarget::Builtin(_) => format!("built-in '{}'", self.owner_name),
        }
    }

    fn subject(&self) -> String {
        match self.target {
            KeybindTarget::Command(_) => format!("{} shortcut '{}'", self.owner(), self.raw),
            KeybindTarget::Builtin(_) => format!("{} chord '{}'", self.owner(), self.raw),
        }
    }

    fn diagnostic(&self, conflicts_with: &str) -> KeymapDiagnostic {
        KeymapDiagnostic::new(
            self.line,
            format!(
                "{} conflicts with {conflicts_with}; {}",
                self.subject(),
                self.scope.dropped()
            ),
        )
    }
}

/// Every monitor-bound alternative of both verbs: custom commands first, then `map` lines in file order.
pub(super) fn monitor_alternatives(
    command_lines: &[ParsedCommandLine],
    map_alternatives: &BTreeMap<BuiltinAction, MapLineAlternatives>,
) -> Vec<MonitorAlternative> {
    let mut alternatives = Vec::new();
    for command_line in command_lines {
        let scope = DropScope::new(command_line.alternatives.len() > 1);
        let command = &command_line.command;
        alternatives.extend(command_line.alternatives.iter().map(|alternative| MonitorAlternative {
            target: KeybindTarget::Command(command.id),
            owner_name: command.name.clone(),
            raw: alternative.raw.clone(),
            keybind: alternative.keybind.clone(),
            scope,
            line: 0,
        }));
    }
    let mut map_entries: Vec<(&BuiltinAction, &MapLineAlternatives)> = map_alternatives.iter().collect();
    map_entries.sort_by_key(|(_, entry)| entry.line);
    for (&action, entry) in map_entries {
        alternatives.extend(entry.alternatives.iter().map(|alternative| MonitorAlternative {
            target: KeybindTarget::Builtin(action),
            owner_name: action.name().to_owned(),
            raw: alternative.raw.clone(),
            keybind: alternative.keybind.clone(),
            scope: entry.scope,
            line: entry.line,
        }));
    }
    alternatives
}

/// The alternatives surviving both passes. Only the offending alternative goes, its siblings keep firing, and a
/// command losing every one stays in the palette.
pub(super) fn validate_bindings(
    alternatives: Vec<MonitorAlternative>,
    menu_owners: &HashMap<Chord, BuiltinAction>,
    reverted_defaults: &BTreeSet<BuiltinAction>,
    diagnostics: &mut Vec<KeymapDiagnostic>,
) -> Vec<MonitorAlternative> {
    let unshadowed = drop_shadowed_alternatives(alternatives, menu_owners, reverted_defaults, diagnostics);
    drop_conflicting_alternatives(unshadowed, diagnostics)
}

/// Drops every alternative the monitor would never see: one whose first chord is an active menu chord, or that
/// holds a reserved monitor chord anywhere. A rejected override's revived default names its owner, since that
/// conflict was not visible as written.
fn drop_shadowed_alternatives(
    alternatives: Vec<MonitorAlternative>,
    menu_owners: &HashMap<Chord, BuiltinAction>,
    reverted_defaults: &BTreeSet<BuiltinAction>,
    diagnostics: &mut Vec<KeymapDiagnostic>,
) -> Vec<MonitorAlternative> {
    let mut kept = Vec::new();
    for alternative in alternatives {
        let owner = alternative
            .keybind
            .first()
            .and_then(|chord| menu_owners.get(chord))
            .copied();
        let kind = match owner {
            Some(owner)
                if reverted_defaults.contains(&owner) && alternative.target != KeybindTarget::Builtin(owner) =>
            {
                format!("built-in '{}'", owner.name())
            }
            Some(_) => "a built-in".to_owned(),
            None if alternative.keybind.iter().any(is_reserved_monitor_chord) => "a reserved shortcut".to_owned(),
            None => {
                kept.push(alternative);
                continue;
            }
        };
        diagnostics.push(alternative.diagnostic(&kind));
    }
    kept
}

/// Settles every duplicate-or-prefix conflict in one pass over a relation computed once: a cross-target pair
/// drops both sides, a same-target prefix pair drops the longer (the matcher fires the shorter, so nothing is
/// lost). Nothing is recomputed and no drop cascades, which is what keeps line and `|` order from deciding an
/// outcome; an alternative whose only conflict was with one that also went still dies, deliberately.
fn drop_conflicting_alternatives(
    alternatives: Vec<MonitorAlternative>,
    diagnostics: &mut Vec<KeymapDiagnostic>,
) -> Vec<MonitorAlternative> {
    let keybinds: Vec<&[Chord]> = alternatives
        .iter()
        .map(|alternative| alternative.keybind.as_slice())
        .collect();
    // the other offender each charged alternative's diagnostic names: the first one found, in file order.
    let mut other_offender: HashMap<usize, String> = HashMap::new();
    for (first, second) in conflicting_pairs(&keybinds) {
        if alternatives[first].target == alternatives[second].target {
            let longer = if is_strict_keybind_prefix(keybinds[first], keybinds[second]) {
                second
            } else {
                first
            };
            other_offender
                .entry(longer)
                .or_insert_with(|| alternatives[longer].owner());
            continue;
        }
        other_offender
            .entry(first)
            .or_insert_with(|| alternatives[second].owner());
        other_offender
            .entry(second)
            .or_insert_with(|| alternatives[first].owner());
    }

    let mut survivors = Vec::new();
    for (index, alternative) in alternatives.into_iter().enumerate() {
        match other_offender.get(&index) {
            Some(other) => diagnostics.push(alternative.diagnostic(other)),
            None => survivors.push(alternative),
        }
    }
    survivors
}

/// The surviving monitor-bound binds of each built-in, in file order.
pub(super) fn surviving_alternatives(survivors: &[MonitorAlternative]) -> BTreeMap<BuiltinAction, Vec<Keybind>> {
    let mut sequences: BTreeMap<BuiltinAction, Vec<Keybind>> = BTreeMap::new();
    for survivor in survivors {
        if let KeybindTarget::Builtin(action) = survivor.target {
            sequences.entry(action).or_default().push(survivor.keybind.clone());
        }
    }
    sequences
}

/// The commands with each keyed one's shortcut spliced from the raw spellings of its surviving alternatives (never
/// re-rendered); one that lost every alternative ends up palette-only. `repeats` survives only beside a surviving
/// leader sequence, the one shape that can repeat.
pub(super) fn apply_surviving_shortcuts(
    command_lines: Vec<ParsedCommandLine>,
    survivors: &[MonitorAlternative],
) -> Vec<CustomCommand> {
    let mut raws: HashMap<Uuid, Vec<&str>> = HashMap::new();
    let mut sequenced: HashSet<Uuid> = HashSet::new();
    for survivor in survivors {
        if let KeybindTarget::Command(id) = survivor.target {
            raws.entry(id).or_default().push(&survivor.raw);
            if survivor.keybind.len() > 1 {
                sequenced.insert(id);
            }
        }
    }
    command_lines
        .into_iter()
        .map(|command_line| {
            let mut command = command_line.command;
            command.repeats = command.repeats && sequenced.contains(&command.id);
            if !command_line.alternatives.is_empty() {
                command.shortcut = raws.get(&command.id).map(|raws| raws.join("|")).unwrap_or_default();
            }
            command
        })
        .collect()
}

/// The unbound set minus every stranded action — one whose `map` line lost its last bind to the cross-section
/// passes — that can have its default back: the line bound nothing, so it never asked to move the action. An
/// action stays unbound when something else holds its default now, which being unbound is what permitted.
pub(super) fn unbound_after_restoring_stranded_defaults(
    menu: &MenuChords<'_>,
    survivors: &[MonitorAlternative],
) -> BTreeSet<BuiltinAction> {
    let still_bound: BTreeSet<BuiltinAction> = survivors
        .iter()
        .filter_map(|survivor| match survivor.target {
            KeybindTarget::Builtin(action) => Some(action),
            KeybindTarget::Command(_) => None,
        })
        .collect();
    let stranded: BTreeSet<BuiltinAction> = menu.unbound.difference(&still_bound).copied().collect();
    if stranded.is_empty() {
        return menu.unbound.clone();
    }

    let mut occupied: HashSet<&Chord> = survivors
        .iter()
        .filter_map(|survivor| survivor.keybind.first())
        .collect();
    occupied.extend(
        BuiltinAction::all()
            .filter(|action| !stranded.contains(action))
            .filter_map(|action| menu.chord(action)),
    );
    // the shipped defaults are distinct, so two stranded actions never want the same chord.
    let restored: BTreeSet<BuiltinAction> = stranded
        .into_iter()
        .filter(|&action| {
            menu.defaults
                .chord(action)
                .is_some_and(|chord| !occupied.contains(chord))
        })
        .collect();
    menu.unbound.difference(&restored).copied().collect()
}
