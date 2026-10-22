//! Ported from upstream's `KeymapTests` store cases: loading `keymap.conf` from disk, plus seeding the starter.

use std::fs;

use super::*;

fn upstream() -> ShippedDefaults {
    ShippedDefaults::upstream()
}

#[test]
fn a_missing_file_is_the_empty_keymap_and_an_existing_one_parses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keymap.conf");
    let (keymap, diagnostics) = load_keymap(&path, &upstream());
    assert_eq!(keymap, Keymap::empty(&upstream()));
    assert!(diagnostics.is_empty());

    fs::write(&path, "command \"Greet\" ctrl+shift+y echo hi\n").unwrap();
    let (keymap, diagnostics) = load_keymap(&path, &upstream());
    assert!(keymap.commands().iter().any(|command| command.name == "Greet"));
    assert!(diagnostics.is_empty());
}

#[test]
fn a_malformed_keymap_reports_its_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keymap.conf");
    fs::write(
        &path,
        "map cmd+d not_an_action\ncommand \"Good\" ctrl+shift+y echo ok\n",
    )
    .unwrap();
    let (keymap, diagnostics) = load_keymap(&path, &upstream());
    assert!(keymap.commands().iter().any(|command| command.name == "Good"));
    assert_eq!(diagnostics, [diagnostic(1, "unknown action 'not_an_action'")]);
}

#[test]
fn an_unreadable_file_is_the_empty_keymap_with_one_whole_file_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keymap.conf");
    fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
    let (keymap, diagnostics) = load_keymap(&path, &upstream());
    assert_eq!(keymap, Keymap::empty(&upstream()));
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].line, 0);
    assert_eq!(
        diagnostics[0].message,
        "could not read keymap.conf: the file isn't valid UTF-8 text"
    );

    // a directory where the file should be exists but cannot be read either.
    let (_, diagnostics) = load_keymap(dir.path(), &upstream());
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.starts_with("could not read keymap.conf: "));
    assert!(
        !diagnostics[0].message.contains("os error"),
        "{}",
        diagnostics[0].message
    );
}

#[test]
fn the_starter_is_written_only_when_no_file_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config").join("ngterm").join("keymap.conf");
    ensure_starter_keymap(&path, &upstream());
    assert_eq!(fs::read_to_string(&path).unwrap(), starter_keymap_conf(&upstream()));

    fs::write(&path, "map cmd+shift+e toggle_split\n").unwrap();
    ensure_starter_keymap(&path, &upstream());
    assert_eq!(fs::read_to_string(&path).unwrap(), "map cmd+shift+e toggle_split\n");
}
