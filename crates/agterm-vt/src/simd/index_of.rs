//! The index of the first occurrence of a byte: the scan behind the stream's escape search.
//!
//! Ghostty: `src/simd/index_of.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-simd/src/index_of.rs` at `RUSTTY_REV` (MIT).
//! Changes: the scalar lane only (`std.mem.indexOfScalar`), which answers what the SIMD kernel does; `memchr` takes
//! its place if a measurement shows the scan hot.

/// The index of the first `needle` in `input`, or `None`.
pub(crate) fn index_of(input: &[u8], needle: u8) -> Option<usize> {
    input.iter().position(|&b| b == needle)
}

#[cfg(test)]
mod tests {
    use super::index_of;

    /// Ghostty's `test "indexOf"`.
    #[test]
    fn index_of_finds_the_first_occurrence() {
        assert_eq!(index_of(b"hello", b' '), None);
        assert_eq!(index_of(b"hi lo", b' '), Some(2));
        let first_line_hit = concat!(
            "XXXXX XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            "\n",
            "XXXXXXXXXXXX XXXXXXXXXXX XXXXXXXXXXXXXXX"
        );
        assert_eq!(index_of(first_line_hit.as_bytes(), b' '), Some(5));
        let second_line_hit = concat!(
            "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            "\n",
            "XXXXXXXXXXXX XXXXXXXXXXX XXXXXXXXXXXXXXX"
        );
        assert_eq!(index_of(second_line_hit.as_bytes(), b' '), Some(53));
    }

    #[test]
    fn index_of_an_empty_input_is_none() {
        assert_eq!(index_of(b"", 0x1B), None);
    }
}
