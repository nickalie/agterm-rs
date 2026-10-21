//! Reset Live Sessions and `live.reset` (upstream's `LiveReset.swift`): which panes the dialog offers, the confirmed
//! set written at quit and consumed once at the next launch, its re-check against that launch's claims and listing, and
//! the report of what the kill did. Host-free: the app joins claims to daemons and kills; this decides which and
//! reports what happened.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agterm_host::LiveSessionRecord;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::daemon_name;
use super::inventory::ClaimWalk;
use super::lifecycle::leaders;
use crate::persist::RestoreMode;
use crate::persist::json::write_atomic;
use crate::persist::upper;

/// Who answers for a daemon's leader (upstream's `SessionHost.Attribution`): a supervising session host, the app
/// itself, nobody (orphaned), or unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::IntoStaticStr)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "camelCase")]
pub enum Attribution {
    Supervisor,
    App,
    Orphaned,
    Unknown,
}

/// What the process responsible for a leader is (upstream's `SessionHost.ResponsibleProcess`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsibleProcess {
    Live(i32),
    Dead,
    Unknown,
}

impl Attribution {
    /// The attribution of `leader` from its responsible process (upstream's `SessionHost.classify`): a leader that is
    /// its own responsible process, or whose responsible process died, is orphaned.
    #[must_use]
    pub fn classify(
        leader: Option<i32>,
        responsible: Option<ResponsibleProcess>,
        host_pid: Option<i32>,
        app_pid: Option<i32>,
    ) -> Self {
        let Some(leader) = leader.filter(|pid| *pid > 0) else {
            return Self::Unknown;
        };
        match responsible {
            Some(ResponsibleProcess::Dead) => Self::Orphaned,
            Some(ResponsibleProcess::Live(pid)) if pid > 0 => {
                if pid == leader {
                    Self::Orphaned
                } else if Some(pid) == host_pid {
                    Self::Supervisor
                } else if Some(pid) == app_pid {
                    Self::App
                } else {
                    Self::Unknown
                }
            }
            _ => Self::Unknown,
        }
    }
}

/// Why a pane was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    /// Its leader is orphaned or attributed to the app.
    #[default]
    Unsupervised,
    /// Its daemon was created before this state directory's recorded session server build change.
    Outdated,
}

/// One pane confirmed for reset, with the leader observed at confirmation so the next launch can tell the same daemon
/// from a replacement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Target {
    #[serde(rename = "paneIdentity", serialize_with = "upper::uuid")]
    pub pane_identity: Uuid,
    #[serde(rename = "sessionID", serialize_with = "upper::uuid")]
    pub session_id: Uuid,
    pub daemon: String,
    #[serde(rename = "leaderPID")]
    pub leader_pid: i32,
    /// A version-1 target has none and reads as unsupervised.
    #[serde(default)]
    pub reason: Reason,
}

/// The confirmed set written at quit and consumed once at the next launch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub version: i64,
    /// Seconds since the Unix epoch, as Foundation's `secondsSince1970` date strategy writes them.
    #[serde(rename = "createdAt")]
    pub created_at: f64,
    pub targets: Vec<Target>,
}

impl Marker {
    pub const CURRENT_VERSION: i64 = 2;
    /// What [`MarkerStore::consume`] accepts: a reset confirmed under the previous version still runs after an update.
    pub const SUPPORTED_VERSIONS: [i64; 2] = [1, 2];

    /// A current-version marker of `targets`, created at `created_at`.
    #[must_use]
    pub fn new(targets: Vec<Target>, created_at: SystemTime) -> Self {
        let created_at = created_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0.0, |since| since.as_secs_f64());
        Self {
            version: Self::CURRENT_VERSION,
            created_at,
            targets,
        }
    }
}

/// What the dialog offers. An incomplete walk, or one pane claimed twice, forbids the action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub targets: Vec<Target>,
    pub inventory_complete: bool,
}

impl Selection {
    /// Distinct sessions among the targets: a split session counts once.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.targets
            .iter()
            .map(|target| target.session_id)
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// Distinct sessions among the outdated targets.
    #[must_use]
    pub fn outdated_session_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|target| target.reason == Reason::Outdated)
            .map(|target| target.session_id)
            .collect::<BTreeSet<_>>()
            .len()
    }
}

/// The panes to offer: one created before `outdated_before` as outdated whatever its attribution, else an orphaned or
/// app-attributed one as unsupervised; a pane without a readable leader is never offered. `None` cutoff selects no
/// outdated pane. `classify` attributes a daemon's leader.
#[must_use]
pub fn select(
    claims: &ClaimWalk,
    records: &[LiveSessionRecord],
    outdated_before: Option<u64>,
    classify: impl Fn(&str, i32) -> Attribution,
) -> Selection {
    let leaders = leaders(records);
    let created = created_times(records);
    let mut seen = BTreeSet::new();
    let mut conflicted = false;
    let mut targets = Vec::new();
    for claim in &claims.claims {
        if !seen.insert(claim.pane_identity) {
            conflicted = true;
            continue;
        }
        let name = daemon_name(claim.pane_identity);
        let Some(&leader) = leaders.get(&name) else {
            continue;
        };
        let reason = if is_outdated(created.get(&name).copied(), outdated_before) {
            Reason::Outdated
        } else if matches!(classify(&name, leader), Attribution::Orphaned | Attribution::App) {
            Reason::Unsupervised
        } else {
            continue;
        };
        targets.push(Target {
            pane_identity: claim.pane_identity,
            session_id: claim.session_id,
            daemon: name,
            leader_pid: leader,
            reason,
        });
    }
    Selection {
        targets,
        inventory_complete: claims.complete && !conflicted,
    }
}

/// Whether a daemon created at `created` predates `cutoff`; never without both.
#[must_use]
pub fn is_outdated(created: Option<u64>, cutoff: Option<u64>) -> bool {
    matches!((created, cutoff), (Some(created), Some(cutoff)) if created < cutoff)
}

fn created_times(records: &[LiveSessionRecord]) -> BTreeMap<String, u64> {
    let mut created = BTreeMap::new();
    for record in records {
        if let Some(at) = record.created_at {
            created.entry(record.name.clone()).or_insert(at);
        }
    }
    created
}

/// What the next launch does with one marker target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Disposition {
    Kill,
    Gone,
    Skipped,
}

/// The marker re-checked against this launch's claims and listing; it only narrows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Narrowed {
    pub dispositions: BTreeMap<Target, Disposition>,
    pub inventory_failed: bool,
}

impl Narrowed {
    /// The targets to kill, by daemon name.
    #[must_use]
    pub fn kill(&self) -> Vec<Target> {
        let mut kill: Vec<Target> = self
            .dispositions
            .iter()
            .filter(|(_, disposition)| **disposition == Disposition::Kill)
            .map(|(target, _)| target.clone())
            .collect();
        kill.sort_by(|a, b| a.daemon.cmp(&b.daemon));
        kill
    }
}

/// Re-checks `marker`: an outdated target still created before `outdated_before`, and an unsupervised one still
/// orphaned, are killed when still claimed and listed with the same leader; a target whose daemon is gone is `gone`;
/// everything else is skipped. Without a listing or the claims nothing is killed (`records == None` is an inventory
/// failure).
#[must_use]
pub fn narrow(
    marker: &Marker,
    claimed: Option<&BTreeSet<Uuid>>,
    records: Option<&[LiveSessionRecord]>,
    outdated_before: Option<u64>,
    classify: impl Fn(&str, i32) -> Attribution,
) -> Narrowed {
    let (Some(records), Some(claimed)) = (records, claimed) else {
        return Narrowed {
            dispositions: marker
                .targets
                .iter()
                .map(|target| (target.clone(), Disposition::Skipped))
                .collect(),
            inventory_failed: records.is_none(),
        };
    };
    let mut by_name: BTreeMap<&str, &LiveSessionRecord> = BTreeMap::new();
    for record in records {
        by_name.entry(record.name.as_str()).or_insert(record);
    }
    let dispositions = marker
        .targets
        .iter()
        .map(|target| {
            let disposition = if !claimed.contains(&target.pane_identity) {
                Disposition::Skipped
            } else if let Some(record) = by_name.get(target.daemon.as_str()) {
                let qualifies = record.leader_pid == Some(target.leader_pid)
                    && match target.reason {
                        Reason::Outdated => is_outdated(record.created_at, outdated_before),
                        Reason::Unsupervised => classify(&target.daemon, target.leader_pid) == Attribution::Orphaned,
                    };
                if qualifies {
                    Disposition::Kill
                } else {
                    Disposition::Skipped
                }
            } else {
                Disposition::Gone
            };
            (target.clone(), disposition)
        })
        .collect();
    Narrowed {
        dispositions,
        inventory_failed: false,
    }
}

/// Pane counts of a reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PaneCounts {
    pub confirmed: usize,
    pub killed: usize,
    pub gone: usize,
    pub skipped: usize,
}

/// Distinct sessions among the targets. A session is reset only when every one of its panes was killed and confirmed
/// gone or had no daemon; any other pane makes it partial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SessionCounts {
    pub affected: usize,
    pub reset: usize,
    pub partial: usize,
    pub unconfirmed: usize,
}

/// What a reset did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub panes: PaneCounts,
    /// Panes whose old leader survived the kill: their launch payloads must be suppressed.
    #[serde(serialize_with = "upper_vec")]
    pub unconfirmed: Vec<Uuid>,
    pub sessions: SessionCounts,
    pub inventory_failed: bool,
}

fn upper_vec<S: serde::Serializer>(ids: &[Uuid], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(ids.iter().map(|id| crate::pane_env::uuid_string(*id)))
}

/// The outcome of `narrowed` given the leader pids still alive after the kill and the poll (`survivors`).
#[must_use]
pub fn outcome(narrowed: &Narrowed, survivors: &BTreeSet<i32>, inventory_failed: bool) -> Outcome {
    let (mut killed, mut gone, mut skipped) = (0, 0, 0);
    let mut unconfirmed = Vec::new();
    let mut reset = BTreeSet::new();
    let mut partial = BTreeSet::new();
    let mut unconfirmed_sessions = BTreeSet::new();
    let mut entries: Vec<_> = narrowed.dispositions.iter().collect();
    entries.sort_by(|a, b| a.0.daemon.cmp(&b.0.daemon));
    for (target, disposition) in entries {
        match disposition {
            Disposition::Gone => {
                gone += 1;
                reset.insert(target.session_id);
            }
            Disposition::Skipped => {
                skipped += 1;
                partial.insert(target.session_id);
            }
            Disposition::Kill if survivors.contains(&target.leader_pid) => {
                unconfirmed.push(target.pane_identity);
                partial.insert(target.session_id);
                unconfirmed_sessions.insert(target.session_id);
            }
            Disposition::Kill => {
                killed += 1;
                reset.insert(target.session_id);
            }
        }
    }
    reset.retain(|session| !partial.contains(session));
    Outcome {
        panes: PaneCounts {
            confirmed: narrowed.dispositions.len(),
            killed,
            gone,
            skipped,
        },
        unconfirmed,
        sessions: SessionCounts {
            affected: reset.len() + partial.len(),
            reset: reset.len(),
            partial: partial.len(),
            unconfirmed: unconfirmed_sessions.len(),
        },
        inventory_failed,
    }
}

/// The marker's file name in the state directory.
pub const MARKER_FILE_NAME: &str = "live-reset.json";
/// The marker's name once consumed.
pub const CONSUMED_FILE_NAME: &str = "live-reset.consumed.json";

/// The confirmation dialog's title and body.
#[must_use]
pub fn dialog_text(session_count: usize, outdated_sessions: usize) -> (String, String) {
    let noun = if session_count == 1 {
        "live session"
    } else {
        "live sessions"
    };
    let body = format!(
        "{session_count} {noun} will be reset. {}Agterm quits and reopens itself right away with your sessions and \
         layout. Commands that were running in those sessions are started again where possible; other work running \
         in them stops, and agent conversations may need to be resumed by hand.",
        outdated_sentence(outdated_sessions, session_count)
    );
    ("Reset Live Sessions?".to_owned(), body)
}

fn outdated_sentence(outdated: usize, total: usize) -> String {
    if outdated == 0 {
        return String::new();
    }
    let tail = "the last Live sessions update and will be recreated on the current one. ";
    let head = match (outdated == total, total == 1, outdated == 1) {
        (true, true, _) => "It predates ".to_owned(),
        (true, false, _) => "They all predate ".to_owned(),
        (false, _, true) => "1 of them predates ".to_owned(),
        (false, _, false) => format!("{outdated} of them predate "),
    };
    head + tail
}

/// The notification after a reset; `None` when every session was reset.
#[must_use]
pub fn notification_text(outcome: &Outcome) -> Option<String> {
    if outcome.inventory_failed {
        return Some("Live sessions were not reset: the session list could not be read.".to_owned());
    }
    let sessions = outcome.sessions;
    if sessions.partial == 0 {
        return None;
    }
    let survivors = if sessions.unconfirmed > 0 {
        let noun = if sessions.unconfirmed == 1 {
            "session"
        } else {
            "sessions"
        };
        format!(
            " Some previous processes in {} {noun} may still be running; those commands were not restarted.",
            sessions.unconfirmed
        )
    } else {
        String::new()
    };
    Some(format!(
        "The reset covered {} of {} live sessions. Run Agterm ▸ Reset Live Sessions… again for the rest.{survivors}",
        sessions.reset, sessions.affected
    ))
}

/// Whether Reset Live Sessions… is offered: only when live is both configured and running.
#[must_use]
pub fn menu_visible(configured: RestoreMode, active: RestoreMode) -> bool {
    configured == RestoreMode::Live && active == RestoreMode::Live
}

/// Why a marker could not be consumed.
#[derive(Debug, thiserror::Error)]
pub enum MarkerError {
    /// The marker did not decode as a supported version; it was removed.
    #[error("the live reset marker is invalid")]
    Invalid,
    /// The rename failed: the reset is not authorized.
    #[error("the live reset marker could not be consumed: {0}")]
    Io(#[from] io::Error),
}

/// The one-shot marker on disk. `consume` renames before decoding, so a crash mid-reset never replays it.
#[derive(Debug, Clone)]
pub struct MarkerStore {
    marker: PathBuf,
    consumed: PathBuf,
}

impl MarkerStore {
    #[must_use]
    pub fn new(directory: &Path) -> Self {
        Self {
            marker: directory.join(MARKER_FILE_NAME),
            consumed: directory.join(CONSUMED_FILE_NAME),
        }
    }

    /// Writes `marker` atomically.
    ///
    /// # Errors
    ///
    /// The encode or the write failed.
    pub fn write(&self, marker: &Marker) -> io::Result<()> {
        write_atomic(&self.marker, &serde_json::to_vec(marker).map_err(io::Error::other)?)
    }

    /// Takes the marker: `None` when there is none.
    ///
    /// # Errors
    ///
    /// [`MarkerError::Invalid`], after removing the file, for anything that does not decode as a supported version;
    /// [`MarkerError::Io`] when the rename failed, and the caller must then treat the reset as not authorized.
    pub fn consume(&self) -> Result<Option<Marker>, MarkerError> {
        if !self.marker.exists() {
            return Ok(None);
        }
        if self.consumed.exists() {
            fs::remove_file(&self.consumed)?;
        }
        fs::rename(&self.marker, &self.consumed)?;
        let decoded = fs::read(&self.consumed)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok())
            .filter(|marker| Marker::SUPPORTED_VERSIONS.contains(&marker.version));
        decoded.map(Some).ok_or_else(|| {
            let _ = fs::remove_file(&self.consumed);
            MarkerError::Invalid
        })
    }

    /// Removes the consumed marker.
    pub fn remove_consumed(&self) {
        let _ = fs::remove_file(&self.consumed);
    }

    /// Removes both files.
    pub fn remove(&self) {
        let _ = fs::remove_file(&self.marker);
        self.remove_consumed();
    }
}

#[cfg(test)]
mod tests;
