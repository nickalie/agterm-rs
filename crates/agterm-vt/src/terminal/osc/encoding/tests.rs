use super::is_safe_utf8;

#[test]
fn safe_utf8() {
    assert!(is_safe_utf8(b"Hello world!"));
    assert!(is_safe_utf8("安全的ユニコード☀️".as_bytes()));
    assert!(!is_safe_utf8(b"No linebreaks\nallowed"));
    assert!(!is_safe_utf8(b"\x07no bells"));
    assert!(!is_safe_utf8(b"\x1b]9;no OSCs\x1b\\\x1b[m"));
    assert!(!is_safe_utf8(b"\x9f8-bit escapes are clever, but no"));
    // The C1 range as code points, encoded in UTF-8.
    assert!(!is_safe_utf8("\u{85}".as_bytes()));
    assert!(!is_safe_utf8(b"\xed\xa0\x80"), "a surrogate is not UTF-8");
}
