use super::zig_corpus::INTS;
use super::{IntType, ParseIntError, ZigInt, parse_int, parse_int_type};

use ParseIntError::{InvalidCharacter, Overflow};

/// `(input, base, parseInt(i32, ..))` cases of Zig's `test_parse_int`, less its base 0 ones.
const I32_CASES: &[(&[u8], u8, Result<i32, ParseIntError>)] = &[
    (b"-10", 10, Ok(-10)),
    (b"+10", 10, Ok(10)),
    (b"", 10, Err(InvalidCharacter)),
    (b"+", 10, Err(InvalidCharacter)),
    (b"-", 10, Err(InvalidCharacter)),
];

/// `(input, base, parseInt(u32, ..))` cases of Zig's `test_parse_int`, less its base 0 ones.
const U32_CASES: &[(&[u8], u8, Result<u32, ParseIntError>)] = &[
    (b"+10", 10, Ok(10)),
    (b"-10", 10, Err(Overflow)),
    (b" 10", 10, Err(InvalidCharacter)),
    (b"10 ", 10, Err(InvalidCharacter)),
    (b"_10_", 10, Err(InvalidCharacter)),
    (b"0x_10_", 10, Err(InvalidCharacter)),
    (b"0x10_", 10, Err(InvalidCharacter)),
    (b"0x_10", 10, Err(InvalidCharacter)),
    // empty string or bare +- is invalid
    (b"", 10, Err(InvalidCharacter)),
    (b"+", 10, Err(InvalidCharacter)),
    (b"-", 10, Err(InvalidCharacter)),
];

#[test]
fn test_parse_int() {
    for &(input, base, want) in I32_CASES {
        assert_eq!(parse_int::<i32>(input, base), want, "{input:?} base {base}");
    }
    for &(input, base, want) in U32_CASES {
        assert_eq!(parse_int::<u32>(input, base), want, "{input:?} base {base}");
    }
}

#[test]
fn test_parse_int_narrow_types() {
    assert_eq!(parse_int::<u8>(b"255", 10), Ok(255));
    assert_eq!(parse_int::<u8>(b"256", 10), Err(Overflow));

    // +0 and -0 should work for unsigned
    assert_eq!(parse_int::<u8>(b"-0", 10), Ok(0));
    assert_eq!(parse_int::<u8>(b"+0", 10), Ok(0));

    // ensure minInt is parsed correctly
    let i1 = IntType::signed(1);
    assert_eq!(parse_int_type(i1, b"-1", 10), Ok(i1.min()));
    assert_eq!(parse_int::<i8>(b"-128", 10), Ok(i8::MIN));
    let i43 = IntType::signed(43);
    assert_eq!(parse_int_type(i43, b"-4398046511104", 10), Ok(i43.min()));

    // edge cases which previously errored due to base overflowing T
    assert_eq!(parse_int_type(IntType::signed(2), b"-10", 2), Ok(-2));
    assert_eq!(parse_int_type(IntType::signed(4), b"-10", 8), Ok(-8));
    assert_eq!(parse_int_type(IntType::signed(5), b"-10", 16), Ok(-16));
}

/// `(type, input, base, parseInt(type, ..))`.
type NarrowCase = (IntType, &'static [u8], u8, Result<i128, ParseIntError>);

/// The cases for Zig's narrow types in `test_parse_unsigned`, which `parseInt` reads the same: these numbers
/// should fit even though the base itself doesn't fit in the destination type.
const NARROW_UNSIGNED_CASES: &[NarrowCase] = &[
    (IntType::unsigned(7), b"1", 10, Ok(1)),
    (IntType::unsigned(7), b"1000", 2, Ok(8)),
    (IntType::unsigned(1), b"0", 10, Ok(0)),
    (IntType::unsigned(1), b"1", 10, Ok(1)),
    (IntType::unsigned(1), b"2", 10, Err(Overflow)),
    (IntType::unsigned(1), b"001", 16, Ok(1)),
    (IntType::unsigned(2), b"3", 16, Ok(3)),
    (IntType::unsigned(2), b"4", 16, Err(Overflow)),
];

#[test]
fn parse_int_narrow_unsigned_types() {
    for &(t, input, base, want) in NARROW_UNSIGNED_CASES {
        assert_eq!(parse_int_type(t, input, base), want, "{t:?} {input:?} base {base}");
    }
}

#[test]
fn test_parse_u64_digit_too_big() {
    assert_eq!(parse_int::<u64>(b"123a", 10), Err(InvalidCharacter));
}

/// The error name, or the value, as the corpus generator prints a result.
fn int_result<T: ZigInt + core::fmt::Display>(s: &[u8]) -> String {
    match parse_int::<T>(s, 10) {
        Ok(v) => v.to_string(),
        Err(Overflow) => "Overflow".to_owned(),
        Err(InvalidCharacter) => "InvalidCharacter".to_owned(),
    }
}

/// Zig 0.16.0's own `parseInt(T, s, 10)` results for every corpus input and eight integer types.
#[test]
#[cfg(target_pointer_width = "64")]
fn parse_int_matches_zig_over_the_corpus() {
    assert!(INTS.len() > 40);
    for (input, want) in INTS {
        let got = [
            int_result::<i16>(input),
            int_result::<u16>(input),
            int_result::<i32>(input),
            int_result::<u32>(input),
            int_result::<isize>(input),
            int_result::<usize>(input),
            int_result::<u8>(input),
            int_result::<i64>(input),
        ];
        assert_eq!(got, *want, "input {:?}", String::from_utf8_lossy(input));
    }
}

#[test]
fn parse_int_sign_and_separator_edges() {
    // a second sign is refused
    assert!(parse_int::<i32>(b"+-5", 10).is_err());
    // -0 reads into an unsigned type
    assert_eq!(parse_int::<u32>(b"-0", 10), Ok(0));
    // separators may repeat between digits
    assert_eq!(parse_int::<u32>(b"1__0", 10), Ok(10));
    assert_eq!(parse_int::<u64>(b"10_000_000", 10), Ok(10_000_000));
}

/// Every step is range-checked at the accumulator's width, so an overflow is reported at the digit that causes it,
/// before a later invalid character (measured with Zig 0.16.0).
#[test]
fn parse_int_overflows_before_a_later_bad_character() {
    assert_eq!(parse_int::<i64>(b"99999999999999999999x", 10), Err(Overflow));
    assert_eq!(parse_int::<u8>(b"99999999999999999999x", 10), Err(Overflow));
    assert_eq!(parse_int::<u8>(b"300x", 10), Err(Overflow));
    assert_eq!(parse_int::<i8>(b"300x", 10), Err(Overflow));
    assert_eq!(parse_int::<i8>(b"-300x", 10), Err(Overflow));
    assert_eq!(parse_int::<i8>(b"-129x", 10), Err(Overflow));
    assert_eq!(parse_int::<u32>(b"-1x", 10), Err(Overflow));
    assert_eq!(parse_int::<u8>(b"100x", 16), Err(Overflow));
}

#[test]
fn digit_separators_and_accumulate_width() {
    assert_eq!(parse_int::<u32>(b"1_0", 10), Ok(10));
    assert_eq!(parse_int::<i32>(b"-1_0", 10), Ok(-10));
    assert_eq!(parse_int::<u32>(b"1__0", 10), Ok(10));
    assert_eq!(parse_int::<u32>(b"10_", 10), Err(InvalidCharacter));
    assert_eq!(parse_int::<u32>(b"_10", 10), Err(InvalidCharacter));
    assert_eq!(parse_int::<u32>(b"_", 10), Err(InvalidCharacter));
    assert_eq!(parse_int::<i32>(b"2147483647", 10), Ok(i32::MAX));
    assert_eq!(parse_int::<i32>(b"-2147483648", 10), Ok(i32::MIN));
    assert_eq!(parse_int::<i32>(b"2147483648", 10), Err(Overflow));
    assert_eq!(parse_int::<u32>(b"-1a", 10), Err(Overflow));
    assert_eq!(parse_int::<usize>(b"-0", 10), Ok(0));
}

#[test]
fn digit_separators_at_odd_widths() {
    let u4 = IntType::unsigned(4);
    assert_eq!(parse_int_type(u4, b"15", 10), Ok(15));
    assert_eq!(parse_int_type(u4, b"16", 10), Err(Overflow));
    assert_eq!(parse_int_type(u4, b"99x", 10), Err(InvalidCharacter));
    assert_eq!(parse_int_type(u4, b"1_2", 10), Ok(12));
    let u21 = IntType::unsigned(21);
    assert_eq!(parse_int_type(u21, b"1F_FFFF", 16), Ok(0x1F_FFFF));
    assert_eq!(parse_int_type(u21, b"20_0000", 16), Err(Overflow));
}

#[test]
fn errors_print_as_zig_error_values() {
    assert_eq!(Overflow.to_string(), "error.Overflow");
    assert_eq!(InvalidCharacter.to_string(), "error.InvalidCharacter");
}

#[test]
fn int_type_ranges() {
    assert_eq!((IntType::unsigned(9).min(), IntType::unsigned(9).max()), (0, 511));
    assert_eq!((IntType::signed(3).min(), IntType::signed(3).max()), (-4, 3));
    assert_eq!(IntType::unsigned(64).max(), i128::from(u64::MAX));
    assert_eq!(IntType::signed(64).min(), i128::from(i64::MIN));
}
