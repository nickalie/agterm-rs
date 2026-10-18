//! The derivations on a UCD built by hand: uucode's grapheme-break and width rules, Ghostty's width and symbol rules,
//! the rule above U+10FFFF, and the assumptions the derivation refuses to go on without.

use super::{Derived, GraphemeBreak, no_control};
use crate::props::{GraphemeBreakNoControl, Properties};
use crate::ucd::{CaseFolding, EastAsianWidth, GeneralCategory, IndicConjunctBreak, OriginalGraphemeBreak, Ucd};

fn ucd() -> Ucd {
    Ucd::blank()
}

fn props(width: u8, zero: bool, gb: GraphemeBreakNoControl) -> Properties {
    Properties {
        width,
        width_zero_in_grapheme: zero,
        grapheme_break: gb,
        emoji_vs_base: false,
    }
}

#[test]
fn widths_follow_wcwidth_and_ghosttys_clamp() {
    use GraphemeBreakNoControl as N;
    let mut u = ucd();
    u.general_category[0x07] = GeneralCategory::OtherControl;
    u.general_category[0x2028] = GeneralCategory::SeparatorLine;
    u.general_category[0xD800] = GeneralCategory::OtherSurrogate;
    // the soft hyphen is default ignorable, but one cell wide
    u.is_default_ignorable[0xAD] = true;
    u.is_default_ignorable[0x200B] = true;
    u.east_asian_width[0x4E00] = EastAsianWidth::Wide;
    u.east_asian_width[0xFF01] = EastAsianWidth::Fullwidth;
    u.east_asian_width[0xFF61] = EastAsianWidth::Halfwidth;
    u.original_grapheme_break[0x1F1E6] = OriginalGraphemeBreak::RegionalIndicator;
    u.general_category[0x300] = GeneralCategory::MarkNonspacing;
    u.original_grapheme_break[0x300] = OriginalGraphemeBreak::Extend;
    u.indic_conjunct_break[0x300] = IndicConjunctBreak::Extend;
    u.general_category[0x20E3] = GeneralCategory::MarkEnclosing;
    let d = Derived::new(&u).unwrap();
    assert_eq!(d.properties_of(0x41), props(1, false, N::Other));
    assert_eq!(d.properties_of(0x07), props(0, true, N::Other));
    assert_eq!(d.properties_of(0x2028), props(0, true, N::Other));
    assert_eq!(d.properties_of(0xD800), props(0, true, N::Other));
    assert_eq!(d.properties_of(0xAD), props(1, false, N::Other));
    assert_eq!(d.properties_of(0x200B), props(0, true, N::Other));
    // U+2E3A TWO-EM DASH is two cells, U+2E3B THREE-EM DASH three, clamped to two
    assert_eq!(d.properties_of(0x2E3A), props(2, false, N::Other));
    assert_eq!(d.properties_of(0x2E3B), props(2, false, N::Other));
    assert_eq!(d.properties_of(0x4E00), props(2, false, N::Other));
    assert_eq!(d.properties_of(0xFF01), props(2, false, N::Other));
    assert_eq!(d.properties_of(0xFF61), props(1, false, N::Other));
    assert_eq!(d.properties_of(0x1F1E6), props(2, false, N::RegionalIndicator));
    assert_eq!(d.properties_of(0x300), props(0, true, N::IndicConjunctBreakExtend));
    // U+20E3 COMBINING ENCLOSING KEYCAP stands alone two wide, but is zero wide in a grapheme, and Ghostty has one
    // width: zero
    assert_eq!(d.properties_of(0x20E3), props(0, true, N::Other));
}

/// Emoji modifiers and prepends are zero wide in a grapheme, but Ghostty keeps their standalone width; Hangul V and
/// T jamo are zero wide.
#[test]
fn emoji_modifiers_and_prepends_keep_their_width() {
    use GraphemeBreakNoControl as N;
    let mut u = ucd();
    u.is_emoji_modifier[0x1F3FB] = true;
    u.is_emoji_component[0x1F3FB] = true;
    u.original_grapheme_break[0x1F3FB] = OriginalGraphemeBreak::Extend;
    u.east_asian_width[0x1F3FB] = EastAsianWidth::Wide;
    u.original_grapheme_break[0x600] = OriginalGraphemeBreak::Prepend;
    u.original_grapheme_break[0x1160] = OriginalGraphemeBreak::V;
    u.original_grapheme_break[0x11A8] = OriginalGraphemeBreak::T;
    let d = Derived::new(&u).unwrap();
    assert_eq!(d.properties_of(0x1F3FB), props(2, true, N::EmojiModifier));
    assert_eq!(d.properties_of(0x600), props(1, true, N::Prepend));
    assert_eq!(d.properties_of(0x1160), props(0, true, N::V));
    assert_eq!(d.properties_of(0x11A8), props(0, true, N::T));
}

/// `props_uucode.get` above U+10FFFF, and the symbols context there.
#[test]
fn code_points_above_the_unicode_range_have_the_fixed_properties() {
    let d = Derived::new(&ucd()).unwrap();
    for cp in [0x11_0000, 0x1F_FFFF] {
        assert_eq!(
            d.properties_of(cp),
            props(1, true, GraphemeBreakNoControl::Other),
            "U+{cp:X}"
        );
        assert!(!d.is_symbol_of(cp));
    }
}

#[test]
fn symbols_are_private_use_or_in_a_symbol_block() {
    let mut u = ucd();
    u.general_category[0xE000] = GeneralCategory::OtherPrivateUse;
    u.in_symbol_block[0x2190] = true;
    let d = Derived::new(&u).unwrap();
    assert!(d.is_symbol_of(0xE000));
    assert!(d.is_symbol_of(0x2190));
    assert!(!d.is_symbol_of(0x41));
}

#[test]
fn emoji_vs_bases_carry_over() {
    let mut u = ucd();
    u.is_emoji_vs_base[0x23] = true;
    let d = Derived::new(&u).unwrap();
    assert!(d.properties_of(0x23).emoji_vs_base);
    assert!(!d.properties_of(0x24).emoji_vs_base);
}

/// `GraphemeBreakDerived`'s refinements: emoji properties first, then the Indic conjunct break, then the original
/// value.
#[test]
fn grapheme_breaks_refine_the_original_values() {
    use GraphemeBreak as G;
    let mut u = ucd();
    u.is_extended_pictographic[0x1F600] = true;
    u.is_extended_pictographic[0x261D] = true;
    u.is_emoji_modifier_base[0x261D] = true;
    u.original_grapheme_break[0x200C] = OriginalGraphemeBreak::Extend;
    u.original_grapheme_break[0x200D] = OriginalGraphemeBreak::Zwj;
    u.indic_conjunct_break[0x200D] = IndicConjunctBreak::Extend;
    u.original_grapheme_break[0x94D] = OriginalGraphemeBreak::Extend;
    u.indic_conjunct_break[0x94D] = IndicConjunctBreak::Linker;
    u.indic_conjunct_break[0x1CF5] = IndicConjunctBreak::Linker;
    u.indic_conjunct_break[0x915] = IndicConjunctBreak::Consonant;
    u.original_grapheme_break[0xD] = OriginalGraphemeBreak::Cr;
    u.original_grapheme_break[0x1] = OriginalGraphemeBreak::Control;
    let d = Derived::new(&u).unwrap();
    let expected = [
        (0x1F600, G::ExtendedPictographic),
        (0x261D, G::EmojiModifierBase),
        (0x200C, G::Zwnj),
        (0x200D, G::Zwj),
        (0x94D, G::IndicConjunctBreakLinkerExtend),
        (0x1CF5, G::IndicConjunctBreakLinkerOther),
        (0x915, G::IndicConjunctBreakConsonant),
        (0xD, G::Cr),
        (0x1, G::Control),
        (0x41, G::Other),
    ];
    for (cp, gb) in expected {
        assert_eq!(d.grapheme_break[cp], gb, "U+{cp:X}");
    }
    // controls, CR and LF are Other without controls
    assert_eq!(no_control(G::Cr), GraphemeBreakNoControl::Other);
    assert_eq!(no_control(G::Control), GraphemeBreakNoControl::Other);
    assert_eq!(no_control(G::Zwnj), GraphemeBreakNoControl::Zwnj);
}

/// A change to a UCD that breaks one of the derivation's assumptions.
type Breaker = fn(&mut Ucd);

/// uucode asserts its assumptions on the UCD; the generator refuses to go on without them.
#[test]
fn grapheme_breaks_refuse_a_ucd_that_breaks_the_assumptions() {
    let cases: [(&str, Breaker); 4] = [
        ("an Extend that is InCB None and not ZWNJ", |u| {
            u.original_grapheme_break[0x300] = OriginalGraphemeBreak::Extend;
        }),
        ("an emoji modifier that is not Extend", |u| {
            u.is_emoji_modifier[0x300] = true;
        }),
        ("an InCB Consonant that is not Other", |u| {
            u.indic_conjunct_break[0x300] = IndicConjunctBreak::Consonant;
            u.original_grapheme_break[0x300] = OriginalGraphemeBreak::Prepend;
        }),
        ("an InCB Linker that is neither Extend nor Other", |u| {
            u.indic_conjunct_break[0x300] = IndicConjunctBreak::Linker;
            u.original_grapheme_break[0x300] = OriginalGraphemeBreak::SpacingMark;
        }),
    ];
    for (what, break_it) in cases {
        let mut u = ucd();
        break_it(&mut u);
        let Err(error) = Derived::new(&u) else {
            panic!("{what}: accepted");
        };
        assert_eq!(error, format!("U+0300: {what}"));
    }
}

/// `CaseFoldingFull`: the F mapping where there is one, else the C mapping.
#[test]
fn full_case_folding_prefers_the_full_mapping() {
    let mut u = ucd();
    u.case_folding = vec![
        (
            0x41,
            CaseFolding {
                common: Some(0x61),
                full: vec![],
            },
        ),
        (
            0xDF,
            CaseFolding {
                common: None,
                full: vec![0x73, 0x73],
            },
        ),
        (
            0x1E9E,
            CaseFolding {
                common: Some(0xDF),
                full: vec![0x73, 0x73],
            },
        ),
    ];
    let d = Derived::new(&u).unwrap();
    assert_eq!(
        d.case_folding_full,
        [(0x41, vec![0x61]), (0xDF, vec![0x73, 0x73]), (0x1E9E, vec![0x73, 0x73])]
    );
}
