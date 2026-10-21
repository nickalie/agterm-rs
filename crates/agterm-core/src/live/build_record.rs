//! When this state directory first launched the session server build it names (upstream's `ZmxBuildRecord.swift`,
//! there the bundled zmx's). Live sessions keep running through app updates, so a session created before the recorded
//! change predates that launch and Reset Live Sessions offers it as outdated. The bundled build id is the one staged
//! beside the `agterm-session` binary (`share/ngterm/session-BUILD`, `scripts/stage.sh`), as
//! `agterm_session::build_id` reads it.
//!
//! Unlike upstream's zmx, the session server hands its sessions over to a server of the new build (its update
//! handoff): once that handoff committed ([`record_handoff`]), the sessions created before the launch run in the
//! bundled build, and the outdated reason is off for that build ([`launch_cutoff`] gives no cutoff). A handoff that
//! failed records nothing, so the sessions stay outdated and the reset offer stands.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Deserializer, Serialize};

use crate::persist::json::write_atomic;

/// The record's file name in the state directory (upstream's `zmx-build.json`, named for the session server here).
pub const FILE_NAME: &str = "session-build.json";

/// The build id and when this state directory first launched it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRecord {
    pub id: String,
    /// Whole seconds since the Unix epoch, the resolution of a session's `created` time.
    #[serde(rename = "changedAt", deserialize_with = "whole_seconds")]
    pub changed_at: u64,
    /// The sessions of the earlier build were handed over to a server of this one (an update handoff committed), so
    /// none predates it. Absent from records written before handoffs existed, which read as `false`.
    #[serde(rename = "handedOver", default, skip_serializing_if = "std::ops::Not::not")]
    pub handed_over: bool,
}

/// A `secondsSince1970` date, floored to whole seconds; a negative one reads as the epoch.
fn whole_seconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let seconds = f64::deserialize(deserializer)?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a float-to-int `as` saturates: a negative or NaN date reads as the epoch, a huge one as u64::MAX"
    )]
    Ok(seconds.floor() as u64)
}

impl BuildRecord {
    /// The record after a launch of `bundled_id` at `now` (whole seconds): `previous` kept for the same build, a new
    /// one dated `now` otherwise.
    #[must_use]
    pub fn advanced(previous: Option<Self>, bundled_id: &str, now: u64) -> Self {
        match previous {
            Some(previous) if previous.id == bundled_id => previous,
            _ => Self {
                id: bundled_id.to_owned(),
                changed_at: now,
                handed_over: false,
            },
        }
    }

    /// The record in `file`; `None` when absent or unreadable.
    #[must_use]
    pub fn load(file: &Path) -> Option<Self> {
        serde_json::from_slice(&fs::read(file).ok()?).ok()
    }
}

/// This launch's outdated cutoff in whole seconds: `None` without a bundled id, which turns the outdated reset reason
/// off, and `None` once the sessions were handed over to the bundled build ([`record_handoff`]). The record is saved
/// when it changed; one that cannot be saved still yields this launch's cutoff, and the next launch dates the change
/// again.
#[must_use]
pub fn launch_cutoff(bundled_id: Option<&str>, directory: &Path, now: SystemTime) -> Option<u64> {
    let record = advance(bundled_id, directory, now, false)?;
    (!record.handed_over).then_some(record.changed_at)
}

/// A handoff of the live sessions to a server of `bundled_id` committed: the record of that build (dated `now` when
/// it is new) says so, and [`launch_cutoff`] gives no cutoff for it from then on. Nothing without a bundled id. Best
/// effort, as the cutoff's own save: a record that cannot be saved leaves the sessions outdated, so the reset offer
/// errs on the side of being made.
pub fn record_handoff(bundled_id: Option<&str>, directory: &Path, now: SystemTime) {
    let _ = advance(bundled_id, directory, now, true);
}

/// The record for a launch of `bundled_id` at `now`, saved when it changed; `handed_over` marks it handed over.
fn advance(bundled_id: Option<&str>, directory: &Path, now: SystemTime, handed_over: bool) -> Option<BuildRecord> {
    let bundled_id = bundled_id.map(str::trim).filter(|id| !id.is_empty())?;
    let file = directory.join(FILE_NAME);
    let previous = BuildRecord::load(&file);
    let now = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let mut record = BuildRecord::advanced(previous.clone(), bundled_id, now);
    record.handed_over |= handed_over;
    if previous.as_ref() != Some(&record)
        && let Ok(bytes) = serde_json::to_vec(&record)
    {
        let _ = write_atomic(&file, &bytes);
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(seconds: f64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs_f64(seconds)
    }

    fn record(id: &str, changed_at: u64) -> BuildRecord {
        BuildRecord {
            id: id.to_owned(),
            changed_at,
            handed_over: false,
        }
    }

    #[test]
    fn advanced_keeps_the_record_for_the_same_build_and_dates_a_new_one() {
        assert_eq!(
            BuildRecord::advanced(None, "rev target digest", 1000),
            record("rev target digest", 1000)
        );
        assert_eq!(
            BuildRecord::advanced(Some(record("a", 500)), "a", 9000),
            record("a", 500)
        );
        assert_eq!(
            BuildRecord::advanced(Some(record("a", 500)), "b", 9000),
            record("b", 9000)
        );
    }

    #[test]
    fn the_cutoff_records_the_first_launch_and_keeps_it_across_relaunches() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(launch_cutoff(Some("a\n"), dir.path(), at(1000.5)), Some(1000));
        assert_eq!(launch_cutoff(Some("a"), dir.path(), at(5000.0)), Some(1000));
        assert_eq!(launch_cutoff(Some("b"), dir.path(), at(7000.0)), Some(7000));
        let file = dir.path().join(FILE_NAME);
        assert_eq!(BuildRecord::load(&file), Some(record("b", 7000)));
        assert_eq!(fs::read_to_string(&file).unwrap(), r#"{"id":"b","changedAt":7000}"#);
    }

    #[test]
    fn an_unreadable_record_reads_as_absent_and_a_fractional_date_as_whole_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE_NAME);
        fs::write(&file, "not json").unwrap();
        assert_eq!(launch_cutoff(Some("a"), dir.path(), at(3000.0)), Some(3000));
        fs::write(&file, r#"{"id":"a","changedAt":1234.9}"#).unwrap();
        assert_eq!(launch_cutoff(Some("a"), dir.path(), at(3000.0)), Some(1234));
    }

    #[test]
    fn a_committed_handoff_turns_the_cutoff_off_for_its_build_only() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE_NAME);
        assert_eq!(launch_cutoff(Some("a"), dir.path(), at(1000.0)), Some(1000));
        // build b launches: its sessions predate it until they are handed over
        assert_eq!(launch_cutoff(Some("b"), dir.path(), at(2000.0)), Some(2000));
        record_handoff(Some("b"), dir.path(), at(2001.0));
        assert_eq!(launch_cutoff(Some("b"), dir.path(), at(3000.0)), None);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            r#"{"id":"b","changedAt":2000,"handedOver":true}"#
        );
        // the next build starts outdated again, until its own handoff
        assert_eq!(launch_cutoff(Some("c"), dir.path(), at(4000.0)), Some(4000));
        assert!(!BuildRecord::load(&file).unwrap().handed_over);
        // a handoff to a build never launched here dates its record too
        record_handoff(Some("d"), dir.path(), at(5000.0));
        assert_eq!(
            BuildRecord::load(&file),
            Some(BuildRecord {
                id: "d".to_owned(),
                changed_at: 5000,
                handed_over: true
            })
        );
        // without a bundled id nothing is recorded
        record_handoff(None, dir.path(), at(6000.0));
        assert_eq!(BuildRecord::load(&file).unwrap().id, "d");
    }

    #[test]
    fn the_cutoff_is_none_without_a_bundled_id() {
        for id in [None, Some(""), Some(" \n")] {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(launch_cutoff(id, dir.path(), SystemTime::now()), None);
            assert!(!dir.path().join(FILE_NAME).exists());
        }
    }
}
