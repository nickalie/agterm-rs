//! Run counts of the `keymap.conf` commands, keyed by name (upstream's `CustomCommandUsage.swift`): what the
//! custom-commands popover's most-used section reads. The name is the durable key, since a command's id is minted
//! on every parse and the parser refuses a second command with a name already taken.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::CustomCommand;

/// The file format's version; another version reads as empty.
pub const USAGE_VERSION: i64 = 1;

/// The counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomCommandUsage {
    pub version: i64,
    pub counts: BTreeMap<String, i64>,
}

impl Default for CustomCommandUsage {
    fn default() -> Self {
        Self {
            version: USAGE_VERSION,
            counts: BTreeMap::new(),
        }
    }
}

impl CustomCommandUsage {
    /// Counts one run of `command`.
    pub fn record(&mut self, command: &CustomCommand) {
        *self.counts.entry(command.name.clone()).or_default() += 1;
    }

    /// The most-run of `commands`, at most `limit`, by descending count with ties in `commands` order; a command
    /// never run is left out.
    #[must_use]
    pub fn most_used<'a>(&self, commands: &'a [CustomCommand], limit: usize) -> Vec<&'a CustomCommand> {
        let mut ranked: Vec<(i64, usize, &CustomCommand)> = commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| {
                let count = self.counts.get(&command.name).copied().unwrap_or(0);
                (count > 0).then_some((count, index, command))
            })
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        ranked.into_iter().take(limit).map(|(_, _, command)| command).collect()
    }
}

/// `<state dir>/custom-command-usage.json`, read tolerantly (missing, corrupt or another version reads empty) and
/// written atomically.
#[derive(Debug, Clone)]
pub struct UsageStore {
    path: PathBuf,
}

impl UsageStore {
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join("custom-command-usage.json"),
        }
    }

    /// The counts on disk.
    #[must_use]
    pub fn load(&self) -> CustomCommandUsage {
        std::fs::read(&self.path)
            .ok()
            .and_then(|data| serde_json::from_slice::<CustomCommandUsage>(&data).ok())
            .filter(|usage| usage.version == USAGE_VERSION)
            .unwrap_or_default()
    }

    /// Counts one run of `command` and writes the result; a failed write is logged.
    pub fn record(&self, command: &CustomCommand) {
        let mut usage = self.load();
        usage.record(command);
        let written = serde_json::to_vec_pretty(&usage)
            .map_err(std::io::Error::other)
            .and_then(|bytes| crate::persist::json::write_atomic(&self.path, &bytes));
        if let Err(err) = written {
            tracing::warn!(%err, "could not save custom command usage");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> CustomCommand {
        CustomCommand::new(name, "true", "")
    }

    #[test]
    fn most_used_ranks_by_count_then_file_order() {
        let commands = [named("a"), named("b"), named("c"), named("d")];
        let mut usage = CustomCommandUsage::default();
        for name in ["c", "b", "c", "a", "b"] {
            usage.record(&named(name));
        }
        usage.counts.insert("gone".to_owned(), 9);
        let names: Vec<&str> = usage.most_used(&commands, 2).iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["b", "c"]);
        assert_eq!(usage.most_used(&commands, 10).len(), 3);
    }

    #[test]
    fn the_store_counts_across_loads_and_reads_garbage_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = UsageStore::new(dir.path());
        store.record(&named("a"));
        store.record(&named("a"));
        assert_eq!(store.load().counts["a"], 2);
        std::fs::write(dir.path().join("custom-command-usage.json"), b"{nope").unwrap();
        assert_eq!(store.load(), CustomCommandUsage::default());
        std::fs::write(
            dir.path().join("custom-command-usage.json"),
            br#"{"version":2,"counts":{"a":1}}"#,
        )
        .unwrap();
        assert!(store.load().counts.is_empty());
    }
}
