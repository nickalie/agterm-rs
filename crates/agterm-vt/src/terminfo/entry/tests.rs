use std::collections::HashSet;

use super::GHOSTTY;
use crate::terminfo::source::Value;

// Ghostty's `test "encode"`.
#[test]
fn encode() {
    assert!(!GHOSTTY.encode().is_empty());
}

#[test]
fn the_names_are_ghosttys() {
    assert_eq!(GHOSTTY.names, ["xterm-ghostty", "ghostty", "Ghostty"]);
    assert!(
        GHOSTTY
            .encode()
            .starts_with("xterm-ghostty|ghostty|Ghostty,\n\tam,\n\tbce,\n")
    );
    assert!(!GHOSTTY.encode().contains("rustty"));
}

#[test]
fn every_capability_has_an_xtgettcap_answer() {
    // Ghostty's comptime map refuses the entry with a compile error otherwise
    let map = GHOSTTY.xtgettcap_map().unwrap();
    // every capability, plus TN, Co and RGB
    assert_eq!(map.len(), GHOSTTY.capabilities.len() + 3);
}

#[test]
fn the_capability_names_are_unique_and_none_is_canceled() {
    let mut names = HashSet::new();
    for cap in GHOSTTY.capabilities {
        assert!(names.insert(cap.name), "{} twice", cap.name);
        assert_ne!(cap.value, Value::Canceled, "{}", cap.name);
    }
    for queryable in ["TN", "Co", "RGB"] {
        assert!(
            !names.contains(queryable),
            "{queryable} is answered apart from the capabilities"
        );
    }
}

#[test]
fn the_entry_is_ghosttys_at_the_pin() {
    // 15 booleans, 5 numbers, 250 strings at LIBGHOSTTY_VT_REV; the infocmp fixture test holds every value to the
    // entry the Zig build compiled
    let count = |f: fn(&Value) -> bool| GHOSTTY.capabilities.iter().filter(|c| f(&c.value)).count();
    assert_eq!(count(|v| matches!(v, Value::Boolean)), 15);
    assert_eq!(count(|v| matches!(v, Value::Numeric(_))), 5);
    assert_eq!(count(|v| matches!(v, Value::String(_))), 250);
    let first = GHOSTTY.capabilities.first().map(|c| c.name);
    let last = GHOSTTY.capabilities.last().map(|c| (c.name, c.value));
    assert_eq!(first, Some("am"));
    assert_eq!(last, Some(("sc", Value::String(r"\E7"))));
}

#[test]
fn backslashes_are_terminfo_escapes_as_written() {
    // Zig's "\\E" is the two characters `\E`; the Rust entry writes them raw
    let value = |name: &str| GHOSTTY.capabilities.iter().find(|c| c.name == name).map(|c| c.value);
    assert_eq!(value("rs1"), Some(Value::String(r"\E]\E\\\Ec")));
    assert_eq!(
        value("acsc").map(|v| matches!(v, Value::String(s) if s.starts_with(r"++\,\,"))),
        Some(true)
    );
    assert_eq!(value("cr"), Some(Value::String(r"\r")));
    assert_eq!(value("colors"), Some(Value::Numeric(256)));
    assert_eq!(value("Tc"), Some(Value::Boolean));
}
