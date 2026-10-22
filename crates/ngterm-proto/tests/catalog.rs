//! The protocol's wire names equal the conformance catalog's `commands` and `events`.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::Path;

use ngterm_proto::{Command, EventKind};
use serde::Deserialize;
use strum::IntoEnumIterator;

#[derive(Deserialize)]
struct Catalog {
    commands: BTreeMap<String, Entry>,
    events: BTreeMap<String, Entry>,
}

#[derive(Deserialize)]
struct Entry {
    status: String,
}

fn load_catalog() -> Result<Catalog, Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/catalog.yaml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(serde_norway::from_str(&text)?)
}

/// Names the catalog still lists as part of upstream's protocol.
fn listed(entries: &BTreeMap<String, Entry>) -> BTreeSet<String> {
    entries
        .iter()
        .filter(|(_, entry)| entry.status != "removed-upstream")
        .map(|(name, _)| renamed(name))
        .collect()
}

/// A catalog name as this build spells it: upstream's `zmx.*` group is `live.*` here (conformance/docs/rust-build.md,
/// The `live` group); every other name is upstream's.
fn renamed(name: &str) -> String {
    name.strip_prefix("zmx.")
        .map_or_else(|| name.to_owned(), |rest| format!("live.{rest}"))
}

#[test]
fn upstreams_zmx_group_is_the_live_group() -> Result<(), Box<dyn Error>> {
    let catalog = load_catalog()?;
    let upstream: BTreeSet<&str> = catalog
        .commands
        .keys()
        .filter_map(|name| name.strip_prefix("zmx."))
        .collect();
    let ours: BTreeSet<String> = Command::all().into_iter().map(|c| c.wire_name().to_owned()).collect();
    let live: BTreeSet<&str> = ours.iter().filter_map(|name| name.strip_prefix("live.")).collect();
    assert_eq!(live, upstream);
    assert!(ours.iter().all(|name| !name.contains("zmx")), "{ours:?}");
    Ok(())
}

#[test]
fn command_wire_names_equal_catalog_commands() -> Result<(), Box<dyn Error>> {
    let catalog = load_catalog()?;
    let ours: BTreeSet<String> = Command::all().into_iter().map(|c| c.wire_name().to_owned()).collect();
    assert_eq!(ours, listed(&catalog.commands));
    Ok(())
}

#[test]
fn every_catalog_command_parses_to_its_own_name() -> Result<(), Box<dyn Error>> {
    let catalog = load_catalog()?;
    for name in listed(&catalog.commands) {
        let command: Command = name.parse()?;
        assert_eq!(command.wire_name(), name);
    }
    Ok(())
}

#[test]
fn event_kinds_equal_catalog_events() -> Result<(), Box<dyn Error>> {
    let catalog = load_catalog()?;
    let ours: BTreeSet<String> = EventKind::iter().map(|k| k.wire_name().to_owned()).collect();
    assert_eq!(ours, listed(&catalog.events));
    Ok(())
}
