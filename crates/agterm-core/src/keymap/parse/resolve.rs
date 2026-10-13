//! The `map` lines' whole-file resolution: last-wins folding per action (upstream's `resolveMapLines`), the
//! compatibility vacating of shipped defaults an existing file already uses, and the order-independent built-in
//! collision fixpoint (`resolveBuiltinOverrides`, `builtinCollisionDrops`).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::super::BuiltinAction;
use super::super::KeymapDiagnostic;
use super::super::defaults::ShippedDefaults;
use super::super::keybind::{Alternative, Chord};
use super::{DropScope, ParsedCommandLine, ParsedMapLine};

/// A menu-bound `map` alternative, kept in file order until the collision pass.
pub(super) struct ParsedOverride {
    pub action: BuiltinAction,
    pub chord: Chord,
    pub line: usize,
}

/// One `map` line's monitor-bound alternatives, with the line to report a drop on and the wording scope.
pub(super) struct MapLineAlternatives {
    pub line: usize,
    pub scope: DropScope,
    pub alternatives: Vec<Alternative>,
    pub repeats: bool,
}

/// The `map` lines folded to one per action and split by dispatch path.
pub(super) struct ResolvedMapLines {
    /// The menu chords, in file order.
    pub overrides: Vec<ParsedOverride>,
    pub alternatives: BTreeMap<BuiltinAction, MapLineAlternatives>,
    /// The actions whose winning line offered no menu chord.
    pub unbound: BTreeSet<BuiltinAction>,
}

impl ResolvedMapLines {
    /// Whether the file maps `action` at all: a menu chord, alternatives, or an explicit lack of a menu chord.
    fn maps(&self, action: BuiltinAction) -> bool {
        self.overrides.iter().any(|parsed| parsed.action == action)
            || self.alternatives.contains_key(&action)
            || self.unbound.contains(&action)
    }

    /// Whether a binding of another action uses `chord`: as its menu chord, or as the first chord of a
    /// monitor-bound alternative.
    fn others_use(&self, action: BuiltinAction, chord: &Chord) -> bool {
        self.overrides
            .iter()
            .any(|parsed| parsed.action != action && parsed.chord == *chord)
            || self.alternatives.iter().any(|(owner, entry)| {
                *owner != action
                    && entry
                        .alternatives
                        .iter()
                        .any(|alternative| alternative.keybind.first() == Some(chord))
            })
    }
}

/// Folds the file-order `map` lines last-wins per action. A line declares the action's whole binding set, so a
/// later one replaces an earlier one's menu chord and alternatives together, a menu chord with none included.
pub(super) fn resolve_map_lines(map_lines: Vec<ParsedMapLine>) -> ResolvedMapLines {
    let mut latest: BTreeMap<BuiltinAction, ParsedMapLine> = BTreeMap::new();
    for map_line in map_lines {
        latest.insert(map_line.action, map_line);
    }
    let mut winners: Vec<ParsedMapLine> = latest.into_values().collect();
    winners.sort_by_key(|map_line| map_line.line);

    let mut resolved = ResolvedMapLines {
        overrides: Vec::new(),
        alternatives: BTreeMap::new(),
        unbound: BTreeSet::new(),
    };
    for map_line in winners {
        let has_chord = map_line.chord.is_some();
        match map_line.chord {
            Some(chord) => resolved.overrides.push(ParsedOverride {
                action: map_line.action,
                chord,
                line: map_line.line,
            }),
            None => {
                resolved.unbound.insert(map_line.action);
            }
        }
        if map_line.alternatives.is_empty() {
            continue;
        }
        let scope = DropScope::new(has_chord || map_line.alternatives.len() > 1);
        resolved.alternatives.insert(
            map_line.action,
            MapLineAlternatives {
                line: map_line.line,
                scope,
                alternatives: map_line.alternatives,
                repeats: map_line.repeats,
            },
        );
    }
    resolved
}

/// The unbound set plus every vacatable action a file written before its default existed would lose: one whose
/// shipped default chord the file uses elsewhere (another action's menu chord or alternative, or a command's
/// alternative) while it does not map the action itself. Mapping the action explicitly opts into its default.
pub(super) fn compatibility_unbound(
    resolved: &ResolvedMapLines,
    command_lines: &[ParsedCommandLine],
    defaults: &ShippedDefaults,
) -> BTreeSet<BuiltinAction> {
    let mut unbound = resolved.unbound.clone();
    for &action in defaults.vacate_when_used() {
        let Some(chord) = defaults.chord(action) else { continue };
        let used = resolved.others_use(action, chord)
            || command_lines.iter().any(|command_line| {
                command_line
                    .alternatives
                    .iter()
                    .any(|alternative| alternative.keybind.first() == Some(chord))
            });
        if used && !resolved.maps(action) {
            unbound.insert(action);
        }
    }
    unbound
}

/// The menu chord an action resolves to against an override set: the override, else the shipped default unless
/// the action is unbound.
pub(super) fn resolved_menu_chord<'a>(
    action: BuiltinAction,
    overrides: &'a BTreeMap<BuiltinAction, Chord>,
    unbound: &BTreeSet<BuiltinAction>,
    defaults: &'a ShippedDefaults,
) -> Option<&'a Chord> {
    if let Some(chord) = overrides.get(&action) {
        return Some(chord);
    }
    if unbound.contains(&action) {
        return None;
    }
    defaults.chord(action)
}

/// One dropped override: the action, the owner its diagnostic names, and its line.
struct CollisionDrop {
    loser: BuiltinAction,
    peer: BuiltinAction,
    line: usize,
}

/// Folds the overrides into the final menu chord map, rejecting only a true final-state collision: two distinct
/// actions on one chord. Order-independent: every overridden owner of a colliding chord drops in the same pass, an
/// override losing to an unmoved default and a collision of overrides alone dropping them all. A dropped override
/// reverts to its default and may collide afresh, so it repeats to a fixpoint; distinct shipped defaults make every
/// collision involve an override, so each pass shrinks the candidates. Diagnostics come out in line order.
pub(super) fn resolve_builtin_overrides(
    resolved: &ResolvedMapLines,
    unbound: &BTreeSet<BuiltinAction>,
    defaults: &ShippedDefaults,
    diagnostics: &mut Vec<KeymapDiagnostic>,
) -> BTreeMap<BuiltinAction, Chord> {
    let mut candidates: BTreeMap<BuiltinAction, Chord> = BTreeMap::new();
    let mut override_line: HashMap<BuiltinAction, usize> = HashMap::new();
    for parsed in &resolved.overrides {
        candidates.insert(parsed.action, parsed.chord.clone());
        override_line.insert(parsed.action, parsed.line);
    }

    let mut dropped: Vec<CollisionDrop> = Vec::new();
    loop {
        let drops = collision_drops(&candidates, unbound, defaults, &override_line);
        if drops.is_empty() {
            break;
        }
        for drop in drops {
            candidates.remove(&drop.loser);
            dropped.push(drop);
        }
    }

    dropped.sort_by_key(|drop| drop.line);
    for drop in dropped {
        let scope = DropScope::new(resolved.alternatives.contains_key(&drop.loser));
        diagnostics.push(KeymapDiagnostic::new(
            drop.line,
            format!(
                "chord conflicts with built-in '{}'; {}",
                drop.peer.name(),
                scope.map_skipped()
            ),
        ));
    }
    candidates
}

/// One fixpoint iteration: every overridden owner of a shared chord, paired with a stable peer for its diagnostic
/// (the chord's unmoved default owner, else the first other owner by name). Empty once collision-free.
fn collision_drops(
    candidates: &BTreeMap<BuiltinAction, Chord>,
    unbound: &BTreeSet<BuiltinAction>,
    defaults: &ShippedDefaults,
    override_line: &HashMap<BuiltinAction, usize>,
) -> Vec<CollisionDrop> {
    // an unbound action holds no chord, so its default stops blocking another built-in.
    let mut owners_by_chord: HashMap<&Chord, Vec<BuiltinAction>> = HashMap::new();
    for action in BuiltinAction::all() {
        if let Some(chord) = resolved_menu_chord(action, candidates, unbound, defaults) {
            owners_by_chord.entry(chord).or_default().push(action);
        }
    }

    let mut drops = Vec::new();
    for owners in owners_by_chord.values().filter(|owners| owners.len() > 1) {
        let default_owner = owners.iter().find(|owner| !candidates.contains_key(owner)).copied();
        let mut stable_owners = owners.clone();
        stable_owners.sort_by_key(|owner| owner.name());
        for &loser in owners.iter().filter(|owner| candidates.contains_key(owner)) {
            let peer = default_owner.or_else(|| stable_owners.iter().copied().find(|owner| *owner != loser));
            if let Some(peer) = peer {
                let line = override_line.get(&loser).copied().unwrap_or(0);
                drops.push(CollisionDrop { loser, peer, line });
            }
        }
    }
    drops
}
