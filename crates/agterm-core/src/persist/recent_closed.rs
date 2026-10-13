//! Open Recent on disk (upstream's `RecentClosed.swift`): `recent-closed.json` in the state directory, the closed
//! sessions and workspaces newest first, at most [`RECENT_CLOSED_LIMIT`] of them.
//!
//! The item types decode as upstream's synthesized `Codable` does: optionals may be absent or `null`, unknown keys
//! are ignored, anything else malformed fails the document. [`RecentClosedStore::load`] reads a failed decode, a
//! version other than [`RECENT_CLOSED_VERSION`] or a missing file as an empty list. `closedAt` is Foundation's
//! default `Date` encoding: seconds since 2001-01-01 00:00 UTC as a JSON number.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::snapshot::{SessionSnapshot, WorkspaceSnapshot};
use super::{json, upper};

/// The list's file name in the state directory.
pub const RECENT_CLOSED_FILE_NAME: &str = "recent-closed.json";
/// How many entries the list keeps (upstream's default `limit`).
pub const RECENT_CLOSED_LIMIT: usize = 20;
/// The file shape's version; any other reads as an empty list.
pub const RECENT_CLOSED_VERSION: i64 = 1;

/// What a closed entry brings back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecentClosedKind {
    Session,
    Workspace,
}

/// One Open Recent entry (upstream's `RecentClosedItem`). A session entry carries `session`, a workspace entry
/// `workspace`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentClosedItem {
    #[serde(serialize_with = "upper::uuid")]
    pub id: Uuid,
    pub kind: RecentClosedKind,
    /// The menu title: the session's display name, or the workspace's name.
    pub title: String,
    /// The session's workspace name, or "N session(s)" for a workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(with = "reference_date")]
    pub closed_at: SystemTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<RecentClosedSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<RecentClosedWorkspace>,
}

/// A closed session and where it sat (upstream's `RecentClosedSession`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentClosedSession {
    #[serde(rename = "workspaceID", serialize_with = "upper::uuid")]
    pub workspace_id: Uuid,
    pub workspace_name: String,
    pub workspace_index: usize,
    pub session_index: usize,
    pub snapshot: SessionSnapshot,
}

/// A closed workspace (upstream's `RecentClosedWorkspace`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentClosedWorkspace {
    pub snapshot: WorkspaceSnapshot,
    /// The selection a reopen restores, only when the snapshot holds it.
    #[serde(
        default,
        rename = "selectedSessionID",
        serialize_with = "upper::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub selected_session_id: Option<Uuid>,
    /// Whether the workspace was in the sidebar focus set when it closed; absent in entries older than the field,
    /// which read as not a member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_member: Option<bool>,
}

/// The file: its version and the entries, newest first (upstream's `RecentClosedState`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentClosedState {
    pub version: i64,
    pub items: Vec<RecentClosedItem>,
}

impl RecentClosedState {
    /// The current version with `items`.
    #[must_use]
    pub fn new(items: Vec<RecentClosedItem>) -> Self {
        Self {
            version: RECENT_CLOSED_VERSION,
            items,
        }
    }
}

impl RecentClosedItem {
    /// Whether `self` names what `other` names: the same entry, or a snapshot of the same session or workspace.
    /// Recording `other` drops every entry it supersedes.
    #[must_use]
    pub fn is_superseded_by(&self, other: &Self) -> bool {
        if self.id == other.id {
            return true;
        }
        match (self.kind, other.kind) {
            (RecentClosedKind::Session, RecentClosedKind::Session) => {
                self.session.as_ref().map(|session| session.snapshot.id)
                    == other.session.as_ref().map(|session| session.snapshot.id)
            }
            (RecentClosedKind::Workspace, RecentClosedKind::Workspace) => {
                self.workspace.as_ref().map(|workspace| workspace.snapshot.id)
                    == other.workspace.as_ref().map(|workspace| workspace.snapshot.id)
            }
            _ => false,
        }
    }
}

/// Records `item` at the front of a newest-first list, dropping the entries it supersedes, and keeps at most `limit`
/// (at least one). The one rule both the file and the model's in-memory list follow.
pub fn record_recent_closed(items: &mut Vec<RecentClosedItem>, item: RecentClosedItem, limit: usize) {
    items.retain(|existing| !existing.is_superseded_by(&item));
    items.insert(0, item);
    items.truncate(limit.max(1));
}

/// Reads and writes `recent-closed.json` (upstream's `RecentClosedStore`). Every change loads the file, applies
/// itself and writes the result back atomically.
#[derive(Debug, Clone)]
pub struct RecentClosedStore {
    path: PathBuf,
    limit: usize,
}

impl RecentClosedStore {
    /// The store of `directory`'s `recent-closed.json`, keeping [`RECENT_CLOSED_LIMIT`] entries.
    #[must_use]
    pub fn new(directory: impl AsRef<Path>) -> Self {
        Self::with_limit(directory, RECENT_CLOSED_LIMIT)
    }

    /// The same store keeping `limit` entries, at least one.
    #[must_use]
    pub fn with_limit(directory: impl AsRef<Path>, limit: usize) -> Self {
        Self {
            path: directory.as_ref().join(RECENT_CLOSED_FILE_NAME),
            limit: limit.max(1),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The entries, newest first, at most the limit; empty when the file is missing, unreadable, undecodable or of
    /// another version.
    #[must_use]
    pub fn load(&self) -> Vec<RecentClosedItem> {
        let Ok(data) = std::fs::read(&self.path) else {
            return Vec::new();
        };
        let Ok(state) = serde_json::from_slice::<RecentClosedState>(&data) else {
            return Vec::new();
        };
        if state.version != RECENT_CLOSED_VERSION {
            return Vec::new();
        }
        let mut items = state.items;
        items.truncate(self.limit);
        items
    }

    /// Records `item` as the newest entry, dropping the entries it supersedes.
    ///
    /// # Errors
    ///
    /// The directory could not be created or the file not written.
    pub fn record(&self, item: RecentClosedItem) -> io::Result<()> {
        let mut items = self.load();
        record_recent_closed(&mut items, item, self.limit);
        self.save(items)
    }

    /// Removes the entry `id`.
    ///
    /// # Errors
    ///
    /// The directory could not be created or the file not written.
    pub fn remove(&self, id: Uuid) -> io::Result<()> {
        let mut items = self.load();
        items.retain(|item| item.id != id);
        self.save(items)
    }

    /// Empties the list.
    ///
    /// # Errors
    ///
    /// The directory could not be created or the file not written.
    pub fn clear(&self) -> io::Result<()> {
        self.save(Vec::new())
    }

    fn save(&self, items: Vec<RecentClosedItem>) -> io::Result<()> {
        json::save_json(&self.path, &RecentClosedState::new(items))
    }
}

/// Foundation's default `Date` coding: a `Double` of seconds since the reference date, 2001-01-01 00:00 UTC.
pub mod reference_date {
    use super::{Duration, SystemTime, UNIX_EPOCH};
    use serde::{Deserialize, Deserializer, Serializer};

    /// The reference date's distance from the Unix epoch: 978 307 200 s.
    const REFERENCE_OFFSET: Duration = Duration::from_hours(271_752);

    fn reference() -> SystemTime {
        UNIX_EPOCH + REFERENCE_OFFSET
    }

    /// `time` as seconds since the reference date, negative before it.
    #[must_use]
    pub fn seconds(time: SystemTime) -> f64 {
        match time.duration_since(reference()) {
            Ok(after) => after.as_secs_f64(),
            Err(before) => -before.duration().as_secs_f64(),
        }
    }

    /// Now, at the precision a `Date` holds, so an entry reads back from disk equal to the one recorded.
    #[must_use]
    pub fn now() -> SystemTime {
        let now = SystemTime::now();
        time(seconds(now)).unwrap_or(now)
    }

    /// The time `seconds` after the reference date; `None` for a value no `SystemTime` holds.
    #[must_use]
    pub fn time(seconds: f64) -> Option<SystemTime> {
        let magnitude = Duration::try_from_secs_f64(seconds.abs()).ok()?;
        if seconds.is_sign_negative() {
            reference().checked_sub(magnitude)
        } else {
            reference().checked_add(magnitude)
        }
    }

    pub(crate) fn serialize<S: Serializer>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(seconds(*time))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SystemTime, D::Error> {
        let seconds = f64::deserialize(deserializer)?;
        time(seconds).ok_or_else(|| serde::de::Error::custom(format!("date out of range: {seconds}")))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn session_item(snapshot_id: Uuid, title: &str) -> RecentClosedItem {
        RecentClosedItem {
            id: Uuid::new_v4(),
            kind: RecentClosedKind::Session,
            title: title.to_owned(),
            subtitle: Some("work".to_owned()),
            closed_at: SystemTime::now(),
            session: Some(RecentClosedSession {
                workspace_id: Uuid::new_v4(),
                workspace_name: "work".to_owned(),
                workspace_index: 0,
                session_index: 0,
                snapshot: SessionSnapshot {
                    custom_name: Some(title.to_owned()),
                    ..SessionSnapshot::new(snapshot_id, "/tmp")
                },
            }),
            workspace: None,
        }
    }

    fn workspace_item(snapshot_id: Uuid, title: &str) -> RecentClosedItem {
        RecentClosedItem {
            id: Uuid::new_v4(),
            kind: RecentClosedKind::Workspace,
            title: title.to_owned(),
            subtitle: Some("1 session".to_owned()),
            closed_at: SystemTime::now(),
            session: None,
            workspace: Some(RecentClosedWorkspace {
                snapshot: WorkspaceSnapshot {
                    id: snapshot_id,
                    name: title.to_owned(),
                    sessions: vec![SessionSnapshot {
                        custom_name: Some("api".to_owned()),
                        ..SessionSnapshot::new(Uuid::new_v4(), "/tmp")
                    }],
                    collapsed: None,
                },
                selected_session_id: None,
                focus_member: None,
            }),
        }
    }

    fn titles(items: &[RecentClosedItem]) -> Vec<&str> {
        items.iter().map(|item| item.title.as_str()).collect()
    }

    #[test]
    fn a_disk_round_trip_keeps_the_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecentClosedStore::new(dir.path());
        let first = session_item(Uuid::new_v4(), "first");
        let second = workspace_item(Uuid::new_v4(), "workspace");
        store.record(first.clone()).unwrap();
        store.record(second.clone()).unwrap();

        let loaded = RecentClosedStore::new(dir.path()).load();
        assert_eq!(
            loaded.iter().map(|item| item.id).collect::<Vec<_>>(),
            [second.id, first.id]
        );
        assert_eq!(loaded[0].workspace.as_ref().unwrap().snapshot.name, "workspace");
        assert_eq!(
            loaded[1].session.as_ref().unwrap().snapshot.custom_name.as_deref(),
            Some("first")
        );
    }

    #[test]
    fn record_dedupes_sessions_and_workspaces_by_snapshot_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecentClosedStore::new(dir.path());
        let (session, workspace) = (Uuid::new_v4(), Uuid::new_v4());
        store.record(session_item(session, "old session")).unwrap();
        store.record(session_item(session, "new session")).unwrap();
        store.record(workspace_item(workspace, "old workspace")).unwrap();
        store.record(workspace_item(workspace, "new workspace")).unwrap();

        let loaded = store.load();
        assert_eq!(titles(&loaded), ["new workspace", "new session"]);
    }

    #[test]
    fn the_default_limit_keeps_the_twenty_newest() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecentClosedStore::new(dir.path());
        for index in 0..25 {
            store
                .record(session_item(Uuid::new_v4(), &format!("session {index}")))
                .unwrap();
        }
        let loaded = store.load();
        assert_eq!(loaded.len(), 20);
        assert_eq!(loaded[0].title, "session 24");
        assert_eq!(loaded[19].title, "session 5");
        assert_eq!(
            RecentClosedStore::with_limit(dir.path(), 0).load().len(),
            1,
            "at least one"
        );
    }

    #[test]
    fn remove_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecentClosedStore::new(dir.path());
        let (keep, drop) = (
            session_item(Uuid::new_v4(), "keep"),
            session_item(Uuid::new_v4(), "drop"),
        );
        store.record(keep.clone()).unwrap();
        store.record(drop.clone()).unwrap();
        store.remove(drop.id).unwrap();
        assert_eq!(titles(&store.load()), ["keep"]);
        store.clear().unwrap();
        assert!(store.load().is_empty());
        assert_eq!(
            fs::read_to_string(store.path()).unwrap(),
            "{\n  \"items\" : [\n\n  ],\n  \"version\" : 1\n}"
        );
    }

    #[test]
    fn another_version_a_bad_document_or_no_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecentClosedStore::new(dir.path());
        assert!(store.load().is_empty());
        let mut state = RecentClosedState::new(vec![session_item(Uuid::new_v4(), "stale")]);
        state.version = RECENT_CLOSED_VERSION + 1;
        json::save_json(store.path(), &state).unwrap();
        assert!(store.load().is_empty());
        for broken in [
            "garbage",
            r#"{"items": []}"#,
            r#"{"version": 1, "items": [{"id": "x"}]}"#,
        ] {
            fs::write(store.path(), broken).unwrap();
            assert!(store.load().is_empty(), "{broken}");
        }
    }

    #[test]
    fn a_workspace_entry_without_focus_member_or_with_a_retired_key_still_decodes() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecentClosedStore::new(dir.path());
        let item = workspace_item(Uuid::new_v4(), "work");
        json::save_json(store.path(), &RecentClosedState::new(vec![item])).unwrap();
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains("focusMember"), "{text}");
        assert_eq!(store.load()[0].workspace.as_ref().unwrap().focus_member, None);

        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value["items"][0]["workspace"]["focusMember"] = true.into();
        value["items"][0]["workspace"]["focusEnabled"] = true.into();
        fs::write(store.path(), value.to_string()).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].workspace.as_ref().unwrap().focus_member, Some(true));
    }

    #[test]
    fn dates_are_seconds_since_the_reference_date() {
        let at = |seconds: u64| UNIX_EPOCH + Duration::from_secs(978_307_200 + seconds);
        assert!((reference_date::seconds(at(0))).abs() < f64::EPSILON);
        assert!((reference_date::seconds(at(90)) - 90.0).abs() < f64::EPSILON);
        assert!((reference_date::seconds(UNIX_EPOCH) + 978_307_200.0).abs() < f64::EPSILON);
        assert_eq!(reference_date::time(90.0), Some(at(90)));
        assert_eq!(reference_date::time(-978_307_200.0), Some(UNIX_EPOCH));
        assert_eq!(reference_date::time(f64::NAN), None);
        assert_eq!(reference_date::time(f64::INFINITY), None);
    }

    #[test]
    fn an_entry_recorded_now_reads_back_equal() {
        let item = RecentClosedItem {
            closed_at: reference_date::now(),
            ..session_item(Uuid::new_v4(), "now")
        };
        let text = json::to_foundation_pretty(&item).unwrap();
        let again: RecentClosedItem = serde_json::from_slice(&text).unwrap();
        assert_eq!(again, item);
    }
}
