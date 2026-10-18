use core::cmp::Ordering;

use super::parse;

/// Whether `input` parses to `want`, compared as `==` compares floats (so `-0.0` is `0.0`).
fn parses_to(input: &[u8], want: Option<f64>) -> bool {
    match (parse(input), want) {
        (Some(got), Some(want)) => got.partial_cmp(&want) == Some(Ordering::Equal),
        (got, want) => got.is_none() && want.is_none(),
    }
}

/// Zig's `test "parse"`, split by the kind of input.
#[test]
fn parse_accepts_fractions_in_range() {
    let cases: [(&[u8], f64); 15] = [
        // Plain values
        (b"0", 0.0),
        (b"1", 1.0),
        (b"0.5", 0.5),
        (b"0.25", 0.25),
        (b"1.0", 1.0),
        (b"1.000000", 1.0),
        (b"0.0", 0.0),
        // Partial forms
        (b".5", 0.5),
        (b"1.", 1.0),
        (b"00.00", 0.0),
        // Signs
        (b"+0.5", 0.5),
        (b"-0", 0.0),
        (b"-0.000", 0.0),
        // Matches std.fmt.parseFloat rounding for typical inputs.
        (b"0.3", 0.3),
        (b"0.123456789012345", 0.123_456_789_012_345),
    ];
    for (input, want) in cases {
        assert!(parses_to(input, Some(want)), "{}", String::from_utf8_lossy(input));
    }
}

/// Digits past the 15th are ignored: long fractions stay finite and equal their 15-digit truncation.
#[test]
fn parse_ignores_digits_past_the_fifteenth() {
    let long_frac = format!("0.{}", "3".repeat(400));
    assert!(parses_to(long_frac.as_bytes(), parse(b"0.333333333333333")));
}

#[test]
fn parse_refuses_negative_and_out_of_range_values() {
    let long_ones = "1".repeat(400);
    let cases: [&[u8]; 6] = [b"-0.5", b"-1", b"1.0000001", b"2", b"255", long_ones.as_bytes()];
    for input in cases {
        assert!(parses_to(input, None), "{}", String::from_utf8_lossy(input));
    }
}

#[test]
fn parse_refuses_invalid_syntax() {
    let cases: [&[u8]; 15] = [
        b"", b".", b"+", b"-", b"+.", b"abc", b"0.5x", b"0x0.8", b"0..5", b"0.5.5", b" 0.5", b"0.5 ", b"1e-1", b"nan",
        b"inf",
    ];
    for input in cases {
        assert!(parses_to(input, None), "{}", String::from_utf8_lossy(input));
    }
}
