//! Unicode full case folding: uucode's `case_folding_full` field.
//!
//! Ghostty: the `case_folding_full` field of the `runtime` table in `src/build/uucode_config.zig` at
//! `LIBGHOSTTY_VT_REV`, read as `uucode.get(.case_folding_full, cp).with(&buffer, cp)` (`input/Binding.zig`'s
//! `foldedCodepoint`); uucode 0.2.0 @ `9d555245` (`src/root.zig` for the test).
//! rustty: `crates/rustty-unicode/src/case_folding.rs` at `RUSTTY_REV` (MIT).
//! Changes: the folding is a binary search over the generated sorted list instead of uucode's table; the mapping
//! comes back as a slice of the returned buffer's code points.

use crate::unicode::generated::case_folding_data::CASE_FOLDING_FULL;

/// The full case folding of `cp`: up to three code points, and how many of them there are. A code point with no
/// folding folds to itself, as uucode's `with` returns `[cp]`.
pub(crate) fn case_folding_full(cp: u32) -> ([u32; 3], usize) {
    match CASE_FOLDING_FULL.binary_search_by_key(&cp, |&(k, _)| k) {
        Ok(i) => {
            let mapping = CASE_FOLDING_FULL[i].1;
            let len = mapping.iter().take_while(|&&c| c != 0).count();
            (mapping, len)
        }
        Err(_) => ([cp, 0, 0], 1),
    }
}

#[cfg(test)]
mod tests {
    use super::{CASE_FOLDING_FULL, case_folding_full};

    /// uucode's `test "case_folding_full"`: ß (U+00DF) folds to "ss".
    #[test]
    fn case_folding_full_sharp_s() {
        let (mapping, len) = case_folding_full(0x00DF);
        assert_eq!(len, 2);
        assert_eq!(&mapping[..len], &[0x0073, 0x0073]);
    }

    /// Spot checks against `CaseFolding-18.0.0.txt`: a C entry, a three-code-point F entry, a code point whose F
    /// mapping wins over its S one, one whose F mapping wins over its T one, one with only C and T, Unicode 18's new
    /// entries, and code points with no entry.
    #[test]
    fn case_folding_full_follows_the_ucd_statuses() {
        // 00C9; C; 00E9; # LATIN CAPITAL LETTER E WITH ACUTE
        assert_eq!(case_folding_full(0x00C9), ([0x00E9, 0, 0], 1));
        // 0390; F; 03B9 0308 0301; # GREEK SMALL LETTER IOTA WITH DIALYTIKA AND TONOS
        assert_eq!(case_folding_full(0x0390), ([0x03B9, 0x0308, 0x0301], 3));
        // 1E9E; F; 0073 0073; and 1E9E; S; 00DF;
        assert_eq!(case_folding_full(0x1E9E), ([0x0073, 0x0073, 0], 2));
        // 0130; F; 0069 0307; and 0130; T; 0069;
        assert_eq!(case_folding_full(0x0130), ([0x0069, 0x0307, 0], 2));
        // 0049; C; 0069; and 0049; T; 0131;
        assert_eq!(case_folding_full(0x0049), ([0x0069, 0, 0], 1));
        // New in 18.0.0: A7DD; C; 0277; 1DF40; C; 1DF41; 1DF95; F; 0073 0073;
        assert_eq!(case_folding_full(0xA7DD), ([0x0277, 0, 0], 1));
        assert_eq!(case_folding_full(0x1DF40), ([0x1DF41, 0, 0], 1));
        assert_eq!(case_folding_full(0x1DF95), ([0x0073, 0x0073, 0], 2));
        // No entry: folds to itself.
        assert_eq!(case_folding_full(0x00E9), ([0x00E9, 0, 0], 1));
        assert_eq!(case_folding_full(0x10_FFFF), ([0x10_FFFF, 0, 0], 1));
    }

    /// The list is sorted (the lookup binary-searches it) and has its header's counts: 1501 C + 105 F = 1606.
    #[test]
    fn case_folding_table_is_sorted_and_complete() {
        assert_eq!(CASE_FOLDING_FULL.len(), 1606);
        assert!(CASE_FOLDING_FULL.windows(2).all(|w| w[0].0 < w[1].0));
        let multi = CASE_FOLDING_FULL.iter().filter(|(_, m)| m[1] != 0).count();
        assert_eq!(multi, 105);
    }
}
