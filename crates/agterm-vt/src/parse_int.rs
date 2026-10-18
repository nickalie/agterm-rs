//! Integer parsing for terminal protocols, without Zig digit separators.
//!
//! Adapted from Zig's `std.fmt.parseInt` (MIT license). Ghostty's comment: this exists because `std.fmt.parseInt`
//! implements Zig integer literal syntax rather than what terminal protocols specify: it ignores `_` digit separators
//! (so `4_2` parses as 42) and accepts a leading `+` or `-` even for unsigned types. Accepting that input makes us
//! diverge from other terminals, so don't replace this with [`crate::fmt::parse_int`].
//!
//! Ghostty: `src/lib/parse_int.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-lib/src/parse_int.rs` at `RUSTTY_REV` (MIT).
//! Changes: the comptime base is a const generic with the comptime assert at compile time; Zig's result type of any
//! width is [`IntType`] for [`parse_int_type`] (the value widened to `i128`), and a native type for [`parse_int`].

use crate::fmt::{IntType, ParseIntError, ZigInt, char_to_digit};

/// Parse ASCII digits in the given base. Signed types allow a leading sign; unsigned types accept digits only.
pub(crate) fn parse_int<T: ZigInt, const BASE: u8>(value: &[u8]) -> Result<T, ParseIntError> {
    parse_int_type::<BASE>(T::TYPE, value).map(T::from_checked)
}

/// [`parse_int`] for the Zig integer type `t` (any width from 1 to 64 bits), the value widened to `i128`.
pub(crate) fn parse_int_type<const BASE: u8>(t: IntType, value: &[u8]) -> Result<i128, ParseIntError> {
    const { assert!(BASE >= 2 && BASE <= 36, "a base is 2 to 36") };
    match value.split_first() {
        None => Err(ParseIntError::InvalidCharacter),
        Some((b'+', rest)) if t.signed => parse_with_sign::<BASE>(t, rest, false),
        Some((b'-', rest)) if t.signed => parse_with_sign::<BASE>(t, rest, true),
        Some(_) => parse_with_sign::<BASE>(t, value, false),
    }
}

/// The digits of `value`, multiplied up and added (or for `neg` subtracted) at Zig's `Accumulate` width, then cast
/// to `t`. Every step is checked at that width, so an overflow is reported at the digit that causes it.
fn parse_with_sign<const BASE: u8>(t: IntType, value: &[u8], neg: bool) -> Result<i128, ParseIntError> {
    if value.is_empty() {
        return Err(ParseIntError::InvalidCharacter);
    }
    let accumulate_t = if t.signed {
        IntType::signed(t.bits.max(8))
    } else {
        IntType::unsigned(t.bits.max(8))
    };
    let fit = |v: i128| {
        if accumulate_t.min() <= v && v <= accumulate_t.max() {
            Ok(v)
        } else {
            Err(ParseIntError::Overflow)
        }
    };
    let mut accumulate: i128 = 0;
    for &c in value {
        let digit = i128::from(char_to_digit(c, BASE)?);
        accumulate = fit(accumulate * i128::from(BASE))?;
        accumulate = fit(if neg { accumulate - digit } else { accumulate + digit })?;
    }
    if t.min() <= accumulate && accumulate <= t.max() {
        Ok(accumulate)
    } else {
        Err(ParseIntError::Overflow)
    }
}

#[cfg(test)]
mod tests;
