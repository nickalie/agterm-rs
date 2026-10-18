use super::{HAlign, MAX_PAYLOAD_LENGTH, VAlign};
use crate::terminal::osc::{Command, Parser};

// The `kitty_text_sizing.zig` diagnostics, asserted on the full captured record vector.

fn warn(msg: &str) -> crate::log_capture::CapturedRecord {
    (tracing::Level::WARN, "kitty_text_sizing".to_string(), msg.to_string())
}

fn drive(input: &str) -> Vec<crate::log_capture::CapturedRecord> {
    let mut p: Parser = Parser::new(false);
    let cap = crate::log_capture::start();
    for ch in input.bytes() {
        p.next(ch);
    }
    let _ = p.end(Some(0x1b));
    let messages = cap.messages();
    drop(cap);
    messages
}

/// `kitty_text_sizing.zig:87` — no `;` in the captured payload.
///
/// The capture starts AFTER the `66;` introducer, so `66;abc` is the
/// no-semicolon case; a bare `66` never reaches this parser at all (the OSC
/// dispatcher goes `Invalid` on the missing introducer separator).
#[test]
fn a_missing_semicolon_warns() {
    assert_eq!(vec![warn("missing semicolon before payload")], drive("66;abc"));
}

/// `:122`/`:126`/`:131` — the key/value shape diagnostics.
#[test]
fn malformed_key_value_pairs_warn() {
    // `s` alone: `splitn(2, '=')` yields the key and no value.
    assert_eq!(vec![warn("missing value")], drive("66;s;hi"));
    // A two-character key.
    assert_eq!(vec![warn("key must be a single character")], drive("66;sc=2;hi"));
}

/// `:137` — an unrecognised single-character key.
#[test]
fn an_unknown_key_warns_with_the_character() {
    assert_eq!(vec![warn("unknown key: 'z'")], drive("66;z=1;hi"));
}

/// `:96` — a payload over `MAX_PAYLOAD_LENGTH`.
///
/// Needs an ALLOCATING parser: with `Parser::new(false)` the OSC capture is
/// capped at the inline `MAX_BUF` (2048) and the dispatcher goes `Invalid`
/// long before this parser sees the payload — so the test would have passed
/// its `Vec::new()` assertion while never reaching the guard.
#[test]
fn an_over_long_payload_warns() {
    let mut p: Parser = Parser::new(true);
    let mut input = String::from("66;;");
    input.push_str(&"a".repeat(MAX_PAYLOAD_LENGTH + 1));

    let cap = crate::log_capture::start();
    for ch in input.bytes() {
        p.next(ch);
    }
    let out = p.end(Some(0x1b)).is_none();
    let messages = cap.messages();
    drop(cap);

    // Positive reach: the guard's own `return None`.
    assert!(out);
    assert_eq!(vec![warn("payload is too long")], messages);
}

/// The absence half: a well-formed OSC 66 emits NOTHING, and the parse
/// really happened (the positive reach).
#[test]
fn a_well_formed_osc_66_is_silent() {
    let mut p: Parser = Parser::new(false);
    let cap = crate::log_capture::start();
    for ch in b"66;s=2;bobr" {
        p.next(*ch);
    }
    let cmd = p.end(Some(0x1b));
    let messages = cap.messages();
    drop(cap);

    match cmd {
        Some(Command::KittyTextSizing(osc)) => {
            assert_eq!(2u8, osc.scale);
            assert_eq!(b"bobr", osc.text);
        }
        other => panic!(
            "the OSC did not parse ({other:?}), so this test never reached \
             the code whose silence it asserts"
        ),
    }
    assert_eq!(Vec::<crate::log_capture::CapturedRecord>::new(), messages);
}

// No Ghostty test. `update` parses every value with
// `lib.parseInt(u4, value, 10)` (`73768913b`), which rejects `_` digit
// separators and, the type being unsigned, a leading `+` (the
// `std.fmt.parseInt` it replaced accepted both). The field then keeps its
// default, Ghostty's `InvalidValue` path.
#[test]
fn osc_66_values_follow_zig_parse_int_but_refuse_digit_separators() {
    let mut p: Parser = Parser::new(false);
    for ch in b"66;n=12:d=+15;bobr" {
        p.next(*ch);
    }
    match p.end(Some(0x1b)) {
        Some(Command::KittyTextSizing(osc)) => {
            assert_eq!(12u8, osc.numerator);
            assert_eq!(0u8, osc.denominator);
        }
        other => panic!("the OSC did not parse ({other:?})"),
    }

    let mut p: Parser = Parser::new(false);
    for ch in b"66;n=1_2:d=15:s=_2:w=2_;bobr" {
        p.next(*ch);
    }
    match p.end(Some(0x1b)) {
        Some(Command::KittyTextSizing(osc)) => {
            assert_eq!(0u8, osc.numerator);
            assert_eq!(15u8, osc.denominator);
            assert_eq!(1u8, osc.scale);
            assert_eq!(0u8, osc.width);
            assert_eq!(b"bobr", osc.text);
        }
        other => panic!("the OSC did not parse ({other:?})"),
    }
}

#[test]
fn osc_66_numeric_fields_reject_digit_separators() {
    let mut p: Parser = Parser::new(false);
    for ch in "66;s=0_2;text".bytes() {
        p.next(ch);
    }
    match p.end(None).unwrap() {
        Command::KittyTextSizing(osc) => {
            assert_eq!(1u8, osc.scale);
            assert_eq!(0u8, osc.width);
            assert_eq!(0u8, osc.numerator);
            assert_eq!(0u8, osc.denominator);
            assert_eq!(VAlign::Top, osc.valign);
            assert_eq!(HAlign::Left, osc.halign);
            assert_eq!(b"text", osc.text);
        }
        other => panic!("expected kitty_text_sizing, got {other:?}"),
    }
}

#[test]
fn osc_66_empty_parameters() {
    let mut p: Parser = Parser::new(false);

    let input = "66;;bobr";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::KittyTextSizing(_)));
    match cmd {
        Command::KittyTextSizing(osc) => {
            assert_eq!(1u8, osc.scale);
            assert_eq!(b"bobr", osc.text);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_66_single_parameter() {
    let mut p: Parser = Parser::new(false);

    let input = "66;s=2;kurwa";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::KittyTextSizing(_)));
    match cmd {
        Command::KittyTextSizing(osc) => {
            assert_eq!(2u8, osc.scale);
            assert_eq!(b"kurwa", osc.text);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_66_multiple_parameters() {
    let mut p: Parser = Parser::new(false);

    let input = "66;s=2:w=7:n=13:d=15:v=1:h=2;long";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::KittyTextSizing(_)));
    match cmd {
        Command::KittyTextSizing(osc) => {
            assert_eq!(2u8, osc.scale);
            assert_eq!(7u8, osc.width);
            assert_eq!(13u8, osc.numerator);
            assert_eq!(15u8, osc.denominator);
            assert_eq!(VAlign::Bottom, osc.valign);
            assert_eq!(HAlign::Center, osc.halign);
            assert_eq!(b"long", osc.text);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_66_scale_is_zero() {
    let mut p: Parser = Parser::new(false);

    let input = "66;s=0;nope";
    for ch in input.bytes() {
        p.next(ch);
    }
    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::KittyTextSizing(_)));
    match cmd {
        Command::KittyTextSizing(osc) => assert_eq!(1u8, osc.scale),
        _ => unreachable!(),
    }
}

#[test]
fn osc_66_invalid_parameters() {
    let mut p: Parser = Parser::new(false);

    for ch in "66;w=8:v=3:n=16;".bytes() {
        p.next(ch);
    }
    let cmd = p.end(Some(0x1b)).unwrap();

    assert!(matches!(cmd, Command::KittyTextSizing(_)));
    match cmd {
        Command::KittyTextSizing(osc) => {
            assert_eq!(0u8, osc.width);
            assert_eq!(osc.valign, VAlign::Top);
            assert_eq!(0u8, osc.numerator);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_66_utf8() {
    let mut p: Parser = Parser::new(false);

    let input = "66;;👻魑魅魍魉ゴースッティ";
    for ch in input.bytes() {
        p.next(ch);
    }

    let cmd = p.end(Some(0x1b)).unwrap();
    assert!(matches!(cmd, Command::KittyTextSizing(_)));
    match cmd {
        Command::KittyTextSizing(osc) => {
            assert_eq!("👻魑魅魍魉ゴースッティ".as_bytes(), osc.text);
        }
        _ => unreachable!(),
    }
}

#[test]
fn osc_66_unsafe_utf8() {
    let mut p: Parser = Parser::new(false);

    let input = "66;;\n";
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(0x1b)).is_none());
}

#[test]
fn osc_66_overlong_utf8() {
    let mut p: Parser = Parser::new(false);

    let input = format!("66;;{}", "bobr".repeat(1025));
    for ch in input.bytes() {
        p.next(ch);
    }

    assert!(p.end(Some(0x1b)).is_none());
}
