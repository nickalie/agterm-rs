//! The stream's bulk UTF-8 decode: everything up to the next ESC, decoded in one call.
//!
//! Ill-formed input is replaced by U+FFFD per the W3C/Unicode "U+FFFD Substitution of Maximal Subparts" algorithm
//! (Unicode §3.9, Tables 3-8 to 3-11), and a sequence cut off by the end of the input is left for the next call.
//!
//! Ghostty: `src/simd/vt.zig` at `LIBGHOSTTY_VT_REV` (the SIMD kernel in `vt.cpp` follows the same algorithm, and
//! Ghostty's differential test holds the two lanes equal).
//! rustty: `crates/rustty-simd/src/vt.rs` at `RUSTTY_REV` (MIT).
//! Changes: the scalar lane only. Its ASCII run copies native-vector-width chunks, which LLVM vectorizes as Zig's
//! `@Vector` code is. The SIMD-against-scalar differential test went with the kernel; its corpus generator holds the
//! scalar lane to the standard library's lossy decode (`String::from_utf8_lossy`), which also substitutes maximal
//! subparts.

use crate::simd::index_of::index_of;
use crate::simd::native_vector_bits;

/// How far one call got.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) struct DecodeResult {
    /// The input bytes consumed.
    pub(crate) consumed: usize,
    /// The code points written to the output.
    pub(crate) decoded: usize,
}

/// The ASCII run's chunk: the native vector width in `u8` lanes (`std.simd.suggestVectorLength(u8)`), or `None` where
/// the target has no usable vectors.
const ASCII_CHUNK: Option<usize> = match native_vector_bits() {
    Some(bits) => Some(bits / 8),
    None => None,
};

/// Decode `input` up to its first ESC (0x1B) into `output`, which holds at least `input.len()` code points.
///
/// Stops early, without consuming them, at the bytes of a sequence that is valid so far but cut off by the end of
/// the input: more input may complete it. A sequence cut off by the ESC can never complete and is replaced.
///
/// # Panics
///
/// If `output` is shorter than `input`.
pub(crate) fn utf8_decode_until_control_seq(input: &[u8], output: &mut [u32]) -> DecodeResult {
    assert!(
        output.len() >= input.len(),
        "the output holds a code point per input byte"
    );

    let decode = &input[..index_of(input, 0x1B).unwrap_or(input.len())];
    let cut_by_input_end = decode.len() == input.len();
    let mut offset = 0;
    let mut count = 0;
    while offset < decode.len() {
        let step = if decode[offset] < 0x80 {
            ascii_run(&decode[offset..], &mut output[count..])
        } else {
            match multibyte(&decode[offset..], cut_by_input_end) {
                Some((cp, len)) => {
                    output[count] = cp;
                    Step {
                        consumed: len,
                        decoded: 1,
                    }
                }
                None => break,
            }
        };
        offset += step.consumed;
        count += step.decoded;
    }
    DecodeResult {
        consumed: offset,
        decoded: count,
    }
}

/// What one step of the decode loop did.
struct Step {
    consumed: usize,
    decoded: usize,
}

/// Copy the run of ASCII bytes that starts `bytes` to `output`, a code point each: vector-width chunks while a whole
/// chunk is ASCII, then byte by byte.
fn ascii_run(bytes: &[u8], output: &mut [u32]) -> Step {
    let mut n = 0;
    if let Some(chunk) = ASCII_CHUNK {
        while let Some(v) = bytes.get(n..n + chunk) {
            if v.iter().any(|&b| b >= 0x80) {
                break;
            }
            for (out, &b) in output[n..n + chunk].iter_mut().zip(v) {
                *out = u32::from(b);
            }
            n += chunk;
        }
    }
    while let Some(&b) = bytes.get(n) {
        if b >= 0x80 {
            break;
        }
        output[n] = u32::from(b);
        n += 1;
    }
    Step {
        consumed: n,
        decoded: n,
    }
}

/// Decode the sequence that starts `bytes` at a non-ASCII lead: its code point and length, U+FFFD and the length of
/// its maximal subpart when it is ill-formed, or `None` when it is valid so far but cut off by the end of the input
/// (`cut_by_input_end`), so the caller stops without consuming it.
fn multibyte(bytes: &[u8], cut_by_input_end: bool) -> Option<(u32, usize)> {
    let b0 = bytes[0];
    // A continuation byte (80-BF) or an invalid lead (C0-C1, F5-FF) is a maximal subpart of its own.
    if !(0xC2..=0xF4).contains(&b0) {
        return Some((0xFFFD, 1));
    }

    // Only the first continuation byte has a lead-dependent range (Table 3-7); later ones are always 80-BF. The
    // ranges exclude overlong, surrogate and out-of-range encodings, so a sequence that fits them decodes by bit
    // assembly with no further checks.
    let seq_len: usize = match b0 {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    };
    let first_range = match b0 {
        0xE0 => 0xA0..=0xBF,
        0xED => 0x80..=0x9F,
        0xF0 => 0x90..=0xBF,
        0xF4 => 0x80..=0x8F,
        _ => 0x80..=0xBF,
    };

    // The lead contributes its low 7 - seq_len bits; count how many continuation bytes form a valid prefix.
    let mut cp = u32::from(b0) & (0x7F >> seq_len);
    let mut valid = 1;
    while valid < seq_len {
        let Some(&cb) = bytes.get(valid) else {
            // Cut off by the end of the decode region: at the true end of the input more input may complete it;
            // before an ESC it never can, and the valid prefix is a maximal subpart.
            if cut_by_input_end {
                return None;
            }
            break;
        };
        let in_range = if valid == 1 {
            first_range.contains(&cb)
        } else {
            (0x80..=0xBF).contains(&cb)
        };
        if !in_range {
            break;
        }
        cp = (cp << 6) | u32::from(cb & 0x3F);
        valid += 1;
    }

    if valid == seq_len {
        Some((cp, seq_len))
    } else {
        Some((0xFFFD, valid))
    }
}

#[cfg(test)]
mod tests;
