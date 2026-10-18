//! The width of a code point as `simd.codepointWidth` answers it.
//!
//! Ghostty: `src/simd/codepoint_width.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-simd/src/codepoint_width.rs` at `RUSTTY_REV` (MIT).
//! Changes: the scalar lane only, uucode's width, which is the properties table's (`crate::unicode`); above U+10FFFF
//! it answers 1. The SIMD kernel answers 1 for every code point up to U+00FF, controls included, where this lane
//! answers uucode's 0; nothing in the terminal calls either (the printer reads the properties table). The result is
//! a `u8` (Ghostty's `i8` is the C kernel's type; a width is never negative).

use crate::unicode::props_table::table;

/// The largest code point uucode has data for (`uucode.config.max_code_point`).
const MAX_CODE_POINT: u32 = 0x10_FFFF;

/// The width of `cp`: uucode's, or 1 above U+10FFFF.
#[inline]
pub(crate) fn codepoint_width(cp: u32) -> u8 {
    if cp > MAX_CODE_POINT {
        return 1;
    }
    table().get(cp).width
}

#[cfg(test)]
mod tests {
    use super::{MAX_CODE_POINT, codepoint_width};
    use crate::unicode::props_table::table;

    /// Ghostty's `test "codepointWidth basic"`.
    #[test]
    fn codepoint_width_basic() {
        assert_eq!(1, codepoint_width(u32::from('a')));
        assert_eq!(1, codepoint_width(0x100)); // Ā
        assert_eq!(2, codepoint_width(0x3400)); // 㐀
        assert_eq!(2, codepoint_width(0x2E3A)); // ⸺
        assert_eq!(2, codepoint_width(0x1F1E6)); // 🇦
        assert_eq!(2, codepoint_width(0x4E00)); // 一
        assert_eq!(2, codepoint_width(0xF900)); // 豈
        assert_eq!(2, codepoint_width(0x20000)); // 𠀀
        assert_eq!(2, codepoint_width(0x30000)); // 𰀀
    }

    /// rustty's check of the scalar lane: uucode's width for every code point, including U+0000..U+00FF, where
    /// uucode gives the C0 and C1 controls 0; above the table, 1.
    #[test]
    fn the_scalar_lane_is_uucodes_width_below_0x100() {
        assert_eq!(0, codepoint_width(0x00));
        assert_eq!(0, codepoint_width(0x09)); // TAB
        assert_eq!(0, codepoint_width(0x7F));
        assert_eq!(0, codepoint_width(0x9F));
        for cp in 0..=0xFF_u32 {
            assert_eq!(table().get(cp).width, codepoint_width(cp), "cp={cp:#x}");
        }
        assert_eq!(1, codepoint_width(MAX_CODE_POINT + 1));
        assert_eq!(1, codepoint_width(u32::MAX));
    }
}
