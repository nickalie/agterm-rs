//! Specialized encodings used in some OSC protocols.
//!
//! Ghostty: `src/terminal/osc/encoding.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/encoding.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's `Utf8View` validation is `str::from_utf8`, which refuses the same inputs (surrogates and overlong
//! forms included).

/// Kitty defines "Escape code safe UTF-8" as valid UTF-8 with the additional requirement of not containing any C0
/// escape codes (0x00-0x1f), DEL (0x7f) and C1 escape codes (0x80-0x9f).
///
/// Used by OSC 66 (text sizing) and OSC 99 (Kitty notifications).
///
/// See: <https://sw.kovidgoyal.net/kitty/desktop-notifications/#safe-utf8>
pub(crate) fn is_safe_utf8(s: &[u8]) -> bool {
    std::str::from_utf8(s).is_ok_and(|utf8| {
        utf8.chars()
            .all(|cp| !matches!(u32::from(cp), 0x00..=0x1f | 0x7f | 0x80..=0x9f))
    })
}

#[cfg(test)]
mod tests;
