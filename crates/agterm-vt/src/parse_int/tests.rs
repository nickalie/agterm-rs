use super::{parse_int, parse_int_type};
use crate::fmt::{IntType, ParseIntError};

use ParseIntError::{InvalidCharacter, Overflow};

const I3: IntType = IntType::signed(3);

#[test]
fn protocol_integer_parsing() {
    assert_eq!(Ok(42), parse_int::<u8, 10>(b"042"));
    assert_eq!(Ok(255), parse_int::<u8, 16>(b"fF"));
    assert_eq!(Ok(42), parse_int::<i32, 10>(b"+42"));
    assert_eq!(Ok(-2_147_483_648), parse_int::<i32, 10>(b"-2147483648"));
    assert_eq!(Ok(2_147_483_647), parse_int::<i32, 10>(b"2147483647"));
    assert_eq!(Ok(-4), parse_int_type::<10>(I3, b"-4"));
    assert_eq!(Err(Overflow), parse_int_type::<10>(I3, b"4"));
    assert_eq!(Err(Overflow), parse_int::<u8, 10>(b"256"));
    assert_eq!(Err(Overflow), parse_int::<i32, 10>(b"2147483648"));
    assert_eq!(Err(Overflow), parse_int::<i32, 10>(b"-2147483649"));
    assert_eq!(Err(InvalidCharacter), parse_int::<u16, 16>(b"f_f"));
    assert_eq!(Err(InvalidCharacter), parse_int::<u16, 16>(b"0xff"));
}

#[test]
fn protocol_integer_parsing_refuses_zig_literal_syntax() {
    for value in [
        &b""[..],
        b"4_2",
        b"4__2",
        b"_42",
        b"42_",
        b" 42",
        b"42 ",
        b"0x2a",
        b"4.2",
        b"4e2",
        b"\xff",
    ] {
        assert_eq!(Err(InvalidCharacter), parse_int::<u8, 10>(value), "{value:?}");
        assert_eq!(Err(InvalidCharacter), parse_int::<i32, 10>(value), "{value:?}");
    }
    for value in [&b"+42"[..], b"-0", b"-42"] {
        assert_eq!(Err(InvalidCharacter), parse_int::<u8, 10>(value), "{value:?}");
    }
    for value in [&b"+"[..], b"-", b"-4_2", b"+4__2"] {
        assert_eq!(Err(InvalidCharacter), parse_int::<i32, 10>(value), "{value:?}");
    }
}

/// The accumulator is `@max(8, bits)` wide and every step is range-checked at that width, so an overflow is reported
/// at the digit that causes it, before a later invalid character.
#[test]
fn accumulator_is_width_faithful() {
    // u8: 99 * 10 = 990 overflows `Accumulate = u8` before the `x`.
    assert_eq!(Err(Overflow), parse_int::<u8, 10>(b"999x"));
    // i3 accumulates in i8: "-200" overflows at the third digit.
    assert_eq!(Err(Overflow), parse_int_type::<10>(I3, b"-200x"));
    // Within `Accumulate` but outside `T`: the final cast.
    assert_eq!(Err(Overflow), parse_int_type::<10>(I3, b"-5"));
    assert_eq!(Ok(3), parse_int_type::<10>(I3, b"+3"));
    // u9 (the OSC 4 color index) accumulates in u9 itself.
    let u9 = IntType::unsigned(9);
    assert_eq!(Ok(511), parse_int_type::<10>(u9, b"511"));
    assert_eq!(Err(Overflow), parse_int_type::<10>(u9, b"512"));
    // Widest types.
    assert_eq!(Ok(u64::MAX), parse_int::<u64, 10>(b"18446744073709551615"));
    assert_eq!(Err(Overflow), parse_int::<u64, 10>(b"18446744073709551616"));
    assert_eq!(Ok(i64::MIN), parse_int::<i64, 10>(b"-9223372036854775808"));
    assert_eq!(Err(Overflow), parse_int::<i64, 10>(b"9223372036854775808"));
    // Base 36 at the narrowest accumulator: 'z' = 35.
    assert_eq!(Ok(35), parse_int::<u8, 36>(b"Z"));
    assert_eq!(Err(Overflow), parse_int::<i8, 36>(b"zz"));
}
