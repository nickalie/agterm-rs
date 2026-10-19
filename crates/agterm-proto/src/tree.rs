//! The `tree` read-back (upstream's `ControlProjection.swift`): the workspace tree of one window, with the live
//! per-window state and the app identity on its top level. Field names, order and optionality follow upstream's
//! `Codable` structs, which the catalog's `fields` list in the same order; an optional field is omitted when unset.
//!
//! The nodes decode as upstream's do, so `ngtermctl` can render a tree from a newer or older server: required fields
//! are the ones upstream decodes without a fallback, and an absent flag upstream defaults reads as that default.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_with::skip_serializing_none;

use crate::version::AppIdentity;

/// The payload of a `tree` response (`result.tree`).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlTree {
    pub workspaces: Vec<ControlWorkspaceNode>,
    /// Milliseconds since the last user input in the window; absent before any.
    #[serde(default)]
    pub idle_ms: Option<i64>,
    /// The window's auto-follow timeout; absent while the feature is off.
    #[serde(default)]
    pub auto_follow_ms: Option<i64>,
    #[serde(default)]
    pub sidebar_visible: Option<bool>,
    /// `tree` or `flagged`.
    #[serde(default)]
    pub sidebar_mode: Option<String>,
    /// `flat`, `plain` or `tree`; app-wide.
    #[serde(default)]
    pub sidebar_flagged_layout: Option<String>,
    /// `browser` or `overlay`; app-wide.
    #[serde(default)]
    pub link_open_mode: Option<String>,
    #[serde(default)]
    pub sidebar_width: Option<f64>,
    /// Whether the focus filter is applied; its members are each workspace's `focused`.
    #[serde(default)]
    pub workspace_filter: Option<bool>,
    #[serde(default)]
    pub quick_visible: Option<bool>,
    /// `surface:<session-id>:<kind>` or `quick`; absent when nothing is zoomed.
    #[serde(default)]
    pub zoomed_surface: Option<String>,
    #[serde(default)]
    pub dashboard_members: Option<Vec<String>>,
    #[serde(default)]
    pub dashboard_highlighted: Option<String>,
    #[serde(default)]
    pub dashboard_font_size: Option<f64>,
    #[serde(default)]
    pub dashboard_font_mode: Option<String>,
    #[serde(default)]
    pub pick_pending: Option<String>,
    #[serde(default)]
    pub ask_pending: Option<String>,
    /// The serving app, the same value `version` reports.
    #[serde(default)]
    pub app: Option<AppIdentity>,
    /// The live-sessions reset state (`live.reset`'s read-back), app-wide.
    #[serde(default)]
    pub live_reset: Option<Value>,
    /// True while the last `windows.json` write failed.
    #[serde(default)]
    pub index_unsaved: Option<bool>,
}

/// A workspace and its sessions.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlWorkspaceNode {
    pub id: String,
    pub name: String,
    /// The window's current workspace: what `active` resolves to for a workspace target.
    pub active: bool,
    /// A member of the focus set, filter applied or not; absent when not.
    #[serde(default)]
    pub focused: Option<bool>,
    /// Collapsed in the sidebar tree; absent when expanded.
    #[serde(default)]
    pub collapsed: Option<bool>,
    pub sessions: Vec<ControlSessionNode>,
}

/// A session. Each optional field is absent when it has nothing to report (see upstream's `ControlSessionNode` for
/// what each one reads back).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the wire shape: upstream's ControlSessionNode reports these five as plain JSON booleans"
)]
pub struct ControlSessionNode {
    pub id: String,
    pub name: String,
    pub cwd: String,
    #[serde(default)]
    pub split_cwd: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// The window's selected session.
    pub active: bool,
    /// The split is shown.
    pub split: bool,
    /// The session has a split pane, shown or hidden.
    #[serde(default)]
    pub has_split: Option<bool>,
    #[serde(default)]
    pub backed_by_live: Option<bool>,
    #[serde(default)]
    pub live_attribution: Option<String>,
    #[serde(default)]
    pub split_live_attribution: Option<String>,
    #[serde(default)]
    pub split_axis: Option<String>,
    #[serde(default)]
    pub split_ratio: Option<f64>,
    #[serde(default)]
    pub split_focused: Option<bool>,
    /// A caller's program covers the session.
    pub overlay: bool,
    #[serde(default)]
    pub overlay_size_percent: Option<i64>,
    #[serde(default)]
    pub pane_overlays: Option<Vec<String>>,
    #[serde(default)]
    pub hud: Option<ControlHudNode>,
    #[serde(default)]
    pub ask: Option<ControlSessionAsk>,
    pub scratch: bool,
    pub flagged: bool,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub command_wait: Option<bool>,
    #[serde(default)]
    pub split_command_wait: Option<bool>,
    #[serde(default)]
    pub foreground: Option<Vec<String>>,
    #[serde(default)]
    pub split_foreground: Option<Vec<String>>,
    #[serde(default)]
    pub foreground_shell: Option<String>,
    #[serde(default)]
    pub split_foreground_shell: Option<String>,
    #[serde(default)]
    pub restore_command: Option<String>,
    #[serde(default)]
    pub split_restore_command: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub status_pane: Option<String>,
    #[serde(default)]
    pub status_blink: Option<bool>,
    #[serde(default)]
    pub status_color: Option<String>,
    #[serde(default)]
    pub status_shape: Option<String>,
    /// Epoch seconds.
    #[serde(default)]
    pub status_changed_at: Option<f64>,
    /// A `BackgroundWatermark` in its persisted shape.
    #[serde(default)]
    pub background: Option<Value>,
    /// `PaneBackgrounds`: the per-pane overrides.
    #[serde(default)]
    pub pane_backgrounds: Option<Value>,
    #[serde(default)]
    pub unseen: Option<i64>,
    #[serde(default)]
    pub font_size: Option<f64>,
    #[serde(default)]
    pub split_font_size: Option<f64>,
    #[serde(default)]
    pub scratch_font_size: Option<f64>,
    #[serde(default)]
    pub surfaces: Option<Vec<ControlSurfaceNode>>,
    /// The main pane's terminal exists.
    #[serde(default)]
    pub realized: Option<bool>,
    #[serde(default)]
    pub remote_host: Option<String>,
    #[serde(default)]
    pub presentation: Option<ControlPresentationNode>,
    #[serde(default)]
    pub presenters: Option<ControlPresentersNode>,
    #[serde(default)]
    pub remote_overlays: Option<Vec<ControlRemoteOverlayNode>>,
    #[serde(default)]
    pub html_overlays: Option<Vec<ControlHtmlOverlayNode>>,
}

impl ControlSessionNode {
    /// A session node with every optional field absent and every flag false.
    #[must_use]
    pub fn new(id: String, name: String, cwd: String) -> Self {
        Self {
            id,
            name,
            cwd,
            split_cwd: None,
            title: None,
            active: false,
            split: false,
            has_split: None,
            backed_by_live: None,
            live_attribution: None,
            split_live_attribution: None,
            split_axis: None,
            split_ratio: None,
            split_focused: None,
            overlay: false,
            overlay_size_percent: None,
            pane_overlays: None,
            hud: None,
            ask: None,
            scratch: false,
            flagged: false,
            context: None,
            command_wait: None,
            split_command_wait: None,
            foreground: None,
            split_foreground: None,
            foreground_shell: None,
            split_foreground_shell: None,
            restore_command: None,
            split_restore_command: None,
            status: None,
            status_pane: None,
            status_blink: None,
            status_color: None,
            status_shape: None,
            status_changed_at: None,
            background: None,
            pane_backgrounds: None,
            unseen: None,
            font_size: None,
            split_font_size: None,
            scratch_font_size: None,
            surfaces: None,
            realized: None,
            remote_host: None,
            presentation: None,
            presenters: None,
            remote_overlays: None,
            html_overlays: None,
        }
    }
}

/// One addressable terminal surface of a session. `id` is `surface:<session-id>:<kind>`; `kind` is `left`, `right`,
/// `scratch`, `overlay`, `overlay-left` or `overlay-right`.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlSurfaceNode {
    pub id: String,
    pub kind: String,
    pub active: bool,
    pub visible: bool,
    /// Primary and split panes only.
    #[serde(default)]
    pub backed_by_live: Option<bool>,
    /// `leader`, `follower` or `unowned`.
    #[serde(default)]
    pub lead: Option<String>,
    #[serde(default)]
    pub reconnect: Option<ControlReconnect>,
    /// The token `--pane-id` resolves.
    #[serde(default, rename = "paneID")]
    pub pane_id: Option<String>,
}

/// A remote pane waiting to be attached again.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlReconnect {
    pub failures: i64,
    #[serde(default)]
    pub reason: Option<String>,
}

const fn yes() -> bool {
    true
}

/// The HUD panel in a session's overlay slot.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlHudNode {
    pub message: String,
    #[serde(default)]
    pub detail: Option<String>,
    pub spinner: String,
    #[serde(default)]
    pub background_color: Option<String>,
    #[serde(default)]
    pub text_color: Option<String>,
    #[serde(default)]
    pub size_percent: Option<i64>,
    #[serde(default)]
    pub height_percent: Option<i64>,
    pub position: String,
    #[serde(default)]
    pub pane: Option<String>,
    pub hide_after: f64,
    #[serde(default)]
    pub markdown: bool,
    #[serde(default)]
    pub font_size: Option<f64>,
    #[serde(default)]
    pub sticky: bool,
    #[serde(default = "yes")]
    pub frame: bool,
}

/// A session's pending terminal ask.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlSessionAsk {
    pub id: String,
    #[serde(default)]
    pub pane: Option<String>,
    #[serde(default)]
    pub remote: Option<bool>,
    #[serde(default)]
    pub replica: Option<bool>,
}

/// One HTML page in an overlay slot.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the wire shape: upstream's ControlHtmlOverlayNode reports these four as plain JSON booleans"
)]
pub struct ControlHtmlOverlayNode {
    #[serde(default)]
    pub pane: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// `loading`, `loaded` or `failed`.
    pub state: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub page: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub can_go_back: Option<bool>,
    #[serde(default)]
    pub can_go_forward: Option<bool>,
    #[serde(default)]
    pub navigation: Option<bool>,
    pub javascript: bool,
    #[serde(default)]
    pub chromeless: bool,
    #[serde(default)]
    pub persistent: bool,
    #[serde(default)]
    pub browse: bool,
    #[serde(default)]
    pub zoom: Option<f64>,
    #[serde(default)]
    pub id: Option<String>,
}

/// An overlay slot of an origin session held by the viewer presenting it.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRemoteOverlayNode {
    #[serde(default)]
    pub pane: Option<String>,
    #[serde(default)]
    pub size_percent: Option<i64>,
}

/// A viewer's presentation stream.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlPresentationNode {
    pub state: String,
    pub mode: String,
    #[serde(default)]
    pub error: Option<String>,
}

/// The viewers of an origin session.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlPresentersNode {
    pub mirrors: i64,
    #[serde(default)]
    pub presenter: Option<bool>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn keys(value: &Value) -> Vec<&str> {
        value
            .as_object()
            .map(|map| map.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }

    #[test]
    fn a_bare_session_carries_only_its_required_fields_in_wire_order() {
        let node = ControlSessionNode::new("S".to_owned(), "zsh".to_owned(), "/h".to_owned());
        let value = serde_json::to_value(&node).unwrap();
        assert_eq!(
            keys(&value),
            ["id", "name", "cwd", "active", "split", "overlay", "scratch", "flagged"]
        );
    }

    #[test]
    fn every_session_field_serializes_in_upstream_order() {
        let mut node = ControlSessionNode::new("S".to_owned(), "n".to_owned(), "/".to_owned());
        node.split_cwd = Some(String::new());
        node.title = Some(String::new());
        node.has_split = Some(true);
        node.backed_by_live = Some(false);
        node.live_attribution = Some(String::new());
        node.split_live_attribution = Some(String::new());
        node.split_axis = Some(String::new());
        node.split_ratio = Some(0.5);
        node.split_focused = Some(false);
        node.overlay_size_percent = Some(1);
        node.pane_overlays = Some(vec![]);
        node.hud = Some(ControlHudNode {
            message: String::new(),
            detail: None,
            spinner: "none".to_owned(),
            background_color: None,
            text_color: None,
            size_percent: None,
            height_percent: None,
            position: "center".to_owned(),
            pane: None,
            hide_after: 0.0,
            markdown: false,
            font_size: None,
            sticky: false,
            frame: true,
        });
        node.ask = Some(ControlSessionAsk {
            id: String::new(),
            pane: None,
            remote: None,
            replica: None,
        });
        node.context = Some(String::new());
        node.command_wait = Some(true);
        node.split_command_wait = Some(true);
        node.foreground = Some(vec![]);
        node.split_foreground = Some(vec![]);
        node.foreground_shell = Some(String::new());
        node.split_foreground_shell = Some(String::new());
        node.restore_command = Some(String::new());
        node.split_restore_command = Some(String::new());
        node.status = Some(String::new());
        node.status_pane = Some(String::new());
        node.status_blink = Some(true);
        node.status_color = Some(String::new());
        node.status_shape = Some(String::new());
        node.status_changed_at = Some(1.0);
        node.background = Some(json!({}));
        node.pane_backgrounds = Some(json!({}));
        node.unseen = Some(1);
        node.font_size = Some(1.0);
        node.split_font_size = Some(1.0);
        node.scratch_font_size = Some(1.0);
        node.surfaces = Some(vec![]);
        node.realized = Some(true);
        node.remote_host = Some(String::new());
        node.presentation = Some(ControlPresentationNode {
            state: String::new(),
            mode: String::new(),
            error: None,
        });
        node.presenters = Some(ControlPresentersNode {
            mirrors: 0,
            presenter: None,
        });
        node.remote_overlays = Some(vec![]);
        node.html_overlays = Some(vec![]);
        let value = serde_json::to_value(&node).unwrap();
        assert_eq!(keys(&value), SESSION_FIELDS);
        let back: ControlSessionNode = serde_json::from_value(value).unwrap();
        assert_eq!(back, node);
    }

    /// The catalog's `field:tree.session.*` order (its `backedByZmx` is `backedByLive` here).
    const SESSION_FIELDS: [&str; 49] = [
        "id",
        "name",
        "cwd",
        "splitCwd",
        "title",
        "active",
        "split",
        "hasSplit",
        "backedByLive",
        "liveAttribution",
        "splitLiveAttribution",
        "splitAxis",
        "splitRatio",
        "splitFocused",
        "overlay",
        "overlaySizePercent",
        "paneOverlays",
        "hud",
        "ask",
        "scratch",
        "flagged",
        "context",
        "commandWait",
        "splitCommandWait",
        "foreground",
        "splitForeground",
        "foregroundShell",
        "splitForegroundShell",
        "restoreCommand",
        "splitRestoreCommand",
        "status",
        "statusPane",
        "statusBlink",
        "statusColor",
        "statusShape",
        "statusChangedAt",
        "background",
        "paneBackgrounds",
        "unseen",
        "fontSize",
        "splitFontSize",
        "scratchFontSize",
        "surfaces",
        "realized",
        "remoteHost",
        "presentation",
        "presenters",
        "remoteOverlays",
        "htmlOverlays",
    ];

    #[test]
    fn the_top_level_serializes_in_upstream_order() {
        let tree = ControlTree {
            workspaces: vec![],
            idle_ms: Some(1),
            auto_follow_ms: Some(1),
            sidebar_visible: Some(true),
            sidebar_mode: Some("tree".to_owned()),
            sidebar_flagged_layout: Some("flat".to_owned()),
            link_open_mode: Some("browser".to_owned()),
            sidebar_width: Some(220.0),
            workspace_filter: Some(false),
            quick_visible: Some(false),
            zoomed_surface: Some(String::new()),
            dashboard_members: Some(vec![]),
            dashboard_highlighted: Some(String::new()),
            dashboard_font_size: Some(1.0),
            dashboard_font_mode: Some(String::new()),
            pick_pending: Some(String::new()),
            ask_pending: Some(String::new()),
            app: Some(AppIdentity::from_recorded("1", None)),
            live_reset: Some(json!({})),
            index_unsaved: Some(true),
        };
        let value = serde_json::to_value(&tree).unwrap();
        assert_eq!(
            keys(&value),
            [
                "workspaces",
                "idleMs",
                "autoFollowMs",
                "sidebarVisible",
                "sidebarMode",
                "sidebarFlaggedLayout",
                "linkOpenMode",
                "sidebarWidth",
                "workspaceFilter",
                "quickVisible",
                "zoomedSurface",
                "dashboardMembers",
                "dashboardHighlighted",
                "dashboardFontSize",
                "dashboardFontMode",
                "pickPending",
                "askPending",
                "app",
                "liveReset",
                "indexUnsaved",
            ]
        );
        assert_eq!(
            keys(&serde_json::to_value(ControlTree::default()).unwrap()),
            ["workspaces"]
        );
    }

    #[test]
    fn nodes_decode_with_upstreams_defaults() {
        let hud: ControlHudNode =
            serde_json::from_value(json!({"message": "m", "spinner": "none", "position": "center", "hideAfter": 0}))
                .unwrap();
        assert!(!hud.markdown && !hud.sticky && hud.frame);
        let page: ControlHtmlOverlayNode =
            serde_json::from_value(json!({"state": "loaded", "javascript": true})).unwrap();
        assert!(page.javascript && !page.chromeless && !page.persistent && !page.browse);
        let surface: ControlSurfaceNode = serde_json::from_value(
            json!({"id": "surface:S:left", "kind": "left", "active": true, "visible": true, "paneID": "P"}),
        )
        .unwrap();
        assert_eq!(surface.pane_id.as_deref(), Some("P"));
        assert!(
            serde_json::from_value::<ControlWorkspaceNode>(json!({"id": "W", "name": "w", "active": true})).is_err()
        );
    }
}
