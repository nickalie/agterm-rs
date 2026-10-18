use super::{Kind, Osc, Request, Special};
use crate::mem::alloc::OutOfMemory;
use crate::mem::testing::with_failing;
use crate::terminal::osc::Terminator;

#[test]
fn osc_kitty_color_protocol_kind_string() {
    assert_eq!("foreground", Kind::Special(Special::Foreground).to_string());
    assert_eq!("42", Kind::Palette(42).to_string());
    assert_eq!(
        "second_transparent_background",
        Kind::Special(Special::SecondTransparentBackground).to_string()
    );

    assert!(Kind::Palette(42).has_terminal_query_color());
    assert!(Kind::Special(Special::Foreground).has_terminal_query_color());
    assert!(!Kind::Special(Special::SelectionBackground).has_terminal_query_color());
}

#[test]
fn osc_kitty_color_indexes_reject_digit_separators() {
    assert_eq!(None, Kind::parse(b"1_0"));
    assert_eq!(Some(Kind::Palette(10)), Kind::parse(b"10"));
    assert_eq!(
        Some(Kind::Special(Special::SelectionForeground)),
        Kind::parse(b"selection_foreground")
    );
}

/// `lib.parseInt` refuses separators and signs, and anything else a base-10 `u8` parse refuses; special keys win
/// over the numeric parse.
#[test]
fn osc_kitty_color_protocol_kind_parse_unsigned_semantics() {
    assert_eq!(Kind::parse(b"42"), Some(Kind::Palette(42)));
    assert_eq!(Kind::parse(b"255"), Some(Kind::Palette(255)));
    for bad in [
        &b"1_2"[..],
        b"2_5_5",
        b"+1",
        b"-1",
        b"-0",
        b"_1",
        b"1_",
        b"_",
        b"",
        b" 1",
        b"1 ",
        b"256",
        b"0x1",
    ] {
        assert_eq!(Kind::parse(bad), None, "{bad:?}");
    }
    assert_eq!(Kind::parse(b"foreground"), Some(Kind::Special(Special::Foreground)));
    assert_eq!(263, Kind::MAX);
}

#[test]
fn appending_requests_can_fail() {
    let mut osc = Osc::new(Terminator::Bel);
    let (failed, stats) = with_failing(0, || osc.append(Request::Query(Kind::Palette(1))));
    assert_eq!(Err(OutOfMemory), failed);
    assert!(stats.has_induced_failure);
    assert!(osc.list.is_empty());
    osc.append(Request::Reset(Kind::Palette(2))).unwrap();
    assert_eq!([Request::Reset(Kind::Palette(2))], osc.list[..]);
    assert_eq!(Terminator::Bel, osc.terminator);
}
