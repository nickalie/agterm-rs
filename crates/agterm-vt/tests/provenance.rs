//! The provenance constants against `build/pins.env` and against each other.

use agterm_vt::provenance::{
    LIBGHOSTTY_VT_REPO, LIBGHOSTTY_VT_REV, RUSTTY_GHOSTTY_REV, RUSTTY_REPO, RUSTTY_REV, RUSTTY_UNICODE_GHOSTTY_REV,
};

const PINS: &str = include_str!("../../../build/pins.env");

/// The value of `NAME="value"` in `build/pins.env`, `None` when the line is missing.
fn pin(name: &str) -> Option<&'static str> {
    PINS.lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix("=\"")?.strip_suffix('"'))
}

#[test]
fn the_pins_and_the_constants_agree() {
    assert_eq!(pin("LIBGHOSTTY_VT_REPO"), Some(LIBGHOSTTY_VT_REPO));
    assert_eq!(
        pin("LIBGHOSTTY_VT_REV"),
        Some(LIBGHOSTTY_VT_REV),
        "a Ghostty pin move updates agterm-vt's provenance and absorbs the drift in the same change"
    );
    assert_eq!(pin("RUSTTY_REPO"), Some(RUSTTY_REPO));
    assert_eq!(pin("RUSTTY_REV"), Some(RUSTTY_REV));
}

#[test]
fn every_revision_is_a_full_lowercase_commit_hash() {
    for rev in [
        LIBGHOSTTY_VT_REV,
        RUSTTY_REV,
        RUSTTY_GHOSTTY_REV,
        RUSTTY_UNICODE_GHOSTTY_REV,
    ] {
        assert_eq!(rev.len(), 40, "{rev}");
        assert!(rev.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')), "{rev}");
    }
}

#[test]
fn the_followed_revisions_are_distinct() {
    // rustty's oracle and its Unicode tables both predate the pin: the drift between them is real work
    assert_ne!(RUSTTY_GHOSTTY_REV, LIBGHOSTTY_VT_REV);
    assert_ne!(RUSTTY_UNICODE_GHOSTTY_REV, LIBGHOSTTY_VT_REV);
    assert_ne!(RUSTTY_UNICODE_GHOSTTY_REV, RUSTTY_GHOSTTY_REV);
}

#[test]
fn a_missing_pin_reads_as_none() {
    assert_eq!(pin("NO_SUCH_PIN"), None);
    // a prefix of a pin's name is not that pin
    assert_eq!(pin("RUSTTY"), None);
}
