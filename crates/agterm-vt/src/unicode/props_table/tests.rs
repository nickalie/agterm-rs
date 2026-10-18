//! Spot checks of the properties table against the Unicode standard, not against the table itself.
//!
//! Ghostty's `test "unicode props: tables match uucode"` compares the generated table with uucode over the whole
//! code point range; that full comparison is the generator's (the zig-free-core plan's Task 6, which reproduces the
//! tables from the UCD). These cases are rustty's independent complement: expected values from the standard and
//! Ghostty's rules, not read from the table.
//!
//! - width: East Asian Width (UAX #11), clamped to [0, 2], and `uucode_config.zig`'s `computeWidth` (width 0 iff zero
//!   width in a grapheme, not an emoji modifier and not `prepend`; otherwise min(2, the standard width));
//! - zero width in a grapheme: uucode's `wcwidth_zero_in_grapheme`;
//! - grapheme break: `Grapheme_Cluster_Break` (UAX #29) folded into `GraphemeBreakNoControl`, with the Indic conjunct
//!   break and emoji classes;
//! - emoji VS base: the bases of `emoji-variation-sequences.txt` (digits, `#` and `*` take the keycap VS16).

use super::table;
use crate::unicode::props::GraphemeBreakNoControl as G;

/// `(code point, width, zero width in a grapheme, grapheme break, emoji VS base, what it is)`.
type Case = (u32, u8, bool, G, bool, &'static str);

const CASES: &[Case] = &[
    // ASCII and Latin: width 1, no special break.
    (0x0041, 1, false, G::Other, false, "U+0041 'A'"),
    (0x007A, 1, false, G::Other, false, "U+007A 'z'"),
    (0x0020, 1, false, G::Other, false, "U+0020 SPACE"),
    (0x007E, 1, false, G::Other, false, "U+007E '~'"),
    // ASCII digits, '#' and '*' are emoji VS bases (keycaps take VS16).
    (0x0039, 1, false, G::Other, true, "U+0039 '9'"),
    (0x0023, 1, false, G::Other, true, "U+0023 '#'"),
    (0x002A, 1, false, G::Other, true, "U+002A '*'"),
    // Controls: width 0, zero width in a grapheme, `Other`.
    (0x0000, 0, true, G::Other, false, "U+0000 NUL"),
    (0x0009, 0, true, G::Other, false, "U+0009 TAB"),
    // Wide CJK and kana.
    (0x4E00, 2, false, G::Other, false, "U+4E00 CJK ideograph one"),
    (0x3042, 2, false, G::Other, false, "U+3042 HIRAGANA LETTER A"),
    (0x2329, 2, false, G::Other, false, "U+2329 LEFT-POINTING ANGLE BRACKET"),
    // Fullwidth forms.
    (0xFF01, 2, false, G::Other, false, "U+FF01 FULLWIDTH EXCLAMATION MARK"),
    (
        0xFF21,
        2,
        false,
        G::Other,
        false,
        "U+FF21 FULLWIDTH LATIN CAPITAL LETTER A",
    ),
    // Combining and zero-width code points.
    (
        0x0301,
        0,
        true,
        G::IndicConjunctBreakExtend,
        false,
        "U+0301 COMBINING ACUTE ACCENT",
    ),
    (
        0x0300,
        0,
        true,
        G::IndicConjunctBreakExtend,
        false,
        "U+0300 COMBINING GRAVE ACCENT",
    ),
    (0x200B, 0, true, G::Other, false, "U+200B ZERO WIDTH SPACE"),
    (0x200D, 0, true, G::Zwj, false, "U+200D ZERO WIDTH JOINER"),
    (
        0xFE0F,
        0,
        true,
        G::IndicConjunctBreakExtend,
        false,
        "U+FE0F VARIATION SELECTOR-16",
    ),
    // An emoji, and an emoji modifier, which keeps width 2 although it has zero width in a grapheme.
    (
        0x1F600,
        2,
        false,
        G::ExtendedPictographic,
        false,
        "U+1F600 GRINNING FACE",
    ),
    (
        0x1F3FF,
        2,
        true,
        G::EmojiModifier,
        false,
        "U+1F3FF EMOJI MODIFIER FITZPATRICK TYPE-6",
    ),
    // A regional indicator (flags).
    (
        0x1F1E6,
        2,
        false,
        G::RegionalIndicator,
        false,
        "U+1F1E6 REGIONAL INDICATOR SYMBOL LETTER A",
    ),
    // `prepend` has zero width in a grapheme but keeps width 1.
    (0x0600, 1, true, G::Prepend, false, "U+0600 ARABIC NUMBER SIGN"),
    // Hangul jamo and syllables.
    (0x1100, 2, false, G::L, false, "U+1100 HANGUL CHOSEONG KIYEOK"),
    (0x1160, 0, true, G::V, false, "U+1160 HANGUL JUNGSEONG FILLER"),
    (0x11A8, 0, true, G::T, false, "U+11A8 HANGUL JONGSEONG KIYEOK"),
    (0xAC00, 2, false, G::Lv, false, "U+AC00 HANGUL SYLLABLE GA"),
    (0xAC01, 2, false, G::Lvt, false, "U+AC01 HANGUL SYLLABLE GAG"),
    // Indic conjunct break: a consonant and a linker (virama).
    (
        0x0915,
        1,
        false,
        G::IndicConjunctBreakConsonant,
        false,
        "U+0915 DEVANAGARI LETTER KA",
    ),
    (
        0x094D,
        0,
        true,
        G::IndicConjunctBreakLinkerExtend,
        false,
        "U+094D DEVANAGARI SIGN VIRAMA",
    ),
    // Unicode 18: InCB=Linker with GCB=Other.
    (
        0x1CF5,
        1,
        false,
        G::IndicConjunctBreakLinkerOther,
        false,
        "U+1CF5 VEDIC SIGN JIHVAMULIYA",
    ),
    (
        0x11A3A,
        1,
        false,
        G::IndicConjunctBreakLinkerOther,
        false,
        "U+11A3A ZANABAZAR SQUARE CLUSTER-INITIAL RA",
    ),
    // Unicode 18: Extend growth and new wide ranges.
    (
        0x05C8,
        0,
        true,
        G::IndicConjunctBreakExtend,
        false,
        "U+05C8 HEBREW POINT SHEVA NA MUDGASH",
    ),
    (
        0x18E00,
        2,
        false,
        G::Other,
        false,
        "U+18E00 the first Jurchen character",
    ),
    (0x3D000, 2, false, G::Other, false, "U+3D000 the first seal character"),
    (
        0x1F6D9,
        2,
        false,
        G::ExtendedPictographic,
        false,
        "U+1F6D9 LIGHTHOUSE (E18.0)",
    ),
];

#[test]
fn unicode_props_match_the_standard() {
    for &(cp, width, zero_in_grapheme, grapheme_break, emoji_vs_base, what) in CASES {
        let p = table().get(cp);
        assert_eq!(p.width, width, "{what}: width");
        assert_eq!(
            p.width_zero_in_grapheme, zero_in_grapheme,
            "{what}: width_zero_in_grapheme"
        );
        assert_eq!(p.grapheme_break, grapheme_break, "{what}: grapheme_break");
        assert_eq!(p.emoji_vs_base, emoji_vs_base, "{what}: emoji_vs_base");
    }
}

/// Above U+10FFFF the generator's rule holds: width 1, zero width in a grapheme, `Other`, no emoji VS base. The two
/// values past the `u21` domain here mask onto such code points.
#[test]
fn code_points_above_the_unicode_range_follow_the_out_of_range_rule() {
    for cp in [0x11_0000, 0x15_ABCD, 0x1F_FFFF, 0x20_0000 | 0x11_0000, u32::MAX] {
        let p = table().get(cp);
        assert_eq!(p.width, 1, "U+{cp:X}: width");
        assert!(p.width_zero_in_grapheme, "U+{cp:X}: width_zero_in_grapheme");
        assert_eq!(p.grapheme_break, G::Other, "U+{cp:X}: grapheme_break");
        assert!(!p.emoji_vs_base, "U+{cp:X}: emoji_vs_base");
    }
}
