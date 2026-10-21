//! `live.list`, `live.prune`, `live.kill` and `live.screen` (upstream's `ControlDispatcher+Zmx.swift` and
//! `ControlServer+Zmx.swift`). Every one needs a running instance by design: only it can join its live stores, its
//! pending closes and its closed windows' files against the observed daemons in one answer.

use std::collections::BTreeSet;

use agterm_host::KillOutcome;
use agterm_proto::command::LiveCmd;
use agterm_proto::{Args, ControlLiveEntry, ControlLiveInventory, ControlResult, Request};

use super::{UNAVAILABLE, live_reset, not_yet, readback};
use crate::Core;
use crate::commands::surface::buffer_extent;
use crate::commands::{Outcome, args_of};
use crate::error::ControlError;
use crate::live::inventory::{
    ClaimState, DaemonObservation, InventoryResult, InventoryRow, PaneRole, join, names_to_prune,
};
use crate::live::reset::is_outdated;
use crate::live::screen::last_lines;
use crate::model::StatusPane;
use crate::pane_env::uuid_string;
use crate::resolve::{ACTIVE, TargetResolution, resolve, trimmed};

/// The refusal of a prune or kill over an inventory nothing can safely be decided on.
const INCOMPLETE_INVENTORY: &str =
    "the pane inventory is incomplete or has conflicting owners, so no daemon can be safely pruned";
/// A listing that failed is not an empty namespace.
const LIST_FAILED: &str = "could not read the live session list";

/// Answers a command of the `live.*` group.
pub(crate) fn dispatch(core: &mut Core, cmd: LiveCmd, request: &Request) -> Outcome {
    let args = args_of(request);
    match cmd {
        LiveCmd::List => list(core),
        LiveCmd::Prune => prune(core),
        LiveCmd::Kill => kill(core, request.target.as_deref(), args),
        LiveCmd::Screen => screen(core, args),
        LiveCmd::Reset => live_reset::reset(core, args),
        LiveCmd::Present => crate::remote::open_presentation(core, request.target.as_deref()),
        // `live.tree` starts through `deferred`, as its remote form waits on ssh
        other => Err(not_yet(other)),
    }
}

impl Core {
    /// The observed daemons joined against every pane claim; an error when this instance has no session host or the
    /// listing failed.
    fn inventory(&self) -> Result<InventoryResult, ControlError> {
        let host = self.live.require_host()?;
        let observed = host.list().map_err(|_| ControlError::rejected(LIST_FAILED))?;
        let walk = self.pane_claims();
        Ok(join(&observed, &walk.claims, walk.complete))
    }
}

/// `live.list`: the inventory under the restore status, with the endpoint and the reset read-back.
fn list(core: &mut Core) -> Outcome {
    Ok(ControlResult {
        live: Some(control_inventory(core)?),
        ..ControlResult::default()
    })
}

/// The inventory as `live.list` answers it, which `live.tree` joins against the open windows.
pub(crate) fn control_inventory(core: &Core) -> Result<ControlLiveInventory, ControlError> {
    let inventory = core.inventory()?;
    let outdated_before = core.live.outdated_before;
    Ok(ControlLiveInventory {
        restore: core.restore_status(),
        inventory_complete: inventory.inventory_complete,
        endpoint: core.live.endpoint.clone(),
        live_reset: readback(core.live.reset.pending.as_ref(), core.live.reset.last.as_ref()),
        entries: inventory.rows.iter().map(|row| entry(row, outdated_before)).collect(),
    })
}

/// One row on the wire.
fn entry(row: &InventoryRow, outdated_before: Option<u64>) -> ControlLiveEntry {
    let claim = row.claim.as_ref();
    let text = |value: &'static str| value.to_owned();
    ControlLiveEntry {
        daemon: row.daemon.clone(),
        state: text(row.state.into()),
        observation: text(row.observation.into()),
        clients: row.clients,
        leader_pid: row.leader_pid,
        window_id: claim.map(|claim| uuid_string(claim.window_id)),
        window_name: claim.and_then(|claim| claim.window_name.clone()),
        window_state: claim.map(|claim| text(claim.window_state.into())),
        workspace_id: claim.and_then(|claim| claim.workspace_id).map(uuid_string),
        workspace_name: claim.and_then(|claim| claim.workspace_name.clone()),
        session_id: claim.map(|claim| uuid_string(claim.session_id)),
        session_name: claim.and_then(|claim| claim.session_name.clone()),
        pane: claim.map(|claim| text(claim.pane.into())),
        outdated: is_outdated(row.created_at, outdated_before).then_some(true),
    }
}

/// `live.prune`: kills the daemons the inventory shows as unclaimed and detached. The gate is checked and revalidated,
/// never atomic: the host has no kill-if-detached, so this re-lists right before it kills and drops any candidate
/// that gained a client in between.
fn prune(core: &mut Core) -> Outcome {
    let inventory = core.inventory()?;
    let candidates = names_to_prune(&inventory).ok_or_else(|| ControlError::rejected(INCOMPLETE_INVENTORY))?;
    if candidates.is_empty() {
        return Ok(pruned("no orphan daemons", 0));
    }
    let host = core.live.require_host()?;
    let recheck = host
        .list()
        .map_err(|_| ControlError::rejected("could not re-read the live session list before pruning"))?;
    let still_detached: BTreeSet<&str> = recheck
        .iter()
        .filter(|record| record.clients == Some(0))
        .map(|record| record.name.as_str())
        .collect();
    let names: BTreeSet<&String> = candidates
        .iter()
        .filter(|name| still_detached.contains(name.as_str()))
        .collect();
    if names.is_empty() {
        return Ok(pruned("no orphan daemons left to prune", 0));
    }
    // never forced: a forced kill's stale-socket unlink cannot be told from success
    let outcomes: Vec<(&String, KillOutcome)> = names.into_iter().map(|name| (name, host.kill(name, false))).collect();
    let killed = outcomes
        .iter()
        .filter(|(_, outcome)| *outcome == KillOutcome::Killed)
        .count();
    core.live.leaders.borrow_mut().note_lifecycle_change();
    Ok(pruned(&prune_report(&outcomes), killed))
}

fn pruned(text: &str, affected: usize) -> ControlResult {
    ControlResult {
        text: Some(text.to_owned()),
        affected: Some(i64::try_from(affected).unwrap_or(i64::MAX)),
        ..ControlResult::default()
    }
}

/// Per daemon rather than a bare count: a stale-socket cleanup is not a kill, and a caller that cannot tell the two
/// apart would believe a live unresponsive daemon had gone.
fn prune_report(outcomes: &[(&String, KillOutcome)]) -> String {
    outcomes
        .iter()
        .map(|(name, outcome)| match outcome {
            KillOutcome::Killed => format!("killed {name}"),
            KillOutcome::StaleSocket => {
                format!("{name}: cleaned up a stale socket, the daemon may still be running")
            }
            KillOutcome::Failed(reason) => format!("{name}: not killed ({reason})"),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// `live.kill`: destroys one pane's daemon, then drives the transition the pane's own exit would have. Resolution runs
/// against the inventory, so it reaches closed and unindexed claims no window shows; nothing about it falls back to
/// what is in front of the user, so `active` is refused for both selectors.
fn kill(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let target = target
        .filter(|target| !target.is_empty())
        .ok_or_else(|| ControlError::rejected("live.kill requires an explicit --target"))?;
    let pane = args
        .pane
        .as_deref()
        .and_then(PaneRole::from_control_name)
        .ok_or_else(|| ControlError::rejected("live.kill requires --pane left|right"))?;
    if args.force != Some(true) {
        return Err(ControlError::rejected("live.kill requires --force"));
    }
    let inventory = core.inventory()?;
    if target == ACTIVE {
        return Err(ControlError::rejected(
            "live.kill needs a session id; 'active' is not accepted",
        ));
    }
    let window = args.window.as_deref();
    if window == Some(ACTIVE) {
        return Err(ControlError::rejected(
            "live.kill needs a window id; 'active' is not accepted",
        ));
    }
    let row = kill_row(&inventory, target, window, pane)?;
    if let Some(refusal) = kill_refusal(&row) {
        return Err(ControlError::rejected(refusal));
    }
    let host = core.live.require_host()?;
    // only an exact confirmation may close a live pane: a stale socket the host cleaned up confirms nothing
    match host.kill(&row.daemon, true) {
        KillOutcome::Killed => {}
        KillOutcome::StaleSocket => {
            return Err(ControlError::rejected(format!(
                "{} did not confirm the kill; the session host cleaned up a stale socket and the daemon may still \
                 be running",
                row.daemon
            )));
        }
        KillOutcome::Failed(reason) => {
            return Err(ControlError::rejected(format!(
                "could not kill {}: {reason}",
                row.daemon
            )));
        }
    }
    core.live.leaders.borrow_mut().note_lifecycle_change();
    let claim = row
        .claim
        .as_ref()
        .ok_or_else(|| ControlError::rejected(INCOMPLETE_INVENTORY))?;
    core.apply_killed_pane_exit(claim.session_id, claim.pane_identity, pane);
    Ok(ControlResult {
        id: Some(uuid_string(claim.session_id)),
        text: Some(format!("killed {}", row.daemon)),
        pane: Some(<&str>::from(pane).to_owned()),
        ..ControlResult::default()
    })
}

/// The row `live.kill` addresses: the claims of `pane`, scoped to `--window` before the session resolves, so a prefix
/// ambiguous across windows can be disambiguated and an exact id in another window is not killed.
fn kill_row(
    inventory: &InventoryResult,
    target: &str,
    window: Option<&str>,
    pane: PaneRole,
) -> Result<InventoryRow, ControlError> {
    let mut owned: Vec<&InventoryRow> = inventory
        .rows
        .iter()
        .filter(|row| row.claim.as_ref().is_some_and(|claim| claim.pane == pane))
        .collect();
    if let Some(window) = window.filter(|window| !window.is_empty()) {
        let mut windows: Vec<uuid::Uuid> = Vec::new();
        for claim in inventory.rows.iter().filter_map(|row| row.claim.as_ref()) {
            if !windows.contains(&claim.window_id) {
                windows.push(claim.window_id);
            }
        }
        let TargetResolution::Resolved(window_id) = resolve(window, &windows, None) else {
            return Err(ControlError::rejected(format!("no such window: {window}")));
        };
        owned.retain(|row| row.claim.as_ref().is_some_and(|claim| claim.window_id == window_id));
    }
    let candidates: Vec<uuid::Uuid> = owned
        .iter()
        .filter_map(|row| row.claim.as_ref().map(|claim| claim.session_id))
        .collect();
    let missing = || ControlError::rejected(format!("no {} pane daemon for session {target}", <&str>::from(pane)));
    let TargetResolution::Resolved(session) = resolve(target, &candidates, None) else {
        return Err(missing());
    };
    owned
        .into_iter()
        .find(|row| row.claim.as_ref().is_some_and(|claim| claim.session_id == session))
        .cloned()
        .ok_or_else(missing)
}

/// Why a row may not be killed, `None` when it may (upstream's `ControlZmxError.killRefusal`). `absent` has nothing to
/// kill, and `unreadable` is refused because a forced kill there can unlink a live daemon's socket and still exit
/// zero, leaving it running and unreachable by name.
fn kill_refusal(row: &InventoryRow) -> Option<String> {
    match row.observation {
        DaemonObservation::Absent => return Some(format!("{} is not running", row.daemon)),
        DaemonObservation::Unreadable => {
            return Some(format!(
                "{} is unreadable; killing it could orphan a live daemon",
                row.daemon
            ));
        }
        DaemonObservation::Running => {}
    }
    match row.state {
        ClaimState::Claimed => None,
        ClaimState::PendingClose => Some(format!(
            "{} belongs to a session waiting out its undo window",
            row.daemon
        )),
        ClaimState::Unknown | ClaimState::Conflicted => Some(INCOMPLETE_INVENTORY.to_owned()),
        ClaimState::Orphan | ClaimState::Foreign => Some(format!("{} is not claimed by that pane", row.daemon)),
    }
}

impl Core {
    /// Runs the pane's exit transition for a daemon `live.kill` destroyed (upstream's `applyKilledPaneExit`): only a
    /// pane whose terminal is a client of that daemon closes, as its own exit would have closed it, and the killed
    /// identity is kept out of the finalizer. A requested-live fallback pane runs a plain shell that never attached;
    /// a dormant pane counts by the daemon it would attach to (`Core::pane_backed`), or its wake would start the killed
    /// program again.
    pub(crate) fn apply_killed_pane_exit(&mut self, session: uuid::Uuid, identity: uuid::Uuid, pane: PaneRole) {
        let role = match pane {
            PaneRole::Left => StatusPane::Left,
            PaneRole::Right => StatusPane::Right,
        };
        let backed = self
            .library()
            .store_for_session(session)
            .and_then(|store| store.session(session))
            .is_some_and(|found| self.pane_backed(found, role));
        if !backed {
            return;
        }
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return;
        };
        match role {
            StatusPane::Left => store.close_primary_pane(session),
            _ => store.close_split_pane(session),
        }
        self.note_already_finalized(identity);
    }
}

/// `live.screen`: the daemon's last leader's grid, by daemon name; it attaches nothing.
fn screen(core: &mut Core, args: &Args) -> Outcome {
    let name = trimmed(args.name.as_deref())
        .filter(|name| is_plain(name))
        .ok_or_else(|| ControlError::rejected("live.screen requires a daemon name"))?;
    let (all, lines) = buffer_extent(args)?;
    let host = core
        .live
        .host()
        .cloned()
        .ok_or_else(|| ControlError::rejected(UNAVAILABLE))?;
    let screen = host
        .screen(name, all || lines.is_some())
        .map_err(|_| ControlError::rejected(format!("could not read the live session screen of {name}")))?;
    let text = match lines {
        Some(lines) => last_lines(&screen.text, lines),
        None => screen.text,
    };
    Ok(ControlResult::text(text))
}

/// A token that is one plain word (upstream's `RemoteSession.isPlain`): no whitespace, no control character.
pub(crate) fn is_plain(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || u32::from(c) < 0x20 || u32::from(c) == 0x7f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_word_has_no_whitespace_or_control_character() {
        assert!(is_plain("agterm-0123"));
        for refused in ["", "two words", "tab\there", "bell\u{7}", "del\u{7f}", "nbsp\u{a0}x"] {
            assert!(!is_plain(refused), "{refused:?}");
        }
    }
}
