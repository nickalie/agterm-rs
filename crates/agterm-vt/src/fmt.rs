//! Zig's `std.fmt.parseInt` and `parseUnsigned`: the integer parser Ghostty's Kitty graphics commands and palette
//! entries read numbers with, at any integer width the callers use.
//!
//! Zig: `lib/std/fmt.zig` (`parseInt`, `parseUnsigned`, `parseIntWithSign`, `charToDigit`) of Zig 0.16.0, the version
//! `LIBGHOSTTY_VT_REV` builds with.
//! rustty: `crates/rustty-zigstd/src/fmt.rs` at `RUSTTY_REV` (MIT).
//! Changes: the integer half only (the float parser has no user here). Zig's result type is a comptime integer of any
//! width (`u4`, `u9`, `i43`, ...); [`IntType`] carries the width and signedness at run time and the `_type` functions
//! return the value widened to `i128`, while [`parse_int`] and [`parse_unsigned`] serve the native types. Each step is
//! range-checked at Zig's `Accumulate` width (`@max(8, bits)`) right away, so an overflow is reported at the digit
//! that causes it, before a later invalid character, as Zig does.
//!
//! This is Zig's literal grammar, not a protocol's: `_` digit separators are skipped, a base of 0 reads a `0b`, `0o`
//! or `0x` prefix, and an unsigned type takes a leading sign (`-0` is 0). Protocol fields use
//! [`crate::parse_int`] instead, which refuses all of that.

use core::fmt;

/// `std.fmt.ParseIntError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseIntError {
    /// The result cannot fit in the type specified.
    Overflow,
    /// The input was empty or contained an invalid character.
    InvalidCharacter,
}

impl fmt::Display for ParseIntError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Zig formats an error value as `error.Name`.
        match self {
            Self::Overflow => f.write_str("error.Overflow"),
            Self::InvalidCharacter => f.write_str("error.InvalidCharacter"),
        }
    }
}

/// A Zig integer type: its bit width (1 to 64) and signedness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IntType {
    /// The width in bits.
    pub(crate) bits: u32,
    /// Whether the type is signed.
    pub(crate) signed: bool,
}

impl IntType {
    /// `uN`.
    pub(crate) const fn unsigned(bits: u32) -> Self {
        assert!(bits >= 1 && bits <= 64, "a Zig integer type here is 1 to 64 bits wide");
        Self { bits, signed: false }
    }

    /// `iN`.
    pub(crate) const fn signed(bits: u32) -> Self {
        assert!(bits >= 1 && bits <= 64, "a Zig integer type here is 1 to 64 bits wide");
        Self { bits, signed: true }
    }

    /// `math.minInt(Self)`.
    pub(crate) const fn min(self) -> i128 {
        if self.signed { -(1_i128 << (self.bits - 1)) } else { 0 }
    }

    /// `math.maxInt(Self)`.
    pub(crate) const fn max(self) -> i128 {
        if self.signed {
            (1_i128 << (self.bits - 1)) - 1
        } else {
            (1_i128 << self.bits) - 1
        }
    }

    /// `math.cast(Self, v)`: `Some(v)` when `v` fits in this type.
    const fn cast(self, v: i128) -> Option<i128> {
        if self.min() <= v && v <= self.max() {
            Some(v)
        } else {
            None
        }
    }

    /// Zig's `Accumulate` for this result type: the same signedness, at least 8 bits wide.
    const fn accumulate(self) -> Self {
        Self {
            bits: if self.bits > 8 { self.bits } else { 8 },
            signed: self.signed,
        }
    }
}

/// A native integer type the parsers produce, with its Zig integer type.
pub(crate) trait ZigInt: Copy + Sized {
    /// The Zig integer type of the same width and signedness.
    const TYPE: IntType;

    /// The parsed value, which the parser has already range-checked against [`Self::TYPE`].
    fn from_checked(v: i128) -> Self;
}

macro_rules! zig_int {
    ($($t:ty => $it:expr),* $(,)?) => {$(
        impl ZigInt for $t {
            const TYPE: IntType = $it;

            fn from_checked(v: i128) -> Self {
                // The value was range-checked against `TYPE`, which is this type's range, so this never saturates.
                <$t>::try_from(v).unwrap_or(if v < 0 { <$t>::MIN } else { <$t>::MAX })
            }
        }
    )*};
}

zig_int! {
    u8 => IntType::unsigned(8),
    u16 => IntType::unsigned(16),
    u32 => IntType::unsigned(32),
    u64 => IntType::unsigned(64),
    usize => IntType::unsigned(usize::BITS),
    i8 => IntType::signed(8),
    i16 => IntType::signed(16),
    i32 => IntType::signed(32),
    i64 => IntType::signed(64),
    isize => IntType::signed(isize::BITS),
}

/// `std.fmt.charToDigit`: the value of the digit `c` in `base`.
pub(crate) const fn char_to_digit(c: u8, base: u8) -> Result<u8, ParseIntError> {
    let value = match c {
        b'0'..=b'9' => c - b'0',
        b'A'..=b'Z' => c - b'A' + 10,
        b'a'..=b'z' => c - b'a' + 10,
        _ => return Err(ParseIntError::InvalidCharacter),
    };
    if value >= base {
        return Err(ParseIntError::InvalidCharacter);
    }
    Ok(value)
}

/// `std.fmt.parseInt(T, buf, base)` for a native integer type. A base of 0 is read from a `0b`, `0o` or `0x` prefix
/// (decimal otherwise); `_` separators between digits are skipped.
pub(crate) fn parse_int<T: ZigInt>(buf: &[u8], base: u8) -> Result<T, ParseIntError> {
    parse_int_type(T::TYPE, buf, base).map(T::from_checked)
}

/// `std.fmt.parseUnsigned(T, buf, base)` for a native integer type: [`parse_int`] without a sign.
pub(crate) fn parse_unsigned<T: ZigInt>(buf: &[u8], base: u8) -> Result<T, ParseIntError> {
    parse_unsigned_type(T::TYPE, buf, base).map(T::from_checked)
}

/// `std.fmt.parseInt(T, buf, base)` for the Zig integer type `t`, the value widened to `i128`.
pub(crate) fn parse_int_type(t: IntType, buf: &[u8], base: u8) -> Result<i128, ParseIntError> {
    match buf.split_first() {
        None => Err(ParseIntError::InvalidCharacter),
        Some((b'+', rest)) => parse_int_with_sign(t, rest, base, false),
        Some((b'-', rest)) => parse_int_with_sign(t, rest, base, true),
        Some(_) => parse_int_with_sign(t, buf, base, false),
    }
}

/// `std.fmt.parseUnsigned(T, buf, base)` for the Zig integer type `t`, the value widened to `i128`.
pub(crate) fn parse_unsigned_type(t: IntType, buf: &[u8], base: u8) -> Result<i128, ParseIntError> {
    parse_int_with_sign(t, buf, base, false)
}

/// The digits after the base prefix, and the base, for Zig's base 0 (`0b`, `0o`, `0x`, else decimal).
fn detect_base(buf: &[u8], base: u8) -> (&[u8], u8) {
    if base != 0 {
        return (buf, base);
    }
    if buf.len() > 2 && buf[0] == b'0' {
        match buf[1].to_ascii_lowercase() {
            b'b' => return (&buf[2..], 2),
            b'o' => return (&buf[2..], 8),
            b'x' => return (&buf[2..], 16),
            _ => {}
        }
    }
    (buf, 10)
}

/// `parseIntWithSign`: the digits of `buf` in `base`, added (or for `neg` subtracted) at the `Accumulate` width of
/// `result`, then cast to `result`.
fn parse_int_with_sign(result: IntType, buf: &[u8], base: u8, neg: bool) -> Result<i128, ParseIntError> {
    if buf.is_empty() {
        return Err(ParseIntError::InvalidCharacter);
    }
    let (digits, base) = detect_base(buf, base);
    let accumulate_t = result.accumulate();
    let (Some(&first), Some(&last)) = (digits.first(), digits.last()) else {
        // `0x` with nothing after it.
        return Err(ParseIntError::InvalidCharacter);
    };
    if first == b'_' || last == b'_' {
        return Err(ParseIntError::InvalidCharacter);
    }

    let mut accumulate: i128 = 0;
    for &c in digits {
        if c == b'_' {
            continue;
        }
        let digit = i128::from(char_to_digit(c, base)?);
        if accumulate != 0 {
            let b = accumulate_t.cast(i128::from(base)).ok_or(ParseIntError::Overflow)?;
            accumulate = accumulate_t.cast(accumulate * b).ok_or(ParseIntError::Overflow)?;
        } else if neg {
            // The first digit of a negative number: "-4" is an `i3` although 4 is not, so the digit is negated
            // before it is cast.
            accumulate = accumulate_t.cast(-digit).ok_or(ParseIntError::Overflow)?;
            continue;
        }
        let d = accumulate_t.cast(digit).ok_or(ParseIntError::Overflow)?;
        let next = if neg { accumulate - d } else { accumulate + d };
        accumulate = accumulate_t.cast(next).ok_or(ParseIntError::Overflow)?;
    }
    result.cast(accumulate).ok_or(ParseIntError::Overflow)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod zig_corpus;
