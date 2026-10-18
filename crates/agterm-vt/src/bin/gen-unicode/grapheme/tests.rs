//! uucode's `BreakState` test, the grapheme-break rules one by one, the precomputed table against the one the
//! library ships, and the `GraphemeBreakTest.txt` check on a small test file.

use super::{
    ALL_BREAKS, BreakState, check_grapheme_break_test, compute_grapheme_break, key_index, precompute, table_break,
};
use crate::derive::Derived;
use crate::props::GraphemeBreakNoControl as G;
use crate::ucd::{IndicConjunctBreak, OriginalGraphemeBreak, Ucd, UcdFile};

/// The table the library reads, `src/unicode/generated/grapheme_data.rs`.
#[path = "../../../unicode/generated/grapheme_data.rs"]
mod shipped;

/// uucode's `test "BreakState table index layout"`.
#[test]
fn break_state_table_index_layout() {
    let mut state = BreakState::default();
    assert_eq!(state.table_index(), 0);
    state.set_after_linker(true);
    assert_eq!(state.table_index(), 1);
    state.set_after_linker(false);
    state.set_base(super::Base::ExtendedPictographic);
    assert_eq!(state.table_index(), 2);
    state.set_after_linker(true);
    assert_eq!(state.table_index(), 3);
    state.set_after_linker(false);
    state.set_base(super::Base::RegionalIndicator);
    assert_eq!(state.table_index(), 4);
    assert_eq!(BreakState::TABLE_LEN, 5);
    state.set_after_linker(true);
    assert_eq!(state.table_index(), 5);
    for i in 0..BreakState::TABLE_LEN {
        assert_eq!(BreakState::from_table_index(i).table_index(), i);
    }
}

/// The precompute depends on the rules alone, not on the UCD: it is the shipped table (rustty's, which its checksum
/// in `generated` pins), byte for byte.
#[test]
fn the_precompute_is_the_shipped_table() {
    assert_eq!(precompute(), shipped::DATA);
}

/// Every key the table has a row for holds what the rules give; the rest are zero, and no value names a state
/// without a row.
#[test]
fn the_precompute_holds_the_rules_for_every_reachable_key() {
    let data = precompute();
    let mut reachable = vec![false; data.len()];
    for row in 0..BreakState::TABLE_LEN {
        for gb1 in ALL_BREAKS {
            for gb2 in ALL_BREAKS {
                let before = BreakState::from_table_index(row);
                let mut expected_state = before;
                let expected = compute_grapheme_break(gb1, gb2, &mut expected_state);
                let mut state = before;
                assert_eq!(table_break(&data, gb1, gb2, &mut state), expected);
                assert_eq!(state, expected_state);
                reachable[key_index(before, gb1, gb2)] = true;
            }
        }
    }
    for (key, value) in data.iter().enumerate() {
        if !reachable[key] {
            assert_eq!(*value, 0, "key {key}");
        }
        assert!(value >> 1 < BreakState::TABLE_LEN, "key {key}");
    }
}

/// Whether each break of a sequence of properties falls where `breaks` says, starting from the default state.
fn breaks(sequence: &[G]) -> Vec<bool> {
    let mut state = BreakState::default();
    sequence
        .windows(2)
        .map(|pair| compute_grapheme_break(pair[0], pair[1], &mut state))
        .collect()
}

#[test]
fn hangul_syllables_marks_and_prepends_do_not_break() {
    // GB6, GB7, GB8
    assert_eq!(breaks(&[G::L, G::V, G::T, G::T]), [false, false, false]);
    assert_eq!(breaks(&[G::L, G::Lv, G::V, G::T]), [false, false, false]);
    assert_eq!(breaks(&[G::Lvt, G::T, G::L]), [false, true]);
    assert_eq!(breaks(&[G::T, G::V]), [true]);
    // GB9a, GB9b, GB9
    assert_eq!(breaks(&[G::Other, G::SpacingMark]), [false]);
    assert_eq!(breaks(&[G::Prepend, G::Other]), [false]);
    assert_eq!(
        breaks(&[G::Other, G::IndicConjunctBreakExtend, G::Zwj, G::Zwnj]),
        [false, false, false]
    );
    // GB999
    assert_eq!(breaks(&[G::Other, G::Other]), [true]);
}

/// `GB9c`: a consonant joins a conjunct after a linker, with extends and ZWJs between.
#[test]
fn indic_conjuncts_do_not_break() {
    use G::{IndicConjunctBreakConsonant as C, IndicConjunctBreakExtend as E, IndicConjunctBreakLinkerExtend as L};
    assert_eq!(breaks(&[C, L, C]), [false, false]);
    assert_eq!(breaks(&[C, E, L, E, G::Zwj, C]), [false, false, false, false, false]);
    // a linker that is not Extend (U+1CF5) starts a cluster of its own, but still links the next consonant
    assert_eq!(breaks(&[C, G::IndicConjunctBreakLinkerOther, C]), [true, false]);
    // no linker before the consonant: a break
    assert_eq!(breaks(&[C, E, C]), [false, true]);
}

/// `GB11`, with uucode's emoji modifier tailoring.
#[test]
fn emoji_sequences_do_not_break() {
    use G::{EmojiModifier as M, EmojiModifierBase as B, ExtendedPictographic as P};
    assert_eq!(breaks(&[P, G::Zwj, P]), [false, false]);
    assert_eq!(
        breaks(&[P, G::IndicConjunctBreakExtend, G::Zwj, P]),
        [false, false, false]
    );
    assert_eq!(breaks(&[B, M]), [false]);
    assert_eq!(breaks(&[B, M, G::Zwj, P]), [false, false, false]);
    // a modifier after anything but its base, and a ZWJ before something not pictographic
    assert_eq!(breaks(&[G::Other, M]), [true]);
    assert_eq!(breaks(&[P, G::Zwj, G::Other]), [false, true]);
    assert_eq!(breaks(&[G::Other, G::Zwj, P]), [false, true]);
}

/// `GB12` and `GB13`: regional indicators pair up.
#[test]
fn regional_indicators_pair_up() {
    let ri = G::RegionalIndicator;
    assert_eq!(breaks(&[ri, ri, ri, ri, ri]), [false, true, false, true]);
    assert_eq!(breaks(&[G::Other, ri, ri]), [true, false]);
}

/// The code points the test file below uses, with their real properties.
fn derived() -> Derived {
    let mut u = Ucd::blank();
    u.original_grapheme_break[0x308] = OriginalGraphemeBreak::Extend;
    u.indic_conjunct_break[0x308] = IndicConjunctBreak::Extend;
    u.original_grapheme_break[0x1F1E6] = OriginalGraphemeBreak::RegionalIndicator;
    u.original_grapheme_break[0xD] = OriginalGraphemeBreak::Cr;
    u.original_grapheme_break[0xA] = OriginalGraphemeBreak::Lf;
    u.is_extended_pictographic[0x261D] = true;
    u.is_emoji_modifier_base[0x261D] = true;
    u.original_grapheme_break[0x1F3FB] = OriginalGraphemeBreak::Extend;
    u.is_emoji_modifier[0x1F3FB] = true;
    u.is_emoji_component[0x1F3FB] = true;
    Derived::new(&u).unwrap()
}

fn test_file(text: &str) -> UcdFile {
    UcdFile {
        name: "GraphemeBreakTest.txt".to_owned(),
        text: text.to_owned(),
    }
}

/// Lines in the test file's format: pairs up to the first control are checked, and an emoji modifier after
/// anything but its base breaks, though UAX #29 says it does not.
#[test]
fn the_test_file_check_passes_the_table() {
    let file = test_file(
        "# GraphemeBreakTest-18.0.0.txt\n\
         ÷ 0020 ÷ 0020 ÷\t#  ÷ [0.2] SPACE (Other) ÷ [999.0] SPACE (Other) ÷ [0.3]\n\
         ÷ 0020 × 0308 ÷\n\
         ÷ 1F1E6 × 1F1E6 ÷ 1F1E6 ÷\n\
         ÷ 000D × 000A ÷\n\
         ÷ 0020 ÷ 000D ÷\n\
         ÷ 0020 × 0308 ÷ 000A ÷\n\
         ÷ 261D × 1F3FB ÷\n\
         ÷ 0020 × 1F3FB ÷\n",
    );
    assert_eq!(check_grapheme_break_test(&file, &derived(), &precompute()), Ok(7));
}

#[test]
fn the_test_file_check_reports_every_mismatch() {
    let file = test_file("÷ 0020 × 0020 ÷\n÷ 0020 ÷ 0308 ÷\n÷ 0020 ÷ 0020 ÷\n");
    let error = check_grapheme_break_test(&file, &derived(), &precompute()).unwrap_err();
    assert!(error.starts_with("2 GraphemeBreakTest.txt mismatches:"), "{error}");
    assert!(
        error.contains("GraphemeBreakTest.txt:1: U+0020 U+0020: expected break false"),
        "{error}"
    );
    assert!(
        error.contains("GraphemeBreakTest.txt:2: U+0020 U+0308: expected break true"),
        "{error}"
    );
}

/// The tailoring turns a × before an emoji modifier into a ÷: a file that already says ÷ there breaks its
/// assumption, which uucode's `inlineAssert(!expected_is_break)` holds.
#[test]
fn the_test_file_check_refuses_a_break_before_an_emoji_modifier() {
    let file = test_file("÷ 261D × 1F3FB ÷\n÷ 0020 ÷ 1F3FB ÷\n");
    assert_eq!(
        check_grapheme_break_test(&file, &derived(), &precompute()),
        Err(
            "GraphemeBreakTest.txt:2: U+0020 U+1F3FB: ÷ before an emoji modifier not after its base, where UAX #29 \
             has ×"
                .to_owned()
        )
    );
}

#[test]
fn the_test_file_check_refuses_malformed_lines() {
    for line in ["0020 ÷ 0020 ÷", "÷ 0020 + 0020 ÷", "÷ 0020 ÷ 0020 ×", "÷ 0020 ÷ 0020"] {
        assert!(
            check_grapheme_break_test(&test_file(line), &derived(), &precompute()).is_err(),
            "{line}"
        );
    }
}
