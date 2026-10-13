//! The persisted window tree (upstream's `Snapshot.swift`, `PersistenceStore.swift`) and the window index
//! (`WindowsIndex`, `WindowEntry` in `WindowLibrary.swift`).
//!
//! Every snapshot field after `workspaces` is optional and LOSSY: a missing key, a JSON `null`, a value of the wrong
//! type or an unknown enum name all decode to `None`, so one bad optional never fails the load and wipes the tree.
//! What stays strict is identity and payload: `version`, `workspaces`, and each workspace's `id`/`name`/`sessions` and
//! session's `id`/`cwd`. A session context is also checked for content. The legacy single-workspace
//! `focusedWorkspaceID` migrates to an enabled one-member focus set and is never written back.
//!
//! The window index is decoded strictly, as upstream's synthesized `Codable`: any defect makes it count as absent.

use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

use super::{json, lossy, upper};
use crate::model::{BackgroundWatermark, PaneBackgrounds, SidebarMode, SplitAxis, validate_context};

/// The per-window file shape's version; a mismatch makes the loader start fresh.
pub const SNAPSHOT_VERSION: i64 = 1;
/// The window index's version, independent of [`SNAPSHOT_VERSION`].
pub const INDEX_VERSION: i64 = 1;

impl Serialize for SplitAxis {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_name())
    }
}

impl<'de> Deserialize<'de> for SplitAxis {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::from_wire(&name).ok_or_else(|| serde::de::Error::custom(format!("unknown split axis {name}")))
    }
}

impl Serialize for SidebarMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_name())
    }
}

impl<'de> Deserialize<'de> for SidebarMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::from_wire(&name).ok_or_else(|| serde::de::Error::custom(format!("unknown sidebar mode {name}")))
    }
}

/// A stored context that breaks the context rules drops to `None`; a valid one is kept trimmed.
fn lossy_context<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let stored: Option<String> = lossy(deserializer)?;
    Ok(stored.and_then(|raw| validate_context(&raw).ok()))
}

/// One window's persisted state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "RawSnapshot")]
pub struct Snapshot {
    pub version: i64,
    #[serde(serialize_with = "upper::option", skip_serializing_if = "Option::is_none")]
    #[serde(rename = "selectedSessionID")]
    pub selected_session_id: Option<Uuid>,
    pub workspaces: Vec<WorkspaceSnapshot>,
    /// The sidebar width in points; `None` is the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_width: Option<f64>,
    /// `None` is shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_visible: Option<bool>,
    /// `None` is the tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_mode: Option<SidebarMode>,
    /// The marked focus set in tree order; `None` is nothing marked.
    #[serde(serialize_with = "upper::option_vec", skip_serializing_if = "Option::is_none")]
    #[serde(rename = "focusedWorkspaceIDs")]
    pub focused_workspace_ids: Option<Vec<Uuid>>,
    /// Whether the focus filter applies; `None` is off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_enabled: Option<bool>,
    /// The most recently selected sessions, current first.
    #[serde(serialize_with = "upper::option_vec", skip_serializing_if = "Option::is_none")]
    pub session_recency: Option<Vec<Uuid>>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            selected_session_id: None,
            workspaces: Vec::new(),
            sidebar_width: None,
            sidebar_visible: None,
            sidebar_mode: None,
            focused_workspace_ids: None,
            focus_enabled: None,
            session_recency: None,
        }
    }
}

/// The decode side of [`Snapshot`]: strict identity, lossy optionals, and the legacy focus key kept apart so the
/// migration can tell an absent focus set from a malformed one.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSnapshot {
    version: i64,
    #[serde(default, deserialize_with = "lossy", rename = "selectedSessionID")]
    selected_session_id: Option<Uuid>,
    workspaces: Vec<WorkspaceSnapshot>,
    #[serde(default, deserialize_with = "lossy")]
    sidebar_width: Option<f64>,
    #[serde(default, deserialize_with = "lossy")]
    sidebar_visible: Option<bool>,
    #[serde(default, deserialize_with = "lossy")]
    sidebar_mode: Option<SidebarMode>,
    /// Kept raw: `None` (absent or null) is what lets the legacy key migrate; a malformed value must not.
    #[serde(default, rename = "focusedWorkspaceIDs")]
    focused_workspace_ids: Option<Value>,
    #[serde(default, deserialize_with = "lossy", rename = "focusedWorkspaceID")]
    legacy_focused_workspace_id: Option<Uuid>,
    #[serde(default, deserialize_with = "lossy")]
    focus_enabled: Option<bool>,
    #[serde(default, deserialize_with = "lossy")]
    session_recency: Option<Vec<Uuid>>,
}

impl From<RawSnapshot> for Snapshot {
    fn from(raw: RawSnapshot) -> Self {
        let (focused_workspace_ids, focus_enabled) = match (raw.focused_workspace_ids, raw.legacy_focused_workspace_id)
        {
            (None, Some(legacy)) => (Some(vec![legacy]), Some(true)),
            (ids, _) => (
                ids.and_then(|ids| Vec::<Uuid>::deserialize(ids).ok()),
                raw.focus_enabled,
            ),
        };
        Self {
            version: raw.version,
            selected_session_id: raw.selected_session_id,
            workspaces: raw.workspaces,
            sidebar_width: raw.sidebar_width,
            sidebar_visible: raw.sidebar_visible,
            sidebar_mode: raw.sidebar_mode,
            focused_workspace_ids,
            focus_enabled,
            session_recency: raw.session_recency,
        }
    }
}

/// One persisted workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    #[serde(serialize_with = "upper::uuid")]
    pub id: Uuid,
    pub name: String,
    pub sessions: Vec<SessionSnapshot>,
    /// The inverse of expanded, so only a collapsed workspace writes it.
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub collapsed: Option<bool>,
}

/// One persisted session (field docs on upstream's `SessionSnapshot`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    #[serde(serialize_with = "upper::uuid")]
    pub id: Uuid,
    #[serde(default, deserialize_with = "lossy", serialize_with = "upper::option")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_identity: Option<Uuid>,
    #[serde(default, deserialize_with = "lossy", serialize_with = "upper::option")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split_pane_identity: Option<Uuid>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    /// The live cwd, or where the session started before any report.
    pub cwd: String,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub is_split: Option<bool>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub has_split: Option<bool>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub split_axis: Option<SplitAxis>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub split_cwd: Option<String>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub split_ratio: Option<f64>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub flagged: Option<bool>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub foreground_command: Option<Vec<String>>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub split_foreground_command: Option<Vec<String>>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub initial_command: Option<String>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub command_wait: Option<bool>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub split_initial_command: Option<String>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub split_command_wait: Option<bool>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub background_watermark: Option<BackgroundWatermark>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub pane_backgrounds: Option<PaneBackgrounds>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub restore_command: Option<String>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub split_restore_command: Option<String>,
    #[serde(default, deserialize_with = "lossy_context", skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

impl SessionSnapshot {
    /// A snapshot of a session with only its identity and cwd, the rest unset.
    #[must_use]
    pub fn new(id: Uuid, cwd: impl Into<String>) -> Self {
        Self {
            id,
            cwd: cwd.into(),
            ..Self::default()
        }
    }

    /// Whether a split exists, shown or hidden (`hasSplit`, else the legacy `isSplit`).
    #[must_use]
    pub fn has_any_split(&self) -> bool {
        self.is_split.unwrap_or(false) || self.has_split.unwrap_or(false)
    }
}

/// One window in the index: its id, name and whether it was open at quit, which drives the reopen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowEntry {
    #[serde(serialize_with = "upper::uuid")]
    pub id: Uuid,
    pub name: String,
    pub is_open: bool,
}

/// `windows.json`: the ordered window list and the frontmost window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowsIndex {
    pub version: i64,
    #[serde(default, serialize_with = "upper::option", skip_serializing_if = "Option::is_none")]
    pub frontmost: Option<Uuid>,
    pub windows: Vec<WindowEntry>,
}

impl WindowsIndex {
    /// An index of the current version.
    #[must_use]
    pub fn new(frontmost: Option<Uuid>, windows: Vec<WindowEntry>) -> Self {
        Self {
            version: INDEX_VERSION,
            frontmost,
            windows,
        }
    }

    /// Reads an index: a missing, unreadable, undecodable, version-mismatched or empty one reads as `None`, so the
    /// caller falls through to recovery, migration or seeding.
    #[must_use]
    pub fn load(path: &Path) -> Option<Self> {
        let index: Self = read_json(path).ok()?;
        (index.version == INDEX_VERSION && !index.windows.is_empty()).then_some(index)
    }
}

/// Why a strict snapshot read failed.
#[derive(Debug)]
pub enum LoadError {
    /// The file could not be read.
    Io(io::Error),
    /// The file is not a snapshot.
    Decode(serde_json::Error),
    /// The file is a snapshot of another version.
    VersionMismatch,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Decode(error) => write!(f, "{error}"),
            Self::VersionMismatch => f.write_str("snapshot version mismatch"),
        }
    }
}

impl std::error::Error for LoadError {}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, LoadError> {
    let data = std::fs::read(path).map_err(LoadError::Io)?;
    serde_json::from_slice(&data).map_err(LoadError::Decode)
}

/// Reads and writes one [`Snapshot`] file (upstream's `PersistenceStore`): a window's `windows/<ID>.json`, or the
/// legacy `workspaces.json`.
#[derive(Debug, Clone)]
pub struct PersistenceStore {
    path: PathBuf,
}

impl PersistenceStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the snapshot, an empty one on any failure (missing, unreadable, corrupt, another version).
    #[must_use]
    pub fn load(&self) -> Snapshot {
        self.load_checked().unwrap_or_default()
    }

    /// The strict read: every failure reaches the caller.
    ///
    /// # Errors
    ///
    /// The file could not be read or decoded, or is of another version.
    pub fn load_checked(&self) -> Result<Snapshot, LoadError> {
        let snapshot: Snapshot = read_json(&self.path)?;
        if snapshot.version != SNAPSHOT_VERSION {
            return Err(LoadError::VersionMismatch);
        }
        Ok(snapshot)
    }

    /// Writes the snapshot atomically, creating the directory if needed.
    ///
    /// # Errors
    ///
    /// The directory could not be created or the file not written.
    pub fn save(&self, snapshot: &Snapshot) -> io::Result<()> {
        json::save_json(&self.path, snapshot)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const WS: &str = "1D000000-0000-4000-8000-000000000001";
    const SESSION: &str = "5E000000-0000-4000-8000-000000000001";

    fn decode(text: &str) -> Snapshot {
        serde_json::from_str(text).unwrap()
    }

    /// A one-workspace, one-session tree with `extra` spliced into the snapshot object.
    fn with_snapshot_key(extra: &str) -> String {
        format!(
            r#"{{ "version": 1, {extra} "workspaces": [ {{ "id": "{WS}", "name": "work", "sessions": [ {{ "id": "{SESSION}", "cwd": "/a" }} ] }} ] }}"#
        )
    }

    /// The same tree with `extra` spliced into the session object.
    fn with_session_key(extra: &str) -> String {
        format!(
            r#"{{ "version": 1, "workspaces": [ {{ "id": "{WS}", "name": "work", "sessions": [ {{ "id": "{SESSION}", "cwd": "/a", {extra} }} ] }} ] }}"#
        )
    }

    fn ws() -> Uuid {
        Uuid::parse_str(WS).unwrap()
    }

    fn session() -> Uuid {
        Uuid::parse_str(SESSION).unwrap()
    }

    #[test]
    fn malformed_snapshot_optionals_drop_alone() {
        type Nilled = fn(&Snapshot) -> bool;
        let cases: [(&str, Nilled); 7] = [
            (r#""selectedSessionID": "not-a-uuid","#, |s| {
                s.selected_session_id.is_none()
            }),
            (r#""sidebarWidth": "wide","#, |s| s.sidebar_width.is_none()),
            (r#""sidebarVisible": "yes","#, |s| s.sidebar_visible.is_none()),
            (r#""sidebarMode": "grid","#, |s| s.sidebar_mode.is_none()),
            (r#""focusedWorkspaceID": "not-a-uuid","#, |s| {
                s.focused_workspace_ids.is_none() && s.focus_enabled.is_none()
            }),
            (r#""focusEnabled": 42,"#, |s| s.focus_enabled.is_none()),
            (r#""sessionRecency": ["not-a-uuid"],"#, |s| s.session_recency.is_none()),
        ];
        for (bad, nilled) in cases {
            let snapshot = decode(&with_snapshot_key(bad));
            assert_eq!(snapshot.workspaces[0].id, ws(), "{bad} wiped the tree");
            assert_eq!(snapshot.workspaces[0].sessions[0].id, session(), "{bad}");
            assert!(nilled(&snapshot), "{bad} did not drop its field");
        }
        for bad in [r#""focusedWorkspaceIDs": ["x"],"#, r#""focusedWorkspaceIDs": 42,"#] {
            assert_eq!(decode(&with_snapshot_key(bad)).focused_workspace_ids, None, "{bad}");
        }
    }

    #[test]
    fn malformed_session_optionals_drop_alone() {
        let bad_values = [
            r#""fontSize": "12""#,
            r#""isSplit": 1"#,
            r#""splitRatio": "half""#,
            r#""flagged": "true""#,
            r#""foregroundCommand": "vim""#,
            r#""commandWait": "no""#,
            r#""customName": 7"#,
            r#""restoreCommand": []"#,
            r#""splitCwd": 3"#,
            r#""splitForegroundCommand": "vim""#,
            r#""initialCommand": ["vim"]"#,
            r#""splitRestoreCommand": 0"#,
            r#""backgroundWatermark": "none""#,
            r#""backgroundWatermark": {"kind": "sparkles"}"#,
            r#""paneIdentity": "nope""#,
            r#""splitAxis": "diagonal""#,
            r#""paneBackgrounds": 1"#,
        ];
        for bad in bad_values {
            let snapshot = decode(&with_session_key(bad));
            let session = &snapshot.workspaces[0].sessions[0];
            assert_eq!(*session, SessionSnapshot::new(self::session(), "/a"), "{bad}");
        }
        let collapsed = format!(
            r#"{{ "version": 1, "workspaces": [ {{ "id": "{WS}", "name": "work", "collapsed": "yes", "sessions": [] }} ] }}"#
        );
        assert_eq!(decode(&collapsed).workspaces[0].collapsed, None);
    }

    #[test]
    fn identity_and_payload_stay_strict() {
        let session_without = |field: &str| with_session_key(r#""x": 0"#).replace(field, "");
        for broken in [
            r#"{ "workspaces": [] }"#.to_owned(),
            r#"{ "version": 1 }"#.to_owned(),
            with_session_key(r#""flagged": true"#).replace(r#""cwd": "/a""#, r#""cwd": 5"#),
            session_without(r#""cwd": "/a", "#),
            format!(r#"{{ "version": 1, "workspaces": [ {{ "id": "{WS}", "sessions": [] }} ] }}"#),
            r#"{ "version": 1, "workspaces": [ { "id": "x", "name": "w", "sessions": [] } ] }"#.to_owned(),
        ] {
            assert!(serde_json::from_str::<Snapshot>(&broken).is_err(), "{broken}");
        }
    }

    #[test]
    fn the_legacy_focus_key_migrates_to_an_enabled_set() {
        let snapshot = decode(&with_snapshot_key(&format!(r#""focusedWorkspaceID": "{WS}","#)));
        assert_eq!(snapshot.focused_workspace_ids, Some(vec![ws()]));
        assert_eq!(snapshot.focus_enabled, Some(true));
        let text = String::from_utf8(json::to_foundation_pretty(&snapshot).unwrap()).unwrap();
        assert!(!text.contains("\"focusedWorkspaceID\""), "{text}");
        assert!(text.contains("\"focusedWorkspaceIDs\"") && text.contains("\"focusEnabled\""));
        let again = decode(&text);
        assert_eq!(again.focused_workspace_ids, Some(vec![ws()]));
        assert_eq!(again.focus_enabled, Some(true));
    }

    #[test]
    fn both_focus_keys_prefer_the_set_and_its_explicit_flag() {
        let other = "1D000000-0000-4000-8000-000000000009";
        let keys =
            format!(r#""focusedWorkspaceID": "{other}", "focusedWorkspaceIDs": ["{WS}"], "focusEnabled": false,"#);
        let snapshot = decode(&with_snapshot_key(&keys));
        assert_eq!(snapshot.focused_workspace_ids, Some(vec![ws()]));
        assert_eq!(snapshot.focus_enabled, Some(false));
    }

    #[test]
    fn a_malformed_focus_set_leaves_the_legacy_key_unmigrated() {
        let keys = format!(r#""focusedWorkspaceID": "{WS}", "focusedWorkspaceIDs": 42, "focusEnabled": false,"#);
        let snapshot = decode(&with_snapshot_key(&keys));
        assert_eq!(snapshot.focused_workspace_ids, None);
        assert_eq!(snapshot.focus_enabled, Some(false));
        // a null set reads as absent, as `decodeIfPresent` returns nil for it, so the legacy key migrates
        let keys = format!(r#""focusedWorkspaceID": "{WS}", "focusedWorkspaceIDs": null,"#);
        assert_eq!(decode(&with_snapshot_key(&keys)).focus_enabled, Some(true));
    }

    #[test]
    fn a_stored_context_is_checked_for_content() {
        for stored in [
            r#""""#,
            r#""   ""#,
            r#""PR\n517""#,
            r#""PR 517""#,
            r#""PR\t517""#,
            r#""PR517\n""#,
        ] {
            let snapshot = decode(&with_session_key(&format!(
                r#""context": {stored}, "customName": "keep""#
            )));
            let session = &snapshot.workspaces[0].sessions[0];
            assert_eq!(session.context, None, "{stored}");
            assert_eq!(session.custom_name.as_deref(), Some("keep"));
        }
        let oversized = "a".repeat(crate::model::CONTEXT_BYTE_LIMIT + 1);
        let snapshot = decode(&with_session_key(&format!(r#""context": "{oversized}""#)));
        assert_eq!(snapshot.workspaces[0].sessions[0].context, None);
        let snapshot = decode(&with_session_key(r#""context": "  PR #517  ""#));
        assert_eq!(snapshot.workspaces[0].sessions[0].context.as_deref(), Some("PR #517"));
    }

    #[test]
    fn removed_and_unknown_keys_are_ignored() {
        let snapshot = decode(&with_snapshot_key(
            r#""statusBarHidden": true, "titleBarHidden": true,"#,
        ));
        assert_eq!(snapshot.workspaces[0].id, ws());
        let snapshot = decode(&with_session_key(r#""hudSpec": {"message": "x"}"#));
        assert_eq!(snapshot.workspaces[0].sessions[0].id, session());
    }

    #[test]
    fn legacy_sessions_decode_with_every_optional_unset() {
        let session: SessionSnapshot = serde_json::from_str(&format!(r#"{{"id":"{SESSION}","cwd":"/tmp"}}"#)).unwrap();
        assert_eq!(session, SessionSnapshot::new(self::session(), "/tmp"));
        let legacy: SessionSnapshot =
            serde_json::from_str(&format!(r#"{{"id":"{SESSION}","cwd":"/tmp","isSplit":true}}"#)).unwrap();
        assert!(legacy.has_any_split());
        assert_eq!((legacy.pane_identity, legacy.split_axis), (None, None));
    }

    #[test]
    fn an_empty_restore_command_round_trips_as_empty() {
        let mut session = SessionSnapshot::new(session(), "/a");
        session.restore_command = Some(String::new());
        let text = json::to_foundation_pretty(&session).unwrap();
        let again: SessionSnapshot = serde_json::from_slice(&text).unwrap();
        assert_eq!(again.restore_command.as_deref(), Some(""));
    }

    #[test]
    fn the_store_recovers_an_empty_snapshot_from_any_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = PersistenceStore::new(dir.path().join("workspaces.json"));
        assert_eq!(store.load(), Snapshot::default());
        assert!(matches!(store.load_checked(), Err(LoadError::Io(_))));
        fs::write(store.path(), "{ not valid json ]").unwrap();
        assert_eq!(store.load(), Snapshot::default());
        assert!(matches!(store.load_checked(), Err(LoadError::Decode(_))));
        fs::write(store.path(), r#"{"version": 2, "workspaces": []}"#).unwrap();
        assert_eq!(store.load(), Snapshot::default());
        assert!(matches!(store.load_checked(), Err(LoadError::VersionMismatch)));
    }

    #[test]
    fn save_load_round_trips_and_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = PersistenceStore::new(dir.path().join("does/not/exist/yet.json"));
        let original = Snapshot {
            selected_session_id: Some(session()),
            workspaces: vec![
                WorkspaceSnapshot {
                    id: ws(),
                    name: "work".to_owned(),
                    sessions: vec![
                        SessionSnapshot {
                            custom_name: Some("build".to_owned()),
                            ..SessionSnapshot::new(session(), "/Users/user/dev/foo")
                        },
                        SessionSnapshot::new(Uuid::new_v4(), "/tmp"),
                    ],
                    collapsed: None,
                },
                WorkspaceSnapshot {
                    id: Uuid::new_v4(),
                    name: "personal".to_owned(),
                    sessions: Vec::new(),
                    collapsed: Some(true),
                },
            ],
            ..Snapshot::default()
        };
        store.save(&original).unwrap();
        assert_eq!(store.load(), original);
    }

    #[test]
    fn ids_are_written_upper_case_and_read_in_either_case() {
        let snapshot = decode(&with_snapshot_key("").to_lowercase());
        assert_eq!(snapshot.workspaces[0].id, ws());
        let text = String::from_utf8(json::to_foundation_pretty(&snapshot).unwrap()).unwrap();
        assert!(text.contains(WS) && text.contains(SESSION), "{text}");
    }

    #[test]
    fn the_index_counts_as_absent_unless_valid_current_and_non_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("windows.json");
        assert_eq!(WindowsIndex::load(&path), None);
        let entry = format!(r#"{{"id": "{WS}", "name": "w", "isOpen": true}}"#);
        for (text, valid) in [
            (format!(r#"{{"version": 1, "windows": [{entry}]}}"#), true),
            (format!(r#"{{"version": 2, "windows": [{entry}]}}"#), false),
            (r#"{"version": 1, "windows": []}"#.to_owned(), false),
            (
                format!(r#"{{"version": 1, "frontmost": "x", "windows": [{entry}]}}"#),
                false,
            ),
            ("garbage".to_owned(), false),
        ] {
            fs::write(&path, &text).unwrap();
            assert_eq!(WindowsIndex::load(&path).is_some(), valid, "{text}");
        }
    }
}
