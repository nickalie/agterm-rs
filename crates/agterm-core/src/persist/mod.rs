//! Persistence in upstream's on-disk formats, which the Linux fork shares: the window index `windows.json`, each
//! window's `windows/<ID>.json` snapshot, the legacy single-window `workspaces.json`, and `settings.json`, all in the
//! state directory.
//!
//! - [`json`]: Foundation's pretty, key-sorted JSON and its atomic write.
//! - [`snapshot`]: the snapshot and index types, with upstream's lossy decoding.
//! - [`settings`]: the app settings and their store.
//! - [`library`]: the window library's persistence: the launch bootstrap (index, orphan recovery, legacy migration,
//!   seeding), loading a closed window, and the saves the model asks for, the debounced ones through the main loop.

pub mod json;
pub mod library;
pub mod settings;
pub mod snapshot;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub use library::{DebouncedSave, LibraryPersistence, SAVE_DEBOUNCE, arm_debounced_saves};
pub use settings::{AppSettings, RestoreMode, SettingsStore};
pub use snapshot::{PersistenceStore, SessionSnapshot, Snapshot, WindowEntry, WindowsIndex, WorkspaceSnapshot};

/// Decodes an optional field LOSSILY, as upstream's `(try? c.decodeIfPresent(...)) ?? nil`: a present value that does
/// not decode as `T` (a wrong type, a malformed id, an unknown enum name) reads as `None` instead of failing the
/// whole document. Pair it with `#[serde(default)]` so a missing key reads as `None` too.
pub(crate) fn lossy<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).ok())
}

/// Ids as Foundation encodes a `UUID`: its upper-case `uuidString`.
pub(crate) mod upper {
    use serde::Serializer;
    use uuid::Uuid;

    use crate::pane_env::uuid_string;

    pub(crate) fn uuid<S: Serializer>(id: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&uuid_string(*id))
    }

    #[expect(clippy::ref_option, reason = "serde's serialize_with passes the field by reference")]
    pub(crate) fn option<S: Serializer>(id: &Option<Uuid>, serializer: S) -> Result<S::Ok, S::Error> {
        match id {
            Some(id) => serializer.serialize_str(&uuid_string(*id)),
            None => serializer.serialize_none(),
        }
    }

    #[expect(clippy::ref_option, reason = "serde's serialize_with passes the field by reference")]
    pub(crate) fn option_vec<S: Serializer>(ids: &Option<Vec<Uuid>>, serializer: S) -> Result<S::Ok, S::Error> {
        match ids {
            Some(ids) => serializer.collect_seq(ids.iter().map(|id| uuid_string(*id))),
            None => serializer.serialize_none(),
        }
    }
}

/// Parses a window file's stem as `UUID(uuidString:)` does: the 36-character hyphenated form, either case.
#[must_use]
pub fn parse_uuid_string(text: &str) -> Option<uuid::Uuid> {
    if text.len() != 36 {
        return None;
    }
    uuid::Uuid::try_parse(text).ok()
}
