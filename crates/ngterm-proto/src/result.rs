//! The successful payload of most commands (upstream's `ControlResult` in `ControlProtocol.swift`): every field
//! optional, absent when unset, in upstream's declaration order. Fields join as their commands do; `tree` and
//! `version` keep their own result types.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::events::EventBatch;
use crate::keymap::{ControlHooks, ControlKeymap};

/// A command's result object.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlResult {
    /// The new or affected id of a mutating command (upper-case UUID).
    #[serde(default)]
    pub id: Option<String>,
    /// `window.resize`: the applied frame width.
    #[serde(default)]
    pub width: Option<i64>,
    /// `window.resize`: the applied frame height.
    #[serde(default)]
    pub height: Option<i64>,
    /// `session.text`/`session.copy`: the text read; `session.search`: the match counter's display text.
    #[serde(default)]
    pub text: Option<String>,
    /// `window.list`.
    #[serde(default)]
    pub windows: Option<Vec<ControlWindowNode>>,
    /// `session.overlay.result`: the overlay program's exit status.
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// `session.search`: libghostty's match count, absent before a query ran.
    #[serde(default)]
    pub count: Option<i64>,
    /// How many things a batch mutation actually changed (`session.close`, `session.move` with several targets).
    #[serde(default)]
    pub affected: Option<i64>,
    /// `theme.set`/`theme.list`: the single theme, absent for ghostty's built-in and while following the appearance.
    #[serde(default)]
    pub theme: Option<String>,
    /// `theme.list`: the bundled theme names.
    #[serde(default)]
    pub themes: Option<Vec<String>>,
    /// `session.resize`: the applied, clamped primary-pane fraction.
    #[serde(default)]
    pub ratio: Option<f64>,
    /// `sidebar.width`: the stored, clamped sidebar width in points.
    #[serde(default)]
    pub sidebar_width: Option<f64>,
    /// The pane a command read or typed into (`left|right|scratch`).
    #[serde(default)]
    pub pane: Option<String>,
    /// `theme.set`/`theme.list`: whether the theme follows the system appearance.
    #[serde(default)]
    pub sync: Option<bool>,
    /// `theme.set`/`theme.list` while following the appearance: the light slot.
    #[serde(default)]
    pub light: Option<String>,
    /// `theme.set`/`theme.list` while following the appearance: the dark slot.
    #[serde(default)]
    pub dark: Option<String>,
    /// `events.read`: a page of the event ring, also on a cursor failure (then the ring's current anchor).
    #[serde(default)]
    pub events: Option<EventBatch>,
    /// `keymap.list`: what `keymap.conf` resolved and what the menu carries.
    #[serde(default)]
    pub keymap: Option<ControlKeymap>,
    /// `hooks.list`: the hooks and their live state.
    #[serde(default)]
    pub hooks: Option<ControlHooks>,
    /// `pick.result`: the current or terminal picker outcome.
    #[serde(default)]
    pub pick: Option<ControlPickResult>,
    /// `ask.result`: the current or terminal dialog outcome.
    #[serde(default)]
    pub ask: Option<ControlAskResult>,
    /// `surface.cursor`: the cursor's column.
    #[serde(default)]
    pub cursor: Option<ControlCursor>,
    /// `restore.mode`: the restore-mode policy (`live.list` repeats it as its header).
    #[serde(default)]
    pub restore: Option<ControlRestoreStatus>,
    /// `live.list`: the daemons joined against the panes that claim them (upstream's `zmx` key, renamed).
    #[serde(default)]
    pub live: Option<ControlLiveInventory>,
    /// `live.tree`: the attachable sessions, here or on another machine.
    #[serde(default)]
    pub remote: Option<ControlRemoteTree>,
    /// `live.reset`: what was confirmed for the next launch.
    #[serde(default)]
    pub live_reset: Option<ControlLiveResetStatus>,
    /// `session.overlay.open --html|--url`: the page opened, which `session.overlay.result --page` reads back.
    #[serde(default, rename = "pageID")]
    pub page_id: Option<String>,
    /// `session.overlay.result --page`: the page's selector outcome.
    #[serde(default)]
    pub page_outcome: Option<ControlHtmlPageOutcome>,
    /// `session.restart`: the replaced shells.
    #[serde(default)]
    pub restart: Option<ControlRestartReceipt>,
}

/// The restore-mode policy `restore.mode` reads (upstream's `ControlRestoreStatus`). The modes travel as raw strings,
/// so a client reading a mode a newer app wrote prints it rather than mistaking it for `none`.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRestoreStatus {
    /// What `settings.json` holds, and so what the next launch requests.
    pub configured: String,
    /// What this process requested at launch.
    pub requested_at_launch: String,
    /// What this launch got: `none` when live was requested but unavailable.
    pub active: String,
    /// `configured` differs from `requestedAtLaunch`.
    pub restart_required: bool,
    /// Why live fell back; present only when live was requested and refused.
    #[serde(default)]
    pub unavailable_reason: Option<String>,
}

/// One row of `live.list` (upstream's `ControlZmxEntry`): a daemon, a pane that claims one, or both. State and
/// observation travel as raw strings, so a state a newer app adds fails one field rather than the whole response.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLiveEntry {
    pub daemon: String,
    /// `claimed`, `orphan`, `unknown`, `conflicted`, `pendingClose` or `foreign`.
    pub state: String,
    /// `running`, `unreadable` or `absent`.
    pub observation: String,
    /// Present only for a daemon observed running.
    #[serde(default)]
    pub clients: Option<u32>,
    #[serde(default, rename = "leaderPID")]
    pub leader_pid: Option<i32>,
    #[serde(default, rename = "windowID")]
    pub window_id: Option<String>,
    /// Absent for an unindexed window, whose name lives only in the index it is missing from.
    #[serde(default)]
    pub window_name: Option<String>,
    /// `open`, `closed` or `unindexed`.
    #[serde(default)]
    pub window_state: Option<String>,
    #[serde(default, rename = "workspaceID")]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_name: Option<String>,
    #[serde(default, rename = "sessionID")]
    pub session_id: Option<String>,
    #[serde(default)]
    pub session_name: Option<String>,
    /// `left` or `right`.
    #[serde(default)]
    pub pane: Option<String>,
    /// True when the daemon was created before the launch's recorded live-session build change; omitted otherwise.
    #[serde(default)]
    pub outdated: Option<bool>,
}

/// What a caller on another machine needs to reach these daemons (upstream's `ControlZmxEndpoint`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLiveEndpoint {
    /// The live-session executable this instance runs.
    pub executable: String,
    /// The socket directory its daemons live under.
    pub socket_directory: String,
}

/// One pane of an attachable session (upstream's `ControlRemotePane`).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRemotePane {
    /// `left` or `right`, the local pane vocabulary.
    pub pane: String,
    pub daemon: String,
    /// The argv the far side reports running in this pane; absent for a plain idle shell.
    #[serde(default)]
    pub foreground: Option<Vec<String>>,
}

/// A session whose every pane resolves to a live daemon (upstream's `ControlRemoteSession`); one whose panes did not
/// all resolve is absent rather than partial.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRemoteSession {
    pub id: String,
    pub name: String,
    #[serde(rename = "windowID")]
    pub window_id: String,
    pub window_name: String,
    #[serde(rename = "workspaceID")]
    pub workspace_id: String,
    pub workspace_name: String,
    /// The far side's own note of what the session is for.
    #[serde(default)]
    pub context: Option<String>,
    pub cwd: String,
    /// The divider direction, present only for a session with a split.
    #[serde(default)]
    pub split_axis: Option<String>,
    pub panes: Vec<ControlRemotePane>,
}

/// `live.tree [HOST]` (upstream's `ControlRemoteTree`): every open window's attachable sessions and the one endpoint
/// they attach through.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlRemoteTree {
    /// The ssh destination the requesting app was given, stamped on after decoding; absent from the local form.
    #[serde(default)]
    pub host: Option<String>,
    pub endpoint: ControlLiveEndpoint,
    pub sessions: Vec<ControlRemoteSession>,
    /// The presentation protocol version this app speaks; absent from an app that predates the stream.
    #[serde(default)]
    pub presentation: Option<i64>,
}

impl ControlRemoteTree {
    /// This answer with the ssh destination the requesting app was given (upstream's `stamped(host:)`).
    #[must_use]
    pub fn stamped(self, host: &str) -> Self {
        Self {
            host: Some(host.to_owned()),
            ..self
        }
    }
}

/// `live.list` (upstream's `ControlZmxInventory`): the restore status as the header, then one row per daemon and per
/// pane expecting one.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLiveInventory {
    pub restore: ControlRestoreStatus,
    pub inventory_complete: bool,
    #[serde(default)]
    pub endpoint: Option<ControlLiveEndpoint>,
    /// The Live sessions reset read-back the tree carries too; omitted while nothing is pending and no launch
    /// consumed a marker.
    #[serde(default)]
    pub live_reset: Option<serde_json::Value>,
    pub entries: Vec<ControlLiveEntry>,
}

/// `live.reset`'s acknowledgement (upstream's `ControlLiveResetStatus`): what was confirmed for the next launch.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLiveResetStatus {
    pub sessions: u32,
    pub panes: u32,
    pub pending: bool,
    /// How many of `sessions` predate the recorded live-session build change; omitted when none.
    #[serde(default)]
    pub outdated: Option<u32>,
}

/// What a successful `session.restart` proves: the pane's shell was replaced (upstream's `ControlRestartReceipt`). The
/// pids are the pane's root shells, not the program the caller's line starts.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRestartReceipt {
    /// The pane's stable token.
    #[serde(rename = "paneID")]
    pub pane_id: String,
    pub old_pid: i32,
    pub new_pid: i32,
    /// The foreground program a restart without a command asked the new shell to run; absent when the caller named
    /// the line. What was requested, not proof the program started.
    #[serde(default)]
    pub replayed_argv: Option<Vec<String>>,
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

    /// `{"count": n}`: a reload's diagnostic count.
    #[must_use]
    pub fn count(count: i64) -> Self {
        Self {
            count: Some(count),
            ..Self::default()
        }
    }

    /// `{"keymap": {...}}`.
    #[must_use]
    pub fn keymap(keymap: ControlKeymap) -> Self {
        Self {
            keymap: Some(keymap),
            ..Self::default()
        }
    }

    /// `{"hooks": {...}}`.
    #[must_use]
    pub fn hooks(hooks: ControlHooks) -> Self {
        Self {
            hooks: Some(hooks),
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

    /// `{"id": id, "width": width, "height": height}`: a window's applied size.
    #[must_use]
    pub fn sized(id: impl Into<String>, width: i64, height: i64) -> Self {
        Self {
            id: Some(id.into()),
            width: Some(width),
            height: Some(height),
            ..Self::default()
        }
    }

    /// `{"text": text}`.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Self::default()
        }
    }

    /// `{"sidebarWidth": width}`.
    #[must_use]
    pub fn sidebar_width(width: f64) -> Self {
        Self {
            sidebar_width: Some(width),
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

/// The state or terminal outcome `pick.result` reports (upstream's `ControlPickOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlPickOutcome {
    Pending,
    Picked,
    Custom,
    Cancelled,
}

/// `pick.result`'s nested payload (upstream's `ControlPickResult`).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlPickResult {
    pub result: ControlPickOutcome,
    /// The picked item's id; absent for a custom answer and without an answer.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// The picked item's zero-based position in the caller's order.
    #[serde(default)]
    pub index: Option<i64>,
    /// The typed text of a custom answer.
    #[serde(default)]
    pub query: Option<String>,
}

impl ControlPickResult {
    /// A result carrying only its outcome.
    #[must_use]
    pub fn outcome(result: ControlPickOutcome) -> Self {
        Self {
            result,
            id: None,
            label: None,
            index: None,
            query: None,
        }
    }
}

/// The state or terminal outcome `ask.result` reports (upstream's `ControlAskOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlAskOutcome {
    /// The dialog awaits an answer.
    Pending,
    /// The dialog resolved to a caller-provided button.
    Answered,
    /// The user dismissed the dialog (Esc, Ctrl+W).
    Escaped,
    /// The dialog was cancelled administratively.
    Cancelled,
}

/// `ask.result`'s nested payload (upstream's `ControlAskResult`).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlAskResult {
    pub result: ControlAskOutcome,
    /// The answered button's id; absent without a button answer.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// The answered button's zero-based position in the caller's order.
    #[serde(default)]
    pub index: Option<i64>,
    /// Why a cancelled dialog ended, absent for an ordinary cancel.
    #[serde(default)]
    pub reason: Option<String>,
}

impl ControlAskResult {
    /// A result carrying only its outcome.
    #[must_use]
    pub fn outcome(result: ControlAskOutcome) -> Self {
        Self {
            result,
            id: None,
            label: None,
            index: None,
            reason: None,
        }
    }
}

/// Where a page stands as a selector (upstream's `ControlHtmlPageOutcomeState`): still open, answered, or closed
/// without an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlHtmlPageOutcomeState {
    Pending,
    Submitted,
    Dismissed,
}

/// `session.overlay.result --page`'s payload (upstream's `ControlHtmlPageOutcome`); `value` only for a submitted page,
/// and it may be empty.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlHtmlPageOutcome {
    #[serde(rename = "pageID")]
    pub page_id: String,
    pub outcome: ControlHtmlPageOutcomeState,
    #[serde(default)]
    pub value: Option<String>,
}

/// A surface's cursor position (upstream's `ControlCursor`): the column only, zero-based from the grid's left edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlCursor {
    pub column: i64,
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
    fn pick_and_ask_outcomes_nest_with_their_fields_in_upstreams_order() {
        let result = ControlResult {
            pick: Some(ControlPickResult {
                id: Some("b".into()),
                label: Some("Beta".into()),
                index: Some(1),
                ..ControlPickResult::outcome(ControlPickOutcome::Picked)
            }),
            ..ControlResult::default()
        };
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            r#"{"pick":{"result":"picked","id":"b","label":"Beta","index":1}}"#
        );
        let ask = ControlResult {
            ask: Some(ControlAskResult::outcome(ControlAskOutcome::Escaped)),
            ..ControlResult::default()
        };
        assert_eq!(serde_json::to_string(&ask).unwrap(), r#"{"ask":{"result":"escaped"}}"#);
        let decoded: ControlResult =
            serde_json::from_str(r#"{"ask":{"result":"cancelled","reason":"presentation-lost"}}"#).unwrap();
        assert_eq!(
            decoded.ask.and_then(|ask| ask.reason).as_deref(),
            Some("presentation-lost")
        );
    }

    #[test]
    fn theme_fields_keep_upstreams_order() {
        let result = ControlResult {
            sidebar_width: Some(200.0),
            dark: Some("Nord".to_owned()),
            light: Some("Dracula".to_owned()),
            sync: Some(true),
            themes: Some(vec!["Dracula".to_owned(), "Nord".to_owned()]),
            theme: Some("Nord".to_owned()),
            ..ControlResult::default()
        };
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            r#"{"theme":"Nord","themes":["Dracula","Nord"],"sidebarWidth":200.0,"sync":true,"light":"Dracula","dark":"Nord"}"#
        );
    }

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
        assert_eq!(
            serde_json::to_string(&ControlResult::sized("A", 900, 600)).unwrap(),
            r#"{"id":"A","width":900,"height":600}"#
        );
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

    /// Upstream's `SessionRestartTests`: the receipt rides last, with upstream's key spellings.
    #[test]
    fn a_restart_receipt_round_trips_and_a_replay_carries_its_argv() {
        let receipt = ControlRestartReceipt {
            pane_id: "token".to_owned(),
            old_pid: 11,
            new_pid: 22,
            replayed_argv: None,
        };
        let result = ControlResult {
            id: Some("9f3c".to_owned()),
            text: Some("restarted".to_owned()),
            pane: Some("right".to_owned()),
            restart: Some(receipt.clone()),
            ..ControlResult::default()
        };
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            r#"{"id":"9f3c","text":"restarted","pane":"right","restart":{"paneID":"token","oldPid":11,"newPid":22}}"#
        );
        let back: ControlResult = serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(back.restart, Some(receipt));
        assert!(
            !serde_json::to_string(&ControlResult::id("9f3c"))
                .unwrap()
                .contains("restart")
        );

        let replayed = ControlRestartReceipt {
            pane_id: "token".to_owned(),
            old_pid: 11,
            new_pid: 22,
            replayed_argv: Some(vec!["sh".into(), "-c".into(), "a b".into(), String::new()]),
        };
        let text = serde_json::to_string(&replayed).unwrap();
        assert!(text.ends_with(r#""replayedArgv":["sh","-c","a b",""]}"#), "{text}");
        assert_eq!(serde_json::from_str::<ControlRestartReceipt>(&text).unwrap(), replayed);
        let old: ControlRestartReceipt = serde_json::from_str(r#"{"paneID":"token","oldPid":11,"newPid":22}"#).unwrap();
        assert_eq!(old.replayed_argv, None);
    }
}
