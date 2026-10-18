//! Parsing of decimal fractions in the unit interval [0, 1].
//!
//! Escape sequences that carry fractional parameters (`XTerm` `rgbi:`
//! color specifications, glyph protocol padding) only ever need plain
//! decimal values such as "0.5". Using `std.fmt.parseFloat` for these
//! pulls the full correctly-rounded float parser and its lookup tables
//! into the binary (~26KB). This implements only the restricted subset
//! we need.
//!
//! Ghostty: `src/terminal/fraction.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/fraction.rs` at `RUSTTY_REV` (MIT).
//! Changes: none beyond Rust's spelling.

/// Parse a decimal fraction in the range [0, 1] inclusive.
///
/// Accepts an optional leading `+` or `-` sign followed by decimal
/// digits with an optional decimal point ("0.5", ".5", "1.", "1"); at
/// least one digit is required. Exponent and hexadecimal float syntax
/// are not supported. Returns null for invalid syntax and for any value
/// outside [0, 1] (so negative values other than -0 are rejected).
///
/// Results are correctly rounded for inputs of up to 15 fractional
/// digits; further digits are validated but ignored, which can move the
/// result by less than 1e-15.
pub(crate) fn parse(value: &[u8]) -> Option<f64> {
    let mut v = value;
    let mut negative = false;
    if !v.is_empty() {
        match v[0] {
            b'+' => v = &v[1..],
            b'-' => {
                negative = true;
                v = &v[1..];
            }
            _ => {}
        }
    }

    let mut i: usize = 0;
    let int_part: f64 = {
        let mut int_part: f64 = 0.0;
        while i < v.len() && v[i] != b'.' {
            let d = v[i];
            if !d.is_ascii_digit() {
                return None;
            }
            int_part = int_part * 10.0 + f64::from(d - b'0');
            i += 1;
        }
        int_part
    };

    let mut digits: usize = i;
    let mut frac: u64 = 0;
    let mut scale: u64 = 1;
    if i < v.len() {
        debug_assert_eq!(v[i], b'.');
        i += 1;
        while i < v.len() {
            let d = v[i];
            if !d.is_ascii_digit() {
                return None;
            }
            // Stop accumulating after 15 digits so that frac and scale
            // both stay exactly representable in f64 (and cannot
            // overflow), making the division below round only once.
            if scale < 1_000_000_000_000_000 {
                frac = frac * 10 + u64::from(d - b'0');
                scale *= 10;
            }
            digits += 1;
            i += 1;
        }
    }
    if digits == 0 {
        return None;
    }

    // frac and scale are both exact in f64 (below 2^53 and a power of
    // ten no greater than 1e15), so this division rounds only once.
    // Both are below 2^53 (asserted by the 15-digit cap above), so the conversions are exact.
    #[expect(
        clippy::cast_precision_loss,
        reason = "frac and scale stay below 1e15 < 2^53, exact in f64"
    )]
    let magnitude = int_part + (frac as f64) / (scale as f64);
    let result = if negative { -magnitude } else { magnitude };

    // Range check. Written so that -0 passes and any out-of-range or
    // non-finite value fails (NaN fails `contains`).
    if !(0.0..=1.0).contains(&result) {
        return None;
    }
    Some(result)
}

#[cfg(test)]
mod tests;
