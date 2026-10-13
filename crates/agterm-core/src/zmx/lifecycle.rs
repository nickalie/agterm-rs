//! Reading and reaping the daemon namespace (upstream's `ZmxLifecycle.swift`): the `zmx list` parser, the leader map,
//! the gate that paces foreground refreshes and the launch reap policy. The client running them is `agterm-zmx`'s.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use agterm_host::LiveSessionRecord;

use super::is_daemon_name;
use crate::persist::RestoreMode;

/// Why a `zmx list` output was refused. A refused listing is never an empty namespace: nothing may be reaped from it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListParseError {
    #[error("a zmx list row has no name")]
    MissingName,
    #[error("the zmx list row of {0} has no client count")]
    MissingClients(String),
    #[error("invalid zmx client count: {0}")]
    InvalidClients(String),
    #[error("invalid zmx leader pid: {0}")]
    InvalidLeaderPid(String),
}

/// The records of a `zmx list` output: one tab-separated `key=value` row per daemon, the current one marked `→ `. An
/// `err=` row (a daemon zmx could not read) keeps no client count; invalid creation metadata is dropped rather than
/// failing the listing, which reap and host discovery depend on.
///
/// # Errors
///
/// A row without a name, without a client count (and no `err=`), or with a malformed count or leader pid.
pub fn parse_list(output: &str) -> Result<Vec<LiveSessionRecord>, ListParseError> {
    output
        .split(is_newline)
        .filter(|line| !line.is_empty())
        .map(parse_row)
        .collect()
}

/// Swift's `Character.isNewline`, the separator upstream splits the listing on.
fn is_newline(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

fn parse_row(line: &str) -> Result<LiveSessionRecord, ListParseError> {
    let line = line.trim_matches(|c: char| c.is_whitespace());
    let line = line.strip_prefix("→ ").unwrap_or(line);
    let mut name = None;
    let mut clients = None;
    let mut leader_pid = None;
    let mut created_at = None;
    let mut has_error = false;
    for field in line.split('\t') {
        if let Some(raw) = field.strip_prefix("name=") {
            name = Some(raw);
        } else if let Some(raw) = field.strip_prefix("clients=") {
            clients = Some(
                raw.parse::<u32>()
                    .map_err(|_| ListParseError::InvalidClients(raw.to_owned()))?,
            );
        } else if let Some(raw) = field.strip_prefix("pid=") {
            let pid = raw.parse::<i32>().ok().filter(|pid| *pid > 0);
            leader_pid = Some(pid.ok_or_else(|| ListParseError::InvalidLeaderPid(raw.to_owned()))?);
        } else if let Some(raw) = field.strip_prefix("created=") {
            created_at = raw
                .parse::<i64>()
                .ok()
                .and_then(|value| u64::try_from(value).ok())
                .filter(|value| *value > 0);
        } else if field.starts_with("err=") {
            has_error = true;
        }
    }
    let name = name
        .filter(|name| !name.is_empty())
        .ok_or(ListParseError::MissingName)?;
    if clients.is_none() && !has_error {
        return Err(ListParseError::MissingClients(name.to_owned()));
    }
    Ok(LiveSessionRecord {
        name: name.to_owned(),
        clients,
        leader_pid,
        created_at,
    })
}

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
    use crate::zmx::{NAME_PREFIX, daemon_name};

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
    fn the_parser_keeps_client_counts_and_unreachable_names() {
        let output = "name=agterm-a\tpid=10\tclients=0\tcreated=1\tcwd=/tmp\n\
                      → name=agterm-b\tpid=11\tclients=2\tcreated=1\tcmd=zsh\r\n\
                      name=agterm-busy\terr=Timeout\tstatus=unreachable\n";
        assert_eq!(
            parse_list(output).unwrap(),
            [
                record("agterm-a", Some(0), Some(10), Some(1)),
                record("agterm-b", Some(2), Some(11), Some(1)),
                record("agterm-busy", None, None, None),
            ]
        );
    }

    #[test]
    fn the_parser_reads_the_creation_time_without_ever_failing_on_it() {
        for (field, expected) in [
            ("\tcreated=1790000000", Some(1_790_000_000)),
            ("", None),
            ("\tcreated=0", None),
            ("\tcreated=wat", None),
            ("\tcreated=-5", None),
        ] {
            assert_eq!(
                parse_list(&format!("name=agterm-a\tpid=10\tclients=0{field}\n")).unwrap(),
                [record("agterm-a", Some(0), Some(10), expected)],
                "{field}"
            );
        }
    }

    #[test]
    fn the_parser_refuses_partial_output_instead_of_reaping_from_it() {
        assert_eq!(parse_list(""), Ok(Vec::new()));
        assert_eq!(parse_list("pid=10\tclients=0\n"), Err(ListParseError::MissingName));
        assert_eq!(
            parse_list("name=agterm-a\tclients=wat\n"),
            Err(ListParseError::InvalidClients("wat".to_owned()))
        );
        assert_eq!(
            parse_list("name=agterm-a\tclients=-1\n"),
            Err(ListParseError::InvalidClients("-1".to_owned()))
        );
        assert_eq!(
            parse_list("name=agterm-a\tpid=wat\tclients=0\n"),
            Err(ListParseError::InvalidLeaderPid("wat".to_owned()))
        );
        assert_eq!(
            parse_list("name=agterm-a\tpid=0\tclients=0\n"),
            Err(ListParseError::InvalidLeaderPid("0".to_owned()))
        );
        assert_eq!(
            parse_list("name=agterm-a\tpid=1\n"),
            Err(ListParseError::MissingClients("agterm-a".to_owned()))
        );
    }

    #[test]
    fn the_leader_map_keeps_only_generated_names_with_a_readable_pid() {
        let (app, no_pid, unreachable) = (fresh(), fresh(), fresh());
        let records = parse_list(&format!(
            "name={app}\tpid=10\tclients=0\nname=other\tpid=11\tclients=0\nname=agterm-notes\tpid=12\tclients=0\n\
             name={no_pid}\tclients=0\nname={unreachable}\terr=Timeout\tstatus=unreachable\n"
        ))
        .unwrap();
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
