//! The terminal's hot scans and decoders, in the scalar forms: base64, code point width, the byte search and the
//! UTF-8 decode up to the next escape.
//!
//! Ghostty: `src/simd/main.zig` at `LIBGHOSTTY_VT_REV`; its SIMD kernels (C++ over highway and simdutf) are what
//! libghostty-vt builds by default, its pure-Zig lanes what it builds with `-Dsimd=false`.
//! rustty: `crates/rustty-simd/src/lib.rs` at `RUSTTY_REV` (MIT).
//! Changes: no SIMD library and no C++ (the zig-free-core plan: the scalar paths stay, plus `memchr` where a
//! measurement shows a hot scan), so each function is its scalar lane, behaving as the shipped SIMD lane does where a
//! terminal path would see the difference: [`base64`]'s non-strict decode follows simdutf's forgiving base64, as
//! libghostty-vt's does (the UTF-8 decode and the byte search answer alike on both lanes already). The
//! `options.simd` switch is gone with the kernels. [`base64_scalar`] also holds Zig's `std.base64` standard decoders,
//! which OSC 7501 decodes with: the scalar one is Ghostty's copy of them (rustty kept them with its APC glyph protocol,
//! which is cut). `lanes` and the native vector width stay, for the hand-vectorized
//! loops (the stream, the printer, page resize) that LLVM lowers to vector instructions.

pub(crate) mod base64;
pub(crate) mod base64_scalar;
pub(crate) mod codepoint_width;
pub(crate) mod index_of;
pub(crate) mod vt;

/// The width in bits of the widest vector register the target enables, or `None` where it has no usable vectors (for
/// example wasm32 without simd128): what Zig's `std.simd.suggestVectorLength` derives its answer from, spelled out
/// from the same target facts, since stable Rust has no portable query.
pub(crate) const fn native_vector_bits() -> Option<usize> {
    if cfg!(target_arch = "aarch64") {
        // NEON is part of the base ISA: 128-bit Q registers.
        Some(128)
    } else if cfg!(all(
        any(target_arch = "x86_64", target_arch = "x86"),
        target_feature = "avx2"
    )) {
        Some(256)
    } else if cfg!(all(
        any(target_arch = "x86_64", target_arch = "x86"),
        target_feature = "sse2"
    )) || cfg!(all(target_arch = "wasm32", target_feature = "simd128"))
    {
        Some(128)
    } else {
        None
    }
}

/// The number of vector lanes for a manually vectorized hot loop over elements `elem_size` bytes wide, or `None` when
/// the target has no usable vectors and a plain scalar loop is used instead.
///
/// Twice the native vector width, so each iteration works two vector registers, for better instruction-level
/// parallelism. Ghostty takes the element type (`lanes(comptime T: type)`); a call here is `lanes(size_of::<T>())`.
pub(crate) const fn lanes(elem_size: usize) -> Option<usize> {
    let Some(bits) = native_vector_bits() else {
        return None;
    };
    let native = bits / (elem_size * 8);
    if native == 0 { None } else { Some(native * 2) }
}

#[cfg(test)]
mod tests {
    use super::{lanes, native_vector_bits};

    /// `lanes` is twice the native vector length on every target with vectors, a power of two, and `None` only where
    /// the target has none; the targets agterm ships (x86-64, aarch64) have them.
    #[test]
    fn lanes_matches_the_native_vector_width() {
        let bits = native_vector_bits();
        if cfg!(any(target_arch = "aarch64", target_arch = "x86_64")) {
            assert!(bits.is_some(), "expected vectors on this target");
        }
        match bits {
            Some(bits) => {
                for elem in [1_usize, 2, 4, 8] {
                    let l = lanes(elem).unwrap();
                    assert_eq!(l, 2 * (bits / (elem * 8)));
                    assert!(l.is_power_of_two());
                }
                // An element wider than the register has no lanes.
                assert_eq!(lanes(bits / 8 * 2), None);
            }
            None => assert_eq!(lanes(4), None),
        }
    }
}
