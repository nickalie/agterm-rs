//! The fields Ghostty's tables read, derived from the parsed UCD: uucode's derived fields and the two that Ghostty's
//! uucode config adds.
//!
//! uucode: `src/components.zig` at `UUCODE_REV` (`build/pins.env`, MIT): `GraphemeBreakDerived`,
//! `GraphemeBreakNoControlComponent`, `Wcwidth` and `CaseFoldingFull`, with `types.GraphemeBreak`.
//! Ghostty: `src/build/uucode_config.zig` at `LIBGHOSTTY_VT_REV`: `WidthComponent` (the `width` field) and
//! `IsSymbolComponent` (`is_symbol`); `src/unicode/props_uucode.zig`'s `get` (the `Properties` of a code point, and
//! the rule above U+10FFFF) and `src/unicode/symbols_uucode.zig`'s generator context.
//! Changes: a function per code point instead of uucode's column-at-a-time build components; uucode's
//! `inlineAssert`s and `unreachable`s on the derivation's assumptions are errors naming the code point.

use crate::props::{GraphemeBreakNoControl, Properties};
use crate::ucd::{
    self, CaseFolding, EastAsianWidth, GeneralCategory, IndicConjunctBreak, NUM_CODE_POINTS, OriginalGraphemeBreak,
    Result, Ucd,
};

/// uucode's `types.GraphemeBreak`: `Grapheme_Cluster_Break` refined by the emoji and Indic conjunct break
/// properties.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GraphemeBreak {
    Other,
    Control,
    Prepend,
    Cr,
    Lf,
    RegionalIndicator,
    SpacingMark,
    L,
    V,
    T,
    Lv,
    Lvt,
    Zwj,
    Zwnj,
    ExtendedPictographic,
    EmojiModifierBase,
    EmojiModifier,
    IndicConjunctBreakExtend,
    IndicConjunctBreakLinkerExtend,
    IndicConjunctBreakLinkerOther,
    IndicConjunctBreakConsonant,
}

/// The derived fields of every code point below `NUM_CODE_POINTS`.
pub(crate) struct Derived {
    /// uucode's `grapheme_break`.
    pub(crate) grapheme_break: Vec<GraphemeBreak>,
    /// Ghostty's `Properties` (`props_uucode.get`).
    pub(crate) properties: Vec<Properties>,
    /// Ghostty's `is_symbol`.
    pub(crate) is_symbol: Vec<bool>,
    /// uucode's `case_folding_full`, for the code points `CaseFolding.txt` lists with status C or F, sorted.
    pub(crate) case_folding_full: Vec<(u32, Vec<u32>)>,
}

impl Derived {
    /// Derives every field from `ucd`.
    pub(crate) fn new(ucd: &Ucd) -> Result<Self> {
        let mut grapheme_break = Vec::with_capacity(NUM_CODE_POINTS);
        let mut properties = Vec::with_capacity(NUM_CODE_POINTS);
        let mut is_symbol = Vec::with_capacity(NUM_CODE_POINTS);
        for (i, cp) in (0..NUM_CODE_POINTS).zip(0_u32..) {
            let gb = derive_grapheme_break(ucd, i, cp).map_err(|e| format!("U+{cp:04X}: {e}"))?;
            grapheme_break.push(gb);
            properties.push(code_point_properties(ucd, i, cp, gb));
            is_symbol.push(ucd.general_category[i] == GeneralCategory::OtherPrivateUse || ucd.in_symbol_block[i]);
        }
        Ok(Self {
            grapheme_break,
            properties,
            is_symbol,
            case_folding_full: ucd.case_folding.iter().filter_map(case_folding_full).collect(),
        })
    }

    /// The `Properties` of a code point of the generator's `u21` domain: Ghostty's `props.get`.
    pub(crate) fn properties_of(&self, cp: u32) -> Properties {
        self.properties.get(ucd::index(cp)).copied().unwrap_or(Properties {
            width: 1,
            width_zero_in_grapheme: true,
            grapheme_break: GraphemeBreakNoControl::Other,
            emoji_vs_base: false,
        })
    }

    /// Whether a code point of the `u21` domain is a symbol: the symbols generator's context, false above U+10FFFF.
    pub(crate) fn is_symbol_of(&self, cp: u32) -> bool {
        self.is_symbol.get(ucd::index(cp)).copied().unwrap_or(false)
    }
}

/// `GraphemeBreakDerived` for one code point.
fn derive_grapheme_break(ucd: &Ucd, i: usize, cp: u32) -> Result<GraphemeBreak> {
    use GraphemeBreak as G;
    use OriginalGraphemeBreak as O;
    let original = ucd.original_grapheme_break[i];
    let extended_pictographic = ucd.is_extended_pictographic[i];
    let expect = |holds: bool, what: &str| if holds { Ok(()) } else { Err(what.to_owned()) };
    if ucd.is_emoji_modifier[i] {
        expect(original == O::Extend, "an emoji modifier that is not Extend")?;
        expect(
            !extended_pictographic && ucd.is_emoji_component[i],
            "an emoji modifier that is pictographic or not a component",
        )?;
        return Ok(G::EmojiModifier);
    }
    if ucd.is_emoji_modifier_base[i] {
        expect(original == O::Other, "an emoji modifier base that is not Other")?;
        expect(extended_pictographic, "an emoji modifier base that is not pictographic")?;
        return Ok(G::EmojiModifierBase);
    }
    if extended_pictographic {
        expect(original == O::Other, "an Extended_Pictographic that is not Other")?;
        return Ok(G::ExtendedPictographic);
    }
    match ucd.indic_conjunct_break[i] {
        IndicConjunctBreak::None => match original {
            O::Extend if cp == ucd::ZERO_WIDTH_NON_JOINER => Ok(G::Zwnj),
            O::Extend => Err("an Extend that is InCB None and not ZWNJ".to_owned()),
            other => Ok(no_extend(other)),
        },
        IndicConjunctBreak::Extend if cp == ucd::ZERO_WIDTH_JOINER => {
            expect(original == O::Zwj, "ZWJ that is not ZWJ")?;
            Ok(G::Zwj)
        }
        IndicConjunctBreak::Extend => {
            expect(original == O::Extend, "an InCB Extend that is not Extend")?;
            Ok(G::IndicConjunctBreakExtend)
        }
        IndicConjunctBreak::Linker => match original {
            O::Extend => Ok(G::IndicConjunctBreakLinkerExtend),
            O::Other => Ok(G::IndicConjunctBreakLinkerOther),
            _ => Err("an InCB Linker that is neither Extend nor Other".to_owned()),
        },
        IndicConjunctBreak::Consonant => {
            expect(original == O::Other, "an InCB Consonant that is not Other")?;
            Ok(G::IndicConjunctBreakConsonant)
        }
    }
}

/// The `GraphemeBreak` of the same name as an original value other than `Extend` (`stringToEnum` on the tag).
fn no_extend(original: OriginalGraphemeBreak) -> GraphemeBreak {
    use GraphemeBreak as G;
    use OriginalGraphemeBreak as O;
    match original {
        O::Other | O::Extend => G::Other,
        O::Prepend => G::Prepend,
        O::Cr => G::Cr,
        O::Lf => G::Lf,
        O::Control => G::Control,
        O::RegionalIndicator => G::RegionalIndicator,
        O::SpacingMark => G::SpacingMark,
        O::L => G::L,
        O::V => G::V,
        O::T => G::T,
        O::Lv => G::Lv,
        O::Lvt => G::Lvt,
        O::Zwj => G::Zwj,
    }
}

/// `GraphemeBreakNoControlComponent`: controls, CR and LF become `Other`, the rest keep their name.
pub(crate) fn no_control(gb: GraphemeBreak) -> GraphemeBreakNoControl {
    use GraphemeBreak as G;
    use GraphemeBreakNoControl as N;
    match gb {
        G::Other | G::Control | G::Cr | G::Lf => N::Other,
        G::Prepend => N::Prepend,
        G::RegionalIndicator => N::RegionalIndicator,
        G::SpacingMark => N::SpacingMark,
        G::L => N::L,
        G::V => N::V,
        G::T => N::T,
        G::Lv => N::Lv,
        G::Lvt => N::Lvt,
        G::Zwj => N::Zwj,
        G::Zwnj => N::Zwnj,
        G::ExtendedPictographic => N::ExtendedPictographic,
        G::EmojiModifierBase => N::EmojiModifierBase,
        G::EmojiModifier => N::EmojiModifier,
        G::IndicConjunctBreakExtend => N::IndicConjunctBreakExtend,
        G::IndicConjunctBreakLinkerExtend => N::IndicConjunctBreakLinkerExtend,
        G::IndicConjunctBreakLinkerOther => N::IndicConjunctBreakLinkerOther,
        G::IndicConjunctBreakConsonant => N::IndicConjunctBreakConsonant,
    }
}

/// `Wcwidth`'s width before its U+20E3 exception: what `wcwidth_zero_in_grapheme` is decided from.
fn wcwidth(ucd: &Ucd, i: usize, cp: u32, gb: GraphemeBreak) -> u8 {
    use GeneralCategory as C;
    let gc = ucd.general_category[i];
    if matches!(
        gc,
        C::OtherControl | C::OtherSurrogate | C::SeparatorLine | C::SeparatorParagraph
    ) {
        0
    } else if cp == 0x00AD {
        1
    } else if ucd.is_default_ignorable[i] {
        0
    } else if cp == 0x2E3A {
        2
    } else if cp == 0x2E3B {
        3
    } else if matches!(
        ucd.east_asian_width[i],
        EastAsianWidth::Wide | EastAsianWidth::Fullwidth
    ) || gb == GraphemeBreak::RegionalIndicator
    {
        2
    } else {
        1
    }
}

/// One code point's `Properties`: `Wcwidth`'s `wcwidth_standalone` and `wcwidth_zero_in_grapheme`, then
/// `WidthComponent`'s `width` from them.
fn code_point_properties(ucd: &Ucd, i: usize, cp: u32, gb: GraphemeBreak) -> Properties {
    let width = wcwidth(ucd, i, cp, gb);
    // U+20E3 COMBINING ENCLOSING KEYCAP stands alone two wide, though it is zero wide in a grapheme
    let standalone = if cp == 0x20E3 { 2 } else { width };
    let is_emoji_modifier = ucd.is_emoji_modifier[i];
    let gc = ucd.general_category[i];
    let zero_in_grapheme = width == 0
        || is_emoji_modifier
        || matches!(gc, GeneralCategory::MarkNonspacing | GeneralCategory::MarkEnclosing)
        || matches!(gb, GraphemeBreak::V | GraphemeBreak::T | GraphemeBreak::Prepend);
    let grapheme_break = no_control(gb);
    // Ghostty keeps the standalone width of emoji modifiers (shown alone as color swatches) and of prepend code
    // points (which would vanish at width 0), and clamps the rest to two cells
    let width = if zero_in_grapheme && !is_emoji_modifier && grapheme_break != GraphemeBreakNoControl::Prepend {
        0
    } else {
        standalone.min(2)
    };
    Properties {
        width,
        width_zero_in_grapheme: zero_in_grapheme,
        grapheme_break,
        emoji_vs_base: ucd.is_emoji_vs_base[i],
    }
}

/// `CaseFoldingFull` for a listed code point: its F mapping, else its C mapping.
fn case_folding_full((cp, folding): &(u32, CaseFolding)) -> Option<(u32, Vec<u32>)> {
    if folding.full.is_empty() {
        folding.common.map(|common| (*cp, vec![common]))
    } else {
        Some((*cp, folding.full.clone()))
    }
}

#[cfg(test)]
mod tests;
