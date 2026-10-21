//! Reading and reaping the daemon namespace (upstream's `ZmxLifecycle.swift`): the leader map, the gate that paces
//! foreground refreshes and the launch reap policy, over the records the session host lists (`agterm-session`'s
//! `ServerHost`).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use agterm_host::LiveSessionRecord;

use super::is_daemon_name;
use crate::persist::RestoreMode;

/// The leader pid of every app daemon that reported one, by name; other names are left out.
#[must_use]
pub fn leaders(records: &[LiveSessionRecord]) -> BTreeMap<String, i32> {
    records
        .iter()
        .filter(|record| is_daemon_name(&record.name))
        .filter_map(|record| record.leader_pid.map(|pid| (record.name.clone(), pid)))
        .collect()
}

/// Paces the foreground refreshes of wrapped panes: one after every lifecycle change, else one per
/// [`RefreshGate::RECONCILE_INTERVAL`].
#[derive(Debug, Clone)]
pub struct RefreshGate {
    invalidated: bool,
    last_refresh_at: Option<Instant>,
}

impl Default for RefreshGate {
    fn default() -> Self {
        Self {
            invalidated: true,
            last_refresh_at: None,
        }
    }
}

impl RefreshGate {
    pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

    /// A daemon came or went: the next check refreshes.
    pub fn note_lifecycle_change(&mut self) {
        self.invalidated = true;
    }

    /// Whether to refresh at `now`; a yes counts as the refresh.
    pub fn should_refresh(&mut self, now: Instant) -> bool {
        let expired = self
            .last_refresh_at
            .is_none_or(|last| now.saturating_duration_since(last) >= Self::RECONCILE_INTERVAL);
        if !self.invalidated && !expired {
            return false;
        }
        self.invalidated = false;
        self.last_refresh_at = Some(now);
        true
    }
}

/// The daemons the launch reap kills; `None` when a requested-live inventory was incomplete and no reap is safe. A
/// deliberate non-live request claims no daemon and removes every zero-client daemon this app could have emitted;
/// a live one keeps the daemons its panes claim. Only the generated name shape is ever touched: the namespace is a
/// shared `/tmp` directory and this is the one path that destroys without the user asking.
#[must_use]
pub fn names_to_kill(
    records: &[LiveSessionRecord],
    requested: RestoreMode,
    known_names: Option<&BTreeSet<String>>,
) -> Option<Vec<String>> {
    let live = requested == RestoreMode::Live;
    if live && known_names.is_none() {
        return None;
    }
    Some(
        records
            .iter()
            .filter(|record| is_daemon_name(&record.name) && record.clients == Some(0))
            .filter(|record| !live || known_names.is_none_or(|known| !known.contains(&record.name)))
            .map(|record| record.name.clone())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::live::{NAME_PREFIX, daemon_name};

    fn record(name: &str, clients: Option<u32>, leader_pid: Option<i32>, created_at: Option<u64>) -> LiveSessionRecord {
        LiveSessionRecord {
            name: name.to_owned(),
            clients,
            leader_pid,
            created_at,
        }
    }

    fn fresh() -> String {
        daemon_name(Uuid::new_v4())
    }

    #[test]
    fn the_leader_map_keeps_only_generated_names_with_a_readable_pid() {
        let (app, no_pid, unreachable) = (fresh(), fresh(), fresh());
        let records = [
            record(&app, Some(0), Some(10), None),
            record("other", Some(0), Some(11), None),
            record("agterm-notes", Some(0), Some(12), None),
            record(&no_pid, Some(0), None, None),
            record(&unreachable, None, None, None),
        ];
        assert_eq!(leaders(&records), BTreeMap::from([(app, 10)]));
    }

    #[test]
    fn the_refresh_gate_invalidates_and_reconciles_slowly() {
        let mut gate = RefreshGate::default();
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        assert!(gate.should_refresh(start));
        assert!(!gate.should_refresh(at(1)));
        gate.note_lifecycle_change();
        assert!(gate.should_refresh(at(2)));
        assert!(!gate.should_refresh(at(3)));
        assert!(gate.should_refresh(at(2) + RefreshGate::RECONCILE_INTERVAL));
    }

    #[test]
    fn the_reap_uses_a_complete_live_inventory_and_zero_client_app_names_only() {
        let (known, orphan) = (fresh(), fresh());
        let records = [
            record(&known, Some(0), None, None),
            record(&orphan, Some(0), None, None),
            record(&fresh(), Some(1), None, None),
            record("other", Some(0), None, None),
            record(&fresh(), None, None, None),
        ];
        let claimed = BTreeSet::from([known.clone()]);
        assert_eq!(
            names_to_kill(&records, RestoreMode::Live, Some(&claimed)),
            Some(vec![orphan.clone()])
        );
        assert_eq!(names_to_kill(&records, RestoreMode::Live, None), None);
        assert_eq!(
            names_to_kill(&records, RestoreMode::None, None),
            Some(vec![known, orphan])
        );
    }

    #[test]
    fn the_reap_refuses_every_name_outside_the_generated_daemon_shape() {
        let ours = fresh();
        let records = [
            record(&ours, Some(0), None, None),
            record("agterm-notes", Some(0), None, None),
            record(NAME_PREFIX, Some(0), None, None),
            record(&format!("{NAME_PREFIX}{}", "a".repeat(31)), Some(0), None, None),
            record(&format!("{NAME_PREFIX}{}", "a".repeat(33)), Some(0), None, None),
            record(&format!("{NAME_PREFIX}{}", "A".repeat(32)), Some(0), None, None),
            record(&format!("{ours}-2"), Some(0), None, None),
        ];
        let expected = Some(vec![ours]);
        assert_eq!(
            names_to_kill(&records, RestoreMode::Live, Some(&BTreeSet::new())),
            expected
        );
        assert_eq!(names_to_kill(&records, RestoreMode::Rerun, None), expected);
        assert_eq!(names_to_kill(&records, RestoreMode::None, None), expected);
    }
}
