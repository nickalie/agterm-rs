//! Helpers for parsing metadata shared by Kitty OSC protocols.
//!
//! Kitty OSC 99 and OSC 5522 encode metadata as colon-separated `key=value` fields. The iterator in this module lazily
//! searches that metadata for one key, preserving the order of repeated values without allocating.
//!
//! Parsing is intentionally tolerant. Whitespace around keys and values is trimmed, while malformed fields,
//! non-matching keys, and invalid values are skipped. Returned values are slices of the original metadata and remain
//! valid only as long as that input remains valid.
//!
//! Ghostty: `src/terminal/osc/kitty_metadata.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/kitty_metadata.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig makes a type per comptime key and value set; here the key and the set are fields of one iterator
//! type, which is a Rust `Iterator`.

/// `std.ascii.whitespace`: space, `\t`, `\n`, `\r`, VT (0x0B), FF (0x0C). (Rust's `u8::is_ascii_whitespace` omits
/// VT, so the Zig set is spelled out.)
const ASCII_WHITESPACE: &[u8] = &[b' ', b'\t', b'\n', b'\r', 0x0B, 0x0C];

fn trim_whitespace(s: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = s.len();
    while start < end && ASCII_WHITESPACE.contains(&s[start]) {
        start += 1;
    }
    while end > start && ASCII_WHITESPACE.contains(&s[end - 1]) {
        end -= 1;
    }
    &s[start..end]
}

/// An iterator over the values whose key exactly matches `key`.
///
/// If `valid_value_characters` is non-null, every byte in a returned value must appear in that character set. Passing
/// null disables value validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValueIterator<'a> {
    key: &'static [u8],
    valid_value_characters: Option<&'static [u8]>,
    metadata: &'a [u8],
    pos: usize,
}

impl<'a> ValueIterator<'a> {
    /// Initialize an iterator borrowing `metadata`.
    pub(crate) const fn init(
        key: &'static [u8],
        valid_value_characters: Option<&'static [u8]>,
        metadata: &'a [u8],
    ) -> Self {
        Self {
            key,
            valid_value_characters,
            metadata,
            pos: 0,
        }
    }
}

impl<'a> Iterator for ValueIterator<'a> {
    type Item = &'a [u8];

    /// Return the next valid matching value, or null when none remain. The returned slice borrows the metadata passed
    /// to `init`.
    fn next(&mut self) -> Option<&'a [u8]> {
        while self.pos < self.metadata.len() {
            let end = match self.metadata[self.pos..].iter().position(|&b| b == b':') {
                Some(rel) => self.pos + rel,
                None => self.metadata.len(),
            };
            let field = &self.metadata[self.pos..end];
            self.pos = if end < self.metadata.len() { end + 1 } else { end };

            let Some(equals) = field.iter().position(|&b| b == b'=') else {
                continue;
            };
            let field_key = trim_whitespace(&field[0..equals]);
            if field_key != self.key {
                continue;
            }

            let value = trim_whitespace(&field[equals + 1..]);
            if let Some(valid) = self.valid_value_characters
                && value.iter().any(|b| !valid.contains(b))
            {
                continue;
            }

            return Some(value);
        }

        None
    }
}

#[cfg(test)]
mod tests;
