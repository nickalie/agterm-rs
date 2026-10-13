//! The successful payload of most commands (upstream's `ControlResult` in `ControlProtocol.swift`): every field
//! optional, absent when unset, in upstream's declaration order. Fields join as their commands do; `tree` and
//! `version` keep their own result types.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::events::EventBatch;

/// A command's result object.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlResult {
    /// The new or affected id of a mutating command (upper-case UUID).
    #[serde(default)]
    pub id: Option<String>,
    /// `window.list`.
    #[serde(default)]
    pub windows: Option<Vec<ControlWindowNode>>,
    /// How many things a batch mutation actually changed (`session.close`, `session.move` with several targets).
    #[serde(default)]
    pub affected: Option<i64>,
    /// `events.read`: a page of the event ring, also on a cursor failure (then the ring's current anchor).
    #[serde(default)]
    pub events: Option<EventBatch>,
}

impl ControlResult {
    /// `{"id": id}`.
    #[must_use]
    pub fn id(id: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            ..Self::default()
        }
    }

    /// `{"affected": n}`.
    #[must_use]
    pub fn affected(affected: i64) -> Self {
        Self {
            affected: Some(affected),
            ..Self::default()
        }
    }

    /// `{"events": {...}}`.
    #[must_use]
    pub fn events(events: EventBatch) -> Self {
        Self {
            events: Some(events),
            ..Self::default()
        }
    }

    /// `{"windows": [...]}`.
    #[must_use]
    pub fn windows(windows: Vec<ControlWindowNode>) -> Self {
        Self {
            windows: Some(windows),
            ..Self::default()
        }
    }
}

/// An open window's on-screen frame, in the coordinates `window.move`/`window.resize` take (upstream's
/// `ControlWindowFrame`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlWindowFrame {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub display: i64,
}

/// A window as `window.list` projects it (upstream's `ControlWindowNode`). The live fields are absent for a closed
/// window.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlWindowNode {
    pub id: String,
    pub name: String,
    /// Whether its on-screen window is up.
    pub open: bool,
    /// Whether it is the frontmost window.
    pub active: bool,
    /// The auto-follow timeout; absent while the feature is off.
    #[serde(default)]
    pub auto_follow_ms: Option<i64>,
    #[serde(default)]
    pub sidebar_visible: Option<bool>,
    #[serde(default)]
    pub geometry: Option<ControlWindowFrame>,
    #[serde(default)]
    pub fullscreen: Option<bool>,
    #[serde(default)]
    pub zoomed: Option<bool>,
    #[serde(default)]
    pub minimized: Option<bool>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn unset_fields_are_absent() {
        assert_eq!(
            serde_json::to_value(ControlResult::id("A")).unwrap(),
            json!({"id": "A"})
        );
        assert_eq!(
            serde_json::to_value(ControlResult::affected(0)).unwrap(),
            json!({"affected": 0})
        );
        assert_eq!(serde_json::to_value(ControlResult::default()).unwrap(), json!({}));
    }

    #[test]
    fn a_window_node_keeps_upstreams_field_order() {
        let node = ControlWindowNode {
            id: "W".to_owned(),
            name: "window 1".to_owned(),
            open: true,
            active: true,
            auto_follow_ms: None,
            sidebar_visible: Some(true),
            geometry: None,
            fullscreen: Some(false),
            zoomed: Some(false),
            minimized: Some(false),
        };
        let text = serde_json::to_string(&ControlResult::windows(vec![node.clone()])).unwrap();
        assert_eq!(
            text,
            r#"{"windows":[{"id":"W","name":"window 1","open":true,"active":true,"sidebarVisible":true,"fullscreen":false,"zoomed":false,"minimized":false}]}"#
        );
        let closed: ControlWindowNode =
            serde_json::from_value(json!({"id": "W", "name": "w", "open": false, "active": false})).unwrap();
        assert_eq!(closed.sidebar_visible, None);
        let back: ControlResult = serde_json::from_str(&text).unwrap();
        assert_eq!(back.windows, Some(vec![node]));
    }
}
