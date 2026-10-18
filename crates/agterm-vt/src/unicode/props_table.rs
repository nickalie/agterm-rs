//! The per-code-point [`Properties`] lookup table.
//!
//! Ghostty: `src/unicode/props_table.zig` at `LIBGHOSTTY_VT_REV`; the spot checks follow the property rules of
//! `src/unicode/props_uucode.zig` and `src/build/uucode_config.zig`.
//! rustty: `crates/rustty-unicode/src/props_table.rs` and the tests of `props_uucode.rs` at `RUSTTY_REV` (MIT).
//! Changes: the table is the checked-in generated stages instead of a build-system module. `props_uucode.zig`, the
//! generator that writes them from uucode, is the `gen-unicode` binary's (`src/bin/gen-unicode`); its out-of-range
//! rule (above U+10FFFF: width 1, zero width in a grapheme, `Other`, no emoji VS base) is what the table holds for
//! those code points, and its spot checks against the Unicode standard run on the table here.

use crate::unicode::generated::props_data;
use crate::unicode::lut::TablesRef;
use crate::unicode::props::Properties;

/// The lookup tables for Ghostty's code point properties.
#[inline]
pub(crate) const fn table()
-> TablesRef<Properties, { props_data::STAGE1.len() }, { props_data::STAGE2.len() }, { props_data::STAGE3.len() }> {
    TablesRef {
        stage1: &props_data::STAGE1,
        stage2: &props_data::STAGE2,
        stage3: &props_data::STAGE3,
    }
}

#[cfg(test)]
mod tests;
