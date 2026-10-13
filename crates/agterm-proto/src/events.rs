//! The app-run event ring's wire types (`events.read`, upstream's `ControlEvents.swift`): the event kinds, one event
//! with its kind-specific payload, and the page a read returns.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_with::{DeserializeFromStr, SerializeDisplay, skip_serializing_none};

/// The kind of one ring event, by its wire name.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::EnumIter,
    SerializeDisplay,
    DeserializeFromStr,
)]
pub enum EventKind {
    #[strum(serialize = "status")]
    Status,
    #[strum(serialize = "notify")]
    Notify,
    #[strum(serialize = "session.created")]
    SessionCreated,
    #[strum(serialize = "session.closed")]
    SessionClosed,
    #[strum(serialize = "session.selected")]
    SessionSelected,
    #[strum(serialize = "tree.changed")]
    TreeChanged,
    #[strum(serialize = "pane.split")]
    PaneSplit,
    #[strum(serialize = "pane.scratch")]
    PaneScratch,
    #[strum(serialize = "remote.opened")]
    RemoteOpened,
    #[strum(serialize = "remote.closed")]
    RemoteClosed,
}

impl EventKind {
    /// The wire name, e.g. `session.created`.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        self.into()
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wire_name())
    }
}

/// Kind-specific event data (upstream's `ControlEventPayload`): every field optional and absent when unset, so each
/// kind keeps one stable object shape, in upstream's declaration order.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EventPayload {
    /// The session's display name at emit time.
    #[serde(default)]
    pub name: Option<String>,
    /// `status`: the new status; `pane.split`/`pane.scratch`: `shown` or `hidden`.
    #[serde(default)]
    pub status: Option<String>,
    /// `status`: the pane that set it.
    #[serde(default)]
    pub pane: Option<String>,
    /// `status`: whether the glyph blinks.
    #[serde(default)]
    pub blink: Option<bool>,
    /// `status`: the glyph's color.
    #[serde(default)]
    pub color: Option<String>,
    /// `status`: the per-call glyph silhouette, absent for the default one.
    #[serde(default)]
    pub shape: Option<String>,
    /// What the event replaced: on `status` the status before the write, on `session.selected` the id of the session
    /// that lost the selection (absent when there was none).
    #[serde(default)]
    pub previous: Option<String>,
    /// `notify`: the effective title.
    #[serde(default)]
    pub title: Option<String>,
    /// `notify`: the body.
    #[serde(default)]
    pub body: Option<String>,
    /// `remote.opened`/`remote.closed`: the ssh destination.
    #[serde(default)]
    pub host: Option<String>,
}

/// One immutable ring entry (upstream's `ControlEvent`). The identities are upper-case UUIDs, absent when the event
/// has none (`tree.changed` carries only its window); the payload is always present, empty or not.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlEvent {
    /// The app-run sequence, from 1.
    pub seq: u64,
    /// When the ring took it, in seconds since the Unix epoch.
    pub ts: f64,
    pub kind: EventKind,
    #[serde(default)]
    pub window: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    pub payload: EventPayload,
}

/// A page of a ring read (upstream's `ControlEventBatch`). `next` is the sequence through which the ring scanned,
/// filtered-out entries included: the cursor's next `after`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventBatch {
    /// The app run's id (upper-case UUID).
    pub run: String,
    pub next: u64,
    pub items: Vec<ControlEvent>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use strum::IntoEnumIterator;

    use super::*;

    /// `agtermctl events --json` re-encodes what it read: a timestamp must come back as the same double, which
    /// `serde_json`'s parser only guarantees with its `float_roundtrip` feature.
    #[test]
    fn a_timestamp_survives_a_decode_and_reencode_bit_for_bit() {
        let line = r#"{"seq":10,"ts":1791864251.0450583,"kind":"status","payload":{}}"#;
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        let event: ControlEvent = serde_json::from_value(value).unwrap();
        assert!(
            serde_json::to_string(&event)
                .unwrap()
                .contains(r#""ts":1791864251.0450583"#)
        );
    }

    #[test]
    fn kinds_round_trip_through_json() {
        for kind in EventKind::iter() {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.wire_name()));
            assert_eq!(serde_json::from_str::<EventKind>(&json).unwrap(), kind);
        }
        assert!(serde_json::from_str::<EventKind>(r#""session""#).is_err());
    }

    #[test]
    fn unset_identities_and_payload_fields_are_absent() {
        let event = ControlEvent {
            seq: 5,
            ts: 5.5,
            kind: EventKind::TreeChanged,
            window: Some("win".to_owned()),
            workspace: None,
            session: None,
            payload: EventPayload::default(),
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"seq":5,"ts":5.5,"kind":"tree.changed","window":"win","payload":{}}"#
        );
    }

    /// Upstream's `everyEventKindMatchesGoldenWireShape` status case, in declaration order rather than sorted.
    #[test]
    fn a_status_event_carries_its_payload_in_upstreams_field_order() {
        let event = ControlEvent {
            seq: 1,
            ts: 1.25,
            kind: EventKind::Status,
            window: Some("win".to_owned()),
            workspace: Some("work".to_owned()),
            session: Some("sess".to_owned()),
            payload: EventPayload {
                name: Some("api".to_owned()),
                status: Some("blocked".to_owned()),
                pane: Some("right".to_owned()),
                blink: Some(true),
                color: Some("#aabbcc".to_owned()),
                shape: Some("triangle".to_owned()),
                ..EventPayload::default()
            },
        };
        let text = serde_json::to_string(&event).unwrap();
        assert_eq!(
            text,
            r##"{"seq":1,"ts":1.25,"kind":"status","window":"win","workspace":"work","session":"sess","payload":{"name":"api","status":"blocked","pane":"right","blink":true,"color":"#aabbcc","shape":"triangle"}}"##
        );
        assert_eq!(serde_json::from_str::<ControlEvent>(&text).unwrap(), event);
    }

    #[test]
    fn a_batch_round_trips_and_an_event_needs_its_payload() {
        let wire = json!({"run": "CBB5E3D0-7A9B-4C96-9EA2-18B14380DDB1", "next": 42, "items": [
            {"seq": 42, "ts": 42.25, "kind": "notify", "session": "S",
             "payload": {"name": "api", "title": "Done", "body": "ok"}}
        ]});
        let batch: EventBatch = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(batch.items[0].payload.title.as_deref(), Some("Done"));
        assert_eq!(serde_json::to_value(&batch).unwrap(), wire);
        assert!(serde_json::from_value::<ControlEvent>(json!({"seq": 1, "ts": 0, "kind": "status"})).is_err());
    }
}
