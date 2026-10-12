//! Event kinds of the app-run event ring (`events.read`).

use std::fmt;

use serde_with::{DeserializeFromStr, SerializeDisplay};

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

#[cfg(test)]
mod tests {
    use strum::IntoEnumIterator;

    use super::*;

    #[test]
    fn kinds_round_trip_through_json() {
        for kind in EventKind::iter() {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.wire_name()));
            assert_eq!(serde_json::from_str::<EventKind>(&json).unwrap(), kind);
        }
        assert!(serde_json::from_str::<EventKind>(r#""session""#).is_err());
    }
}
