use super::{Capability, Source, Value, XtgettcapError, hexencode, replace, string_response};

const NAMES: &[&str] = &["ghostty", "xterm-ghostty", "Ghostty"];

const fn cap(name: &'static str, value: Value) -> Capability {
    Capability { name, value }
}

// Ghostty's `test "xtgettcap map"`.
#[test]
fn xtgettcap_map() {
    const SRC: Source = Source {
        names: NAMES,
        capabilities: &[
            cap("am", Value::Boolean),
            cap("colors", Value::Numeric(256)),
            cap("kx", Value::String("^?")),
            cap("kbs", Value::String("^H")),
            cap("kf1", Value::String(r"\EOP")),
            cap("Smulx", Value::String(r"\E[4:%p1%dm")),
        ],
    };

    let map = SRC.xtgettcap_map().unwrap();
    let get = |name: &str| map.get(hexencode(name.as_bytes()).as_slice()).unwrap().as_slice();
    assert_eq!(get("am"), b"\x1bP1+r616D\x1b\\");
    assert_eq!(get("kx"), b"\x1bP1+r6B78=7F\x1b\\");
    assert_eq!(get("kbs"), b"\x1bP1+r6B6273=08\x1b\\");
    assert_eq!(get("kf1"), b"\x1bP1+r6B6631=1B4F50\x1b\\");
    assert_eq!(get("Smulx"), b"\x1bP1+r536D756C78=5C455B343A25703125646D\x1b\\");
}

// Ghostty's `test "encode"`.
#[test]
fn encode() {
    const SRC: Source = Source {
        names: NAMES,
        capabilities: &[
            cap("am", Value::Boolean),
            cap("ccc", Value::Canceled),
            cap("colors", Value::Numeric(256)),
            cap("bel", Value::String("^G")),
        ],
    };

    let expected = "ghostty|xterm-ghostty|Ghostty,\n\tam,\n\tccc@,\n\tcolors#256,\n\tbel=^G,\n";
    assert_eq!(SRC.encode(), expected);
}

#[test]
fn the_map_answers_tn_co_and_rgb_besides_the_capabilities() {
    const SRC: Source = Source {
        names: NAMES,
        capabilities: &[cap("am", Value::Boolean)],
    };
    let map = SRC.xtgettcap_map().unwrap();
    assert_eq!(map.len(), 4);
    // TN names the first name, "ghostty"
    assert_eq!(map[b"544E".as_slice()], b"\x1bP1+r544E=67686F73747479\x1b\\");
    assert_eq!(map[b"436F".as_slice()], b"\x1bP1+r436F=323536\x1b\\");
    assert_eq!(map[b"524742".as_slice()], b"\x1bP1+r524742=38\x1b\\");
}

#[test]
fn the_keys_are_uppercase_hex_names() {
    const SRC: Source = Source {
        names: NAMES,
        capabilities: &[cap("kf1", Value::String(r"\EOP"))],
    };
    let map = SRC.xtgettcap_map().unwrap();
    assert!(map.contains_key(b"6B6631".as_slice()));
    // the DCS handler uppercases a request before the lookup; the map holds only the uppercase form
    assert!(!map.contains_key(b"6b6631".as_slice()));
    // nor the raw name
    assert!(!map.contains_key(b"kf1".as_slice()));
}

#[test]
fn a_canceled_capability_has_no_answer() {
    const SRC: Source = Source {
        names: NAMES,
        capabilities: &[cap("am", Value::Boolean), cap("ccc", Value::Canceled)],
    };
    assert_eq!(SRC.xtgettcap_map(), Err(XtgettcapError::Canceled("ccc")));
    assert_eq!(
        XtgettcapError::Canceled("ccc").to_string(),
        "capability ccc: canceled not handled yet"
    );
}

#[test]
fn a_control_character_it_cannot_encode_has_no_answer() {
    const SRC: Source = Source {
        names: NAMES,
        capabilities: &[cap("bad", Value::String("a^G"))],
    };
    for bad in [r"\E^G", "x^G", "^", r"^\E"] {
        assert_eq!(string_response(bad), None, "{bad}");
    }
    assert_eq!(SRC.xtgettcap_map(), Err(XtgettcapError::ControlChar("bad")));
}

#[test]
fn a_string_with_parameters_is_reported_as_written() {
    assert_eq!(string_response(r"\E[%p1%d q").unwrap(), br"\E[%p1%d q");
    // even with a caret in it
    assert_eq!(string_response("%p1%c^G").unwrap(), b"%p1%c^G");
}

#[test]
fn a_string_without_parameters_is_encoded() {
    assert_eq!(string_response(r"\E[H\E[2J").unwrap(), b"\x1b[H\x1b[2J");
    assert_eq!(string_response("^G").unwrap(), b"\x07");
    assert_eq!(string_response("^?").unwrap(), b"\x7f");
    // other backslash escapes stay as written (Ghostty encodes \E only)
    assert_eq!(string_response(r"\r").unwrap(), br"\r");
    assert_eq!(string_response("").unwrap(), b"");
}

#[test]
fn a_boolean_or_empty_string_answers_without_a_value() {
    const SRC: Source = Source {
        names: NAMES,
        capabilities: &[cap("am", Value::Boolean), cap("e", Value::String(""))],
    };
    let map = SRC.xtgettcap_map().unwrap();
    assert_eq!(map[b"616D".as_slice()], b"\x1bP1+r616D\x1b\\");
    assert_eq!(map[b"65".as_slice()], b"\x1bP1+r65\x1b\\");
}

#[test]
fn hexencode_is_uppercase_and_two_digits_per_byte() {
    assert_eq!(hexencode(b""), b"");
    assert_eq!(hexencode(b"\x00\x0f\xab\xff"), b"000FABFF");
    assert_eq!(hexencode(b"TN"), b"544E");
}

#[test]
fn replace_replaces_every_occurrence_left_to_right() {
    assert_eq!(replace(b"aXbXX", b"X", b"yy"), b"ayybyyyy");
    assert_eq!(replace(b"aaa", b"aa", b"b"), b"ba");
    assert_eq!(replace(b"abc", b"", b"z"), b"abc");
    assert_eq!(replace(b"", b"a", b"z"), b"");
}
