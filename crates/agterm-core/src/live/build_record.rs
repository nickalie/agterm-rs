//! When this state directory first launched the zmx build it names (upstream's `ZmxBuildRecord.swift`). Live sessions
//! keep their daemons through app updates, so a daemon created before the recorded change predates that launch and
//! Reset Live Sessions offers it as outdated. The bundled build id is the staged `share/agterm/zmx-BUILD`
//! (`scripts/build-zmx.sh`).

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Deserializer, Serialize};

use crate::persist::json::write_atomic;

/// The record's file name in the state directory.
pub const FILE_NAME: &str = "zmx-build.json";

/// The build id and when this state directory first launched it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRecord {
    pub id: String,
    /// Whole seconds since the Unix epoch, the resolution of zmx's `created` time.
    #[serde(rename = "changedAt", deserialize_with = "whole_seconds")]
    pub changed_at: u64,
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
/// off. The record is saved when it changed; one that cannot be saved still yields this launch's cutoff, and the next
/// launch dates the change again.
#[must_use]
pub fn launch_cutoff(bundled_id: Option<&str>, directory: &Path, now: SystemTime) -> Option<u64> {
    let bundled_id = bundled_id.map(str::trim).filter(|id| !id.is_empty())?;
    let file = directory.join(FILE_NAME);
    let previous = BuildRecord::load(&file);
    let now = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let record = BuildRecord::advanced(previous.clone(), bundled_id, now);
    if previous.as_ref() != Some(&record)
        && let Ok(bytes) = serde_json::to_vec(&record)
    {
        let _ = write_atomic(&file, &bytes);
    }
    Some(record.changed_at)
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
    fn the_cutoff_is_none_without_a_bundled_id() {
        for id in [None, Some(""), Some(" \n")] {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(launch_cutoff(id, dir.path(), SystemTime::now()), None);
            assert!(!dir.path().join(FILE_NAME).exists());
        }
    }
}
