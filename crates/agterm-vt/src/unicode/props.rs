//! The property set per code point that Ghostty cares about.
//!
//! Adding to this lets you find new properties but also potentially makes our lookup tables less efficient.
//!
//! Ghostty: `src/unicode/props.zig` at `LIBGHOSTTY_VT_REV`, with uucode's `types.GraphemeBreakNoControl`.
//! rustty: `crates/rustty-unicode/src/props.rs` at `RUSTTY_REV` (MIT).
//! Changes: `Properties` is a plain struct, not a `packed struct(u16)`, so it needs no padding field; the generated
//! tables hold its values. `eql` and `format`, which only the table generator uses, come with the generator (the
//! zig-free-core plan's Task 6); derived equality stands in for `eql` meanwhile.

/// The grapheme break property of a code point that is not a control: uucode's `GraphemeBreakNoControl`
/// (`enum(u5)`). The discriminants are uucode's, because the generated tables and the grapheme precompute key are
/// built from them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub(crate) enum GraphemeBreakNoControl {
    #[default]
    Other = 0,
    Prepend = 1,
    RegionalIndicator = 2,
    SpacingMark = 3,
    L = 4,
    V = 5,
    T = 6,
    Lv = 7,
    Lvt = 8,
    Zwj = 9,
    Zwnj = 10,
    ExtendedPictographic = 11,
    EmojiModifierBase = 12,
    EmojiModifier = 13,
    // `extend` is `zwnj` + `indic_conjunct_break_extend` + `indic_conjunct_break_linker_extend`.
    IndicConjunctBreakExtend = 14,
    IndicConjunctBreakLinkerExtend = 15,
    /// `InCB=Linker` with `Grapheme_Cluster_Break=Other` (U+1CF5, U+1CF6, U+11A3A as of Unicode 18): unlike
    /// `IndicConjunctBreakLinkerExtend` it is not part of `extend`.
    IndicConjunctBreakLinkerOther = 16,
    IndicConjunctBreakConsonant = 17,
}

/// The properties of one code point.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Properties {
    /// Code point width, clamped to [0, 2]: Ghostty handles control characters itself, and a wide character is at
    /// most 2 cells (a 3-em dash becomes a 2-em dash).
    pub(crate) width: u8,

    /// Whether the code point does not contribute to the width of a grapheme cluster (not used for single code
    /// point cells).
    pub(crate) width_zero_in_grapheme: bool,

    /// The grapheme break property.
    pub(crate) grapheme_break: GraphemeBreakNoControl,

    /// Whether the code point is the base of an emoji variation sequence (VS15/VS16 change its width).
    pub(crate) emoji_vs_base: bool,
}
