//! What zmx reports joined against what agterm's panes claim (upstream's `ZmxInventory.swift`), the rows `live.list`
//! explains, and the prune policy over them.

use std::collections::{BTreeMap, BTreeSet};

use agterm_host::LiveSessionRecord;
use uuid::Uuid;

use super::{daemon_name, is_daemon_name};

/// How a daemon relates to the panes agterm knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "camelCase")]
pub enum ClaimState {
    Claimed,
    Orphan,
    Unknown,
    Conflicted,
    PendingClose,
    Foreign,
}

/// What the listing could see of the daemon itself. Separate from the client count because `clients == None` alone
/// cannot tell a daemon that is gone from one zmx failed to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "camelCase")]
pub enum DaemonObservation {
    Running,
    Unreadable,
    Absent,
}

/// The window a claim belongs to: loaded, persisted-but-closed, or present on disk yet missing from `windows.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "camelCase")]
pub enum OwnerWindowState {
    Open,
    Closed,
    Unindexed,
}

/// Which pane of a session a claim belongs to; the scratch pane is never wrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "camelCase")]
pub enum PaneRole {
    Left,
    Right,
}

impl PaneRole {
    /// The spellings the rest of the API takes for a pane, so `--pane primary` means here what it means everywhere
    /// else; read-back stays `left`/`right`.
    #[must_use]
    pub fn from_control_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "left" | "top" | "primary" => Some(Self::Left),
            "right" | "bottom" | "split" => Some(Self::Right),
            _ => None,
        }
    }
}

/// One pane that expects a daemon, with the names and ids needed to explain and address it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneClaim {
    pub pane_identity: Uuid,
    pub pane: PaneRole,
    /// Soft-closed and inside its undo window: absent from the workspaces but its daemon still owned.
    pub pending_close: bool,
    pub window_id: Uuid,
    /// `None` for an unindexed window: names live only in `windows.json`, which lacks it.
    pub window_name: Option<String>,
    pub window_state: OwnerWindowState,
    pub workspace_id: Option<Uuid>,
    pub workspace_name: Option<String>,
    pub session_id: Uuid,
    pub session_name: Option<String>,
}

/// The panes that expect a daemon, and whether the walk could account for everything it found. Incomplete means some
/// pane is unaccounted for, so nothing may be pruned against this answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimWalk {
    pub claims: Vec<PaneClaim>,
    pub complete: bool,
}

/// One row of the runtime inventory: a daemon, a pane, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryRow {
    pub daemon: String,
    pub state: ClaimState,
    pub observation: DaemonObservation,
    pub clients: Option<u32>,
    pub leader_pid: Option<i32>,
    /// `None` for a foreign daemon, an orphan and a conflicted name: no single pane owns any of them.
    pub claim: Option<PaneClaim>,
    pub created_at: Option<u64>,
}

/// The joined inventory. `inventory_complete` is false when the claim walk could not account for everything or two
/// panes claim one daemon, since neither answer can be acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryResult {
    pub rows: Vec<InventoryRow>,
    pub inventory_complete: bool,
}

/// The union of the observed daemons and the expected claims, sorted by daemon name: a union so `live.list` can explain
/// both a leaked daemon and a pane whose daemon vanished, and `live.prune` can be audited against what it showed.
#[must_use]
pub fn join(observed: &[LiveSessionRecord], claims: &[PaneClaim], inventory_complete: bool) -> InventoryResult {
    let mut claims_by_name: BTreeMap<String, Vec<&PaneClaim>> = BTreeMap::new();
    for claim in claims {
        claims_by_name
            .entry(daemon_name(claim.pane_identity))
            .or_default()
            .push(claim);
    }
    let conflicted: BTreeSet<&String> = claims_by_name
        .iter()
        .filter(|(_, group)| group.len() > 1)
        .map(|(name, _)| name)
        .collect();
    let row = |facts: Facts, group: &[&PaneClaim]| {
        let is_conflicted = conflicted.contains(&facts.daemon);
        facts.row(group, is_conflicted, inventory_complete)
    };
    let mut rows: Vec<InventoryRow> = observed
        .iter()
        .map(|record| {
            let group = claims_by_name.get(&record.name).map_or(&[][..], Vec::as_slice);
            row(Facts::observed(record), group)
        })
        .collect();
    let observed_names: BTreeSet<&str> = observed.iter().map(|record| record.name.as_str()).collect();
    for (name, group) in &claims_by_name {
        if !observed_names.contains(name.as_str()) {
            rows.push(row(Facts::absent(name), group));
        }
    }
    rows.sort_by(|a, b| a.daemon.cmp(&b.daemon));
    InventoryResult {
        rows,
        inventory_complete: inventory_complete && conflicted.is_empty(),
    }
}

/// What the listing saw of one daemon, before any claim is applied.
struct Facts {
    daemon: String,
    observation: DaemonObservation,
    clients: Option<u32>,
    leader_pid: Option<i32>,
    created_at: Option<u64>,
}

impl Facts {
    fn observed(record: &LiveSessionRecord) -> Self {
        Self {
            daemon: record.name.clone(),
            observation: if record.clients.is_some() {
                DaemonObservation::Running
            } else {
                DaemonObservation::Unreadable
            },
            clients: record.clients,
            leader_pid: record.leader_pid,
            created_at: record.created_at,
        }
    }

    fn absent(daemon: &str) -> Self {
        Self {
            daemon: daemon.to_owned(),
            observation: DaemonObservation::Absent,
            clients: None,
            leader_pid: None,
            created_at: None,
        }
    }

    fn row(self, claims: &[&PaneClaim], conflicted: bool, inventory_complete: bool) -> InventoryRow {
        let (state, claim) = if conflicted {
            (ClaimState::Conflicted, None)
        } else if let Some(owner) = claims.first() {
            let state = if owner.pending_close {
                ClaimState::PendingClose
            } else {
                ClaimState::Claimed
            };
            (state, Some((*owner).clone()))
        } else if is_daemon_name(&self.daemon) {
            // an unmatched name is an orphan only when the claim walk saw everything; otherwise its pane may simply be
            // one this inventory could not read
            let state = if inventory_complete {
                ClaimState::Orphan
            } else {
                ClaimState::Unknown
            };
            (state, None)
        } else {
            (ClaimState::Foreign, None)
        };
        InventoryRow {
            daemon: self.daemon,
            state,
            observation: self.observation,
            clients: self.clients,
            leader_pid: self.leader_pid,
            claim,
            created_at: self.created_at,
        }
    }
}

/// The daemons `live.prune` may take; `None` when no prune is safe at all. Every term is required: a complete,
/// conflict-free inventory, an identity no pane claims, a daemon observed running with no clients. A closed window's
/// panes are claimed with zero clients, so the count alone never implies an orphan. Observed, not guaranteed: pinned
/// zmx has no kill-if-detached, so the caller re-lists right before it kills and drops anything that gained a client.
#[must_use]
pub fn names_to_prune(result: &InventoryResult) -> Option<Vec<String>> {
    result.inventory_complete.then(|| {
        result
            .rows
            .iter()
            .filter(|row| {
                row.state == ClaimState::Orphan
                    && row.observation == DaemonObservation::Running
                    && row.clients == Some(0)
            })
            .map(|row| row.daemon.clone())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(last: u8) -> Uuid {
        Uuid::from_u128(u128::from(last))
    }

    fn claim(pane_identity: Uuid, pane: PaneRole, pending_close: bool, session_id: Uuid) -> PaneClaim {
        PaneClaim {
            pane_identity,
            pane,
            pending_close,
            window_id: id(0xc0),
            window_name: Some("window 1".to_owned()),
            window_state: OwnerWindowState::Open,
            workspace_id: None,
            workspace_name: Some("default".to_owned()),
            session_id,
            session_name: Some("build".to_owned()),
        }
    }

    fn left(pane_identity: Uuid) -> PaneClaim {
        claim(pane_identity, PaneRole::Left, false, id(0xd0))
    }

    fn record(name: &str, clients: Option<u32>, leader_pid: Option<i32>) -> LiveSessionRecord {
        LiveSessionRecord {
            name: name.to_owned(),
            clients,
            leader_pid,
            created_at: None,
        }
    }

    fn row<'a>(result: &'a InventoryResult, daemon: &str) -> &'a InventoryRow {
        result.rows.iter().find(|row| row.daemon == daemon).unwrap()
    }

    #[test]
    fn a_claimed_daemon_carries_its_owner_and_client_count() {
        let name = daemon_name(id(0xaa));
        let mut observed = record(&name, Some(1), Some(42));
        observed.created_at = Some(7);
        let absent = Uuid::new_v4();
        let result = join(&[observed], &[left(id(0xaa)), left(absent)], true);
        let claimed = row(&result, &name);
        assert_eq!(
            (claimed.state, claimed.observation, claimed.clients, claimed.leader_pid),
            (ClaimState::Claimed, DaemonObservation::Running, Some(1), Some(42))
        );
        assert_eq!(claimed.claim.as_ref().unwrap().session_name.as_deref(), Some("build"));
        assert_eq!(claimed.created_at, Some(7));
        assert_eq!(row(&result, &daemon_name(absent)).created_at, None);
    }

    #[test]
    fn an_unmatched_daemon_is_an_orphan_only_when_the_inventory_is_complete() {
        let name = daemon_name(id(0xaa));
        let observed = [record(&name, Some(0), Some(42))];
        assert_eq!(row(&join(&observed, &[], true), &name).state, ClaimState::Orphan);
        assert_eq!(row(&join(&observed, &[], false), &name).state, ClaimState::Unknown);
    }

    #[test]
    fn closed_unindexed_and_soft_closed_claims_stay_owned() {
        let name = daemon_name(id(0xaa));
        let observed = [record(&name, Some(0), Some(42))];
        let mut closed = left(id(0xaa));
        closed.window_state = OwnerWindowState::Closed;
        let result = join(&observed, &[closed], true);
        assert_eq!(
            (row(&result, &name).state, row(&result, &name).clients),
            (ClaimState::Claimed, Some(0))
        );
        let mut unindexed = left(id(0xaa));
        unindexed.window_state = OwnerWindowState::Unindexed;
        unindexed.window_name = None;
        let result = join(&observed, &[unindexed], true);
        assert_eq!(row(&result, &name).claim.as_ref().unwrap().window_name, None);
        let pending = claim(id(0xaa), PaneRole::Left, true, id(0xd0));
        assert_eq!(
            row(&join(&observed, &[pending], true), &name).state,
            ClaimState::PendingClose
        );
    }

    #[test]
    fn an_unreadable_row_is_distinct_from_an_absent_daemon() {
        let (a, b) = (daemon_name(id(0xaa)), daemon_name(id(0xbb)));
        let result = join(
            &[record(&a, None, None)],
            &[left(id(0xaa)), claim(id(0xbb), PaneRole::Right, false, id(0xd0))],
            true,
        );
        let unreadable = row(&result, &a);
        assert_eq!(
            (unreadable.observation, unreadable.clients),
            (DaemonObservation::Unreadable, None)
        );
        let absent = row(&result, &b);
        assert_eq!(
            (absent.observation, absent.state, absent.clients),
            (DaemonObservation::Absent, ClaimState::Claimed, None)
        );
    }

    #[test]
    fn a_duplicate_pane_identity_conflicts_its_row_instead_of_picking_an_owner() {
        let name = daemon_name(id(0xaa));
        let orphan = daemon_name(id(0xee));
        let result = join(
            &[record(&name, Some(0), Some(42)), record(&orphan, Some(0), Some(44))],
            &[left(id(0xaa)), claim(id(0xaa), PaneRole::Right, false, id(0xd1))],
            true,
        );
        let rows: Vec<_> = result.rows.iter().filter(|row| row.daemon == name).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].state, rows[0].claim.as_ref()), (ClaimState::Conflicted, None));
        assert!(!result.inventory_complete);
        assert_eq!(names_to_prune(&result), None);
    }

    #[test]
    fn user_daemons_merely_starting_with_the_prefix_are_foreign_not_orphans() {
        for name in ["agterm-not-ours", "agterm-00112233445566778899AABBCCDDEEFF", "notes"] {
            let result = join(&[record(name, Some(0), Some(7))], &[], true);
            assert_eq!(
                (row(&result, name).state, row(&result, name).claim.as_ref()),
                (ClaimState::Foreign, None)
            );
            assert_eq!(names_to_prune(&result), Some(Vec::new()));
        }
    }

    #[test]
    fn prune_takes_only_unmatched_detached_running_app_daemons() {
        let (a, b) = (daemon_name(id(0xaa)), daemon_name(id(0xbb)));
        let (attached, unreadable) = (daemon_name(id(0xf1)), daemon_name(id(0xf2)));
        let observed = [
            record(&a, Some(0), Some(42)),
            record(&b, Some(0), Some(43)),
            record("notes", Some(0), Some(7)),
            record(&attached, Some(2), Some(45)),
            record(&unreadable, None, None),
        ];
        let result = join(&observed, &[claim(id(0xbb), PaneRole::Right, false, id(0xd0))], true);
        assert_eq!(names_to_prune(&result), Some(vec![a]));
        assert_eq!(names_to_prune(&join(&observed, &[], false)), None);
        assert_eq!(names_to_prune(&join(&[], &[left(id(0xaa))], true)), Some(Vec::new()));
    }

    #[test]
    fn rows_are_sorted_by_daemon_name() {
        let result = join(
            &[record("zz", Some(0), None), record("aa", Some(0), None)],
            &[left(id(0xaa))],
            true,
        );
        let names: Vec<&str> = result.rows.iter().map(|row| row.daemon.as_str()).collect();
        assert_eq!(names, ["aa", daemon_name(id(0xaa)).as_str(), "zz"]);
    }

    #[test]
    fn pane_roles_take_the_apis_spellings() {
        for name in ["left", "TOP", "primary"] {
            assert_eq!(PaneRole::from_control_name(name), Some(PaneRole::Left));
        }
        for name in ["right", "bottom", "Split"] {
            assert_eq!(PaneRole::from_control_name(name), Some(PaneRole::Right));
        }
        assert_eq!(PaneRole::from_control_name("scratch"), None);
        assert_eq!(<&str>::from(ClaimState::PendingClose), "pendingClose");
    }
}
