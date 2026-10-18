//! The per-code-point "is a symbol" lookup table.
//!
//! Ghostty: `src/unicode/symbols_table.zig` at `LIBGHOSTTY_VT_REV`; the spot checks follow `computeIsSymbol` in
//! `src/build/uucode_config.zig`.
//! rustty: `crates/rustty-unicode/src/symbols_table.rs` and the tests of `symbols_uucode.rs` at `RUSTTY_REV` (MIT).
//! Changes: the table is the checked-in generated stages instead of a build-system module. `symbols_uucode.zig`, the
//! generator that writes them from uucode, is the `gen-unicode` binary's (`src/bin/gen-unicode`); its out-of-range
//! rule (no code point above U+10FFFF is a symbol) is what the table holds there, and its spot checks run on the
//! table here.

use crate::unicode::generated::symbols_data;
use crate::unicode::lut::TablesRef;

/// The lookup tables for whether a code point is a symbol.
#[inline]
pub(crate) const fn table()
-> TablesRef<bool, { symbols_data::STAGE1.len() }, { symbols_data::STAGE2.len() }, { symbols_data::STAGE3.len() }> {
    TablesRef {
        stage1: &symbols_data::STAGE1,
        stage2: &symbols_data::STAGE2,
        stage3: &symbols_data::STAGE3,
    }
}

#[cfg(test)]
mod tests {
    use super::table;

    /// rustty's spot checks against the standard, not against the table: a code point is a symbol iff its general
    /// category is `Co` (private use) or it is in one of the blocks Arrows, Dingbats, Emoticons, Miscellaneous
    /// Symbols, Enclosed Alphanumerics, Enclosed Alphanumeric Supplement, Miscellaneous Symbols and Pictographs, and
    /// Transport and Map Symbols (`computeIsSymbol`). Ghostty's full comparison with uucode is the generator's.
    #[test]
    fn unicode_symbols_match_the_standard() {
        let cases: &[(u32, bool, &str)] = &[
            (0x2190, true, "U+2190 LEFTWARDS ARROW (Arrows)"),
            (0x2192, true, "U+2192 RIGHTWARDS ARROW (Arrows)"),
            (0x21FF, true, "U+21FF the end of Arrows"),
            (0x2702, true, "U+2702 BLACK SCISSORS (Dingbats)"),
            (0x2764, true, "U+2764 HEAVY BLACK HEART (Dingbats)"),
            (0x2600, true, "U+2600 BLACK SUN WITH RAYS (Miscellaneous Symbols)"),
            (0x2622, true, "U+2622 RADIOACTIVE SIGN (Miscellaneous Symbols)"),
            (0x2460, true, "U+2460 CIRCLED DIGIT ONE (Enclosed Alphanumerics)"),
            (
                0x1F150,
                true,
                "U+1F150 NEGATIVE CIRCLED LATIN CAPITAL LETTER A (Enclosed Alphanumeric Supplement)",
            ),
            (0x1F600, true, "U+1F600 GRINNING FACE (Emoticons)"),
            (0x1F62D, true, "U+1F62D LOUDLY CRYING FACE (Emoticons)"),
            (0x1F300, true, "U+1F300 CYCLONE (Miscellaneous Symbols and Pictographs)"),
            (
                0x1F4A9,
                true,
                "U+1F4A9 PILE OF POO (Miscellaneous Symbols and Pictographs)",
            ),
            (0x1F680, true, "U+1F680 ROCKET (Transport and Map Symbols)"),
            (0x1F6A2, true, "U+1F6A2 SHIP (Transport and Map Symbols)"),
            (0xE000, true, "U+E000 the start of the private use area"),
            (0xF8FF, true, "U+F8FF the end of the private use area"),
            (0xF0000, true, "U+F0000 the start of Supplementary Private Use Area-A"),
            (0x0041, false, "U+0041 'A'"),
            (0x0039, false, "U+0039 '9'"),
            (0x0020, false, "U+0020 SPACE"),
            (0x4E00, false, "U+4E00 CJK ideograph one"),
            (0x3042, false, "U+3042 HIRAGANA LETTER A"),
            (0x0915, false, "U+0915 DEVANAGARI LETTER KA"),
            // A mathematical symbol, but Mathematical Operators is not one of the blocks.
            (0x2200, false, "U+2200 FOR ALL (Mathematical Operators)"),
        ];
        for &(cp, expected, what) in cases {
            assert_eq!(table().get(cp), expected, "{what}");
        }
    }

    /// No code point above U+10FFFF is a symbol; `u32::MAX` masks onto one of them.
    #[test]
    fn code_points_above_the_unicode_range_are_not_symbols() {
        for cp in [0x11_0000, 0x1F_FFFF, u32::MAX] {
            assert!(!table().get(cp), "U+{cp:X}");
        }
    }
}
