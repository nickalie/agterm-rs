//! The grapheme-break rules without controls, the precomputed table Ghostty builds from them, and uucode's
//! `GraphemeBreakTest.txt` check of the result.
//!
//! uucode: `src/grapheme.zig` at `UUCODE_REV` (`build/pins.env`, MIT): `BreakState`, `computeGraphemeBreakNoControl`
//! with its `is*NoControl` helpers, and `testGraphemeBreakNoControl` (the `GraphemeBreakTest.txt` tests).
//! Ghostty: `src/unicode/grapheme.zig`'s comptime `Precompute` at `LIBGHOSTTY_VT_REV`.
//! Changes: `BreakState` is its `u3` bit pattern (`base * 2 + after_linker`), as the library's `BreakState` is. The
//! precompute's keys that cannot occur (a state past `table_len`, a break property past the enum's last value) are
//! zero, where Zig leaves them `undefined`. The `GraphemeBreakTest.txt` check runs on every generation, over the
//! table the library ships, and fails it on any mismatch; it stands in for Ghostty's debug `main`, which compared the
//! table with uucode's rules over every pair of code points and was never run in CI.

use crate::derive::{Derived, GraphemeBreak, no_control};
use crate::props::GraphemeBreakNoControl;
use crate::ucd::{self, Result, UcdFile};

/// uucode's `BreakState`, `packed struct(u3) { after_linker: bool, base: Base }`, as its bit pattern.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct BreakState(u8);

/// `BreakState.Base`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Base {
    Default = 0,
    ExtendedPictographic = 1,
    RegionalIndicator = 2,
}

impl BreakState {
    /// `table_len`: every `Base` with and without `after_linker`, less the one combination that never occurs (a
    /// regional indicator after a linker), which is the highest bit pattern.
    pub(crate) const TABLE_LEN: u8 = 3 * 2 - 1;

    /// `fromTableIndex`.
    pub(crate) const fn from_table_index(index: u8) -> Self {
        Self(index)
    }

    /// `tableIndex`.
    pub(crate) const fn table_index(self) -> u8 {
        self.0
    }

    fn after_linker(self) -> bool {
        self.0 & 1 != 0
    }

    fn set_after_linker(&mut self, after_linker: bool) {
        self.0 = (self.0 & !1) | u8::from(after_linker);
    }

    fn base(self) -> Base {
        match self.0 >> 1 {
            0 => Base::Default,
            1 => Base::ExtendedPictographic,
            _ => Base::RegionalIndicator,
        }
    }

    fn set_base(&mut self, base: Base) {
        self.0 = (self.0 & 1) | ((base as u8) << 1);
    }
}

fn is_indic_conjunct_break_extend(gb: GraphemeBreakNoControl) -> bool {
    matches!(
        gb,
        GraphemeBreakNoControl::IndicConjunctBreakExtend | GraphemeBreakNoControl::Zwj
    )
}

fn is_indic_conjunct_break_linker(gb: GraphemeBreakNoControl) -> bool {
    matches!(
        gb,
        GraphemeBreakNoControl::IndicConjunctBreakLinkerExtend | GraphemeBreakNoControl::IndicConjunctBreakLinkerOther
    )
}

/// `isExtendNoControl`. Emoji modifiers are `Extend` in UAX #29 but not here: uucode follows UTS #51, where a
/// modifier only joins the emoji modifier base right before it (a tailoring UAX #29 allows).
fn is_extend(gb: GraphemeBreakNoControl) -> bool {
    matches!(
        gb,
        GraphemeBreakNoControl::Zwnj
            | GraphemeBreakNoControl::IndicConjunctBreakExtend
            | GraphemeBreakNoControl::IndicConjunctBreakLinkerExtend
    )
}

fn is_extended_pictographic(gb: GraphemeBreakNoControl) -> bool {
    matches!(
        gb,
        GraphemeBreakNoControl::ExtendedPictographic | GraphemeBreakNoControl::EmojiModifierBase
    )
}

/// Whether an emoji base continues through `gb` (the `extended_pictographic` state's keep list).
fn keeps_emoji_base(gb: GraphemeBreakNoControl) -> bool {
    is_extend(gb)
        || matches!(
            gb,
            GraphemeBreakNoControl::Zwj
                | GraphemeBreakNoControl::ExtendedPictographic
                | GraphemeBreakNoControl::EmojiModifierBase
                | GraphemeBreakNoControl::EmojiModifier
        )
}

/// `computeGraphemeBreakNoControl`: whether there is a break between two code points of these properties, updating
/// the state carried between calls. Controls, CR and LF are `Other` here: the caller handles them first.
pub(crate) fn compute_grapheme_break(
    gb1: GraphemeBreakNoControl,
    gb2: GraphemeBreakNoControl,
    state: &mut BreakState,
) -> bool {
    use GraphemeBreakNoControl as G;
    // whether the next flag is set depends only on the input, not on the rules below, which only read
    // `after_linker` and only write `base`
    let after_linker =
        is_indic_conjunct_break_linker(gb1) || (state.after_linker() && is_indic_conjunct_break_extend(gb1));
    state.set_after_linker(after_linker && is_indic_conjunct_break_extend(gb2));

    match state.base() {
        Base::RegionalIndicator if gb1 != G::RegionalIndicator || gb2 != G::RegionalIndicator => {
            state.set_base(Base::Default);
        }
        Base::ExtendedPictographic if !keeps_emoji_base(gb1) || !keeps_emoji_base(gb2) => state.set_base(Base::Default),
        _ => {}
    }

    if hangul_or_mark_joins(gb1, gb2) {
        return false;
    }

    // GB9c: InCB=Linker InCB=Extend* x InCB=Consonant
    if after_linker && gb2 == G::IndicConjunctBreakConsonant {
        state.set_base(Base::Default);
        return false;
    }

    if let Some(result) = emoji_sequence_break(gb1, gb2, state) {
        return result;
    }

    // GB12 and GB13: Regional Indicator
    if gb1 == G::RegionalIndicator && gb2 == G::RegionalIndicator {
        if state.base() == Base::Default {
            state.set_base(Base::RegionalIndicator);
            return false;
        }
        state.set_base(Base::Default);
        return true;
    }

    // GB9: x (Extend | ZWJ); GB999: otherwise, break everywhere
    !(is_extend(gb2) || gb2 == G::Zwj)
}

/// `GB6` to `GB9b`: the Hangul syllable sequences, spacing marks and prepends, which never break.
fn hangul_or_mark_joins(gb1: GraphemeBreakNoControl, gb2: GraphemeBreakNoControl) -> bool {
    use GraphemeBreakNoControl as G;
    // GB6: L x (L | V | LV | LVT)
    (gb1 == G::L && matches!(gb2, G::L | G::V | G::Lv | G::Lvt))
        // GB7: (LV | V) x (V | T)
        || (matches!(gb1, G::Lv | G::V) && matches!(gb2, G::V | G::T))
        // GB8: (LVT | T) x T
        || (matches!(gb1, G::Lvt | G::T) && gb2 == G::T)
        // GB9a: x SpacingMark
        || gb2 == G::SpacingMark
        // GB9b: Prepend x
        || gb1 == G::Prepend
}

/// GB11: emoji ZWJ sequences and emoji modifier sequences. `Some` when the rule decides.
fn emoji_sequence_break(
    gb1: GraphemeBreakNoControl,
    gb2: GraphemeBreakNoControl,
    state: &mut BreakState,
) -> Option<bool> {
    use GraphemeBreakNoControl as G;
    if is_extended_pictographic(gb1) {
        if is_extend(gb2) || gb2 == G::Zwj || (gb1 == G::EmojiModifierBase && gb2 == G::EmojiModifier) {
            state.set_base(Base::ExtendedPictographic);
            return Some(false);
        }
    } else if state.base() == Base::ExtendedPictographic {
        if (is_extend(gb1) || gb1 == G::EmojiModifier) && (is_extend(gb2) || gb2 == G::Zwj) {
            return Some(false);
        }
        state.set_base(Base::Default);
        if gb1 == G::Zwj && is_extended_pictographic(gb2) {
            return Some(false);
        }
    }
    None
}

/// Every `GraphemeBreakNoControl` value, in discriminant order.
pub(crate) const ALL_BREAKS: [GraphemeBreakNoControl; 18] = {
    use GraphemeBreakNoControl as G;
    [
        G::Other,
        G::Prepend,
        G::RegionalIndicator,
        G::SpacingMark,
        G::L,
        G::V,
        G::T,
        G::Lv,
        G::Lvt,
        G::Zwj,
        G::Zwnj,
        G::ExtendedPictographic,
        G::EmojiModifierBase,
        G::EmojiModifier,
        G::IndicConjunctBreakExtend,
        G::IndicConjunctBreakLinkerExtend,
        G::IndicConjunctBreakLinkerOther,
        G::IndicConjunctBreakConsonant,
    ]
};

/// The index of a precompute key: `packed struct(u13) { state: u3, gb1: u5, gb2: u5 }`.
pub(crate) fn key_index(state: BreakState, gb1: GraphemeBreakNoControl, gb2: GraphemeBreakNoControl) -> usize {
    usize::from(state.table_index()) | ((gb1 as usize) << 3) | ((gb2 as usize) << 8)
}

/// Ghostty's `Precompute.data` as bytes: for every key, `result | state << 1` (`packed struct(u8)`).
pub(crate) fn precompute() -> [u8; 8192] {
    let mut data = [0_u8; 8192];
    for row in 0..BreakState::TABLE_LEN {
        for gb1 in ALL_BREAKS {
            for gb2 in ALL_BREAKS {
                let mut state = BreakState::from_table_index(row);
                let key = key_index(state, gb1, gb2);
                let result = compute_grapheme_break(gb1, gb2, &mut state);
                data[key] = u8::from(result) | (state.table_index() << 1);
            }
        }
    }
    data
}

/// The break the precomputed table gives, as the library's `graphemeBreak` reads it.
fn table_break(
    data: &[u8; 8192],
    gb1: GraphemeBreakNoControl,
    gb2: GraphemeBreakNoControl,
    state: &mut BreakState,
) -> bool {
    let value = data[key_index(*state, gb1, gb2)];
    *state = BreakState::from_table_index(value >> 1);
    value & 1 != 0
}

/// uucode's `testGraphemeBreakNoControl` over the precomputed table: every `GraphemeBreakTest.txt` line, cut at its
/// first control, CR or LF (which the table does not handle), must break where the file says, except that an emoji
/// modifier after anything but an emoji modifier base breaks (uucode's UTS #51 tailoring). Returns the number of
/// pairs checked.
pub(crate) fn check_grapheme_break_test(file: &UcdFile, derived: &Derived, data: &[u8; 8192]) -> Result<usize> {
    let mut checked = 0;
    let mut failures = Vec::new();
    for (line, text) in file.data_lines() {
        checked += check_test_line(file, line, text, derived, data, &mut failures)?;
    }
    if failures.is_empty() {
        Ok(checked)
    } else {
        Err(format!(
            "{} GraphemeBreakTest.txt mismatches:\n{}",
            failures.len(),
            failures.join("\n")
        ))
    }
}

/// One `÷ cp ×|÷ cp ... ÷` line of the test file.
fn check_test_line(
    file: &UcdFile,
    line: usize,
    text: &str,
    derived: &Derived,
    data: &[u8; 8192],
    failures: &mut Vec<String>,
) -> Result<usize> {
    let mut parts = text.split(' ');
    if parts.next() != Some("÷") {
        return Err(file.error(line, "a test line that does not start with ÷"));
    }
    let tokens: Vec<&str> = parts.collect();
    let mut cps = Vec::with_capacity(tokens.len().div_ceil(2));
    let mut marks = Vec::with_capacity(cps.capacity());
    for pair in tokens.chunks(2) {
        cps.push(ucd::parse_cp(pair[0]).map_err(|e| file.error(line, e))?);
        let mark = pair.get(1).copied().unwrap_or("");
        if mark != "÷" && mark != "×" {
            return Err(file.error(line, format!("{mark:?} is neither ÷ nor ×")));
        }
        marks.push(mark == "÷");
    }
    let is_control = |cp: u32| {
        matches!(
            derived.grapheme_break[ucd::index(cp)],
            GraphemeBreak::Control | GraphemeBreak::Cr | GraphemeBreak::Lf
        )
    };
    let mut state = BreakState::default();
    let mut checked = 0;
    for (i, pair) in cps.windows(2).enumerate() {
        if is_control(pair[0]) || is_control(pair[1]) {
            break;
        }
        let gb1 = no_control(derived.grapheme_break[ucd::index(pair[0])]);
        let gb2 = no_control(derived.grapheme_break[ucd::index(pair[1])]);
        let mut expected = marks[i];
        if gb2 == GraphemeBreakNoControl::EmojiModifier && gb1 != GraphemeBreakNoControl::EmojiModifierBase {
            expected = true;
        }
        if table_break(data, gb1, gb2, &mut state) != expected {
            failures.push(file.error(
                line,
                format!("U+{:04X} U+{:04X}: expected break {expected}", pair[0], pair[1]),
            ));
        }
        checked += 1;
    }
    if marks.last() != Some(&true) {
        return Err(file.error(line, "a test line that does not end with ÷"));
    }
    Ok(checked)
}

#[cfg(test)]
mod tests;
