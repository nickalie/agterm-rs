//! Ghostty's `base64.zig` tests, rustty's in-place tests, and the forgiving lane held to simdutf's answers.

use super::{Base64Invalid, Padding, Streaming, decode, decode_in_place, decode_strict, max_len};

/// `(input, simdutf's max_len, simdutf's decode)`, `None` for an error.
type SimdutfCase = (&'static [u8], usize, Option<&'static [u8]>);

/// The answers of Ghostty's SIMD lane, `ghostty_simd_base64_max_length` and `ghostty_simd_base64_decode` over the
/// simdutf 9.0.0 Ghostty vendors at the pin (`pkg/simdutf`), recorded from a harness linking it. They cover whitespace of every kind (and VT, which is not whitespace here), padding with
/// whitespace around it, missing and excess padding, a third `=`, padding inside the data, one-character remainders,
/// unused tail bits, non-ASCII and NUL bytes, and the URL alphabet (not accepted). When this lane was written, the
/// same harness also checked a random corpus of 200,000 inputs (alphabet, padding, whitespace and junk bytes) against
/// it: `max_len`, `decode` and `decode_in_place` agreed on every one.
const SIMDUTF_CASES: &[SimdutfCase] = &[
    (b"", 0, Some(b"")),
    (b"=", 0, None),
    (b"==", 0, None),
    (b"===", 0, None),
    (b" ", 0, Some(b"")),
    (b"\x09\x0a\x0c\x0d ", 3, Some(b"")),
    (b"\x0b", 0, None),
    (b"Q", 0, None),
    (b"!", 0, None),
    (b"Q=", 0, None),
    (b"QQ", 1, Some(b"A")),
    (b"QQ=", 1, None),
    (b"QQ==", 1, Some(b"A")),
    (b"QQ===", 2, None),
    (b"QUI", 2, Some(b"AB")),
    (b"QUI=", 2, Some(b"AB")),
    (b"QUI==", 2, None),
    (b"QUJD", 3, Some(b"ABC")),
    (b"QUJD=", 3, None),
    (b"QUJDRA", 4, Some(b"ABCD")),
    (b"QUJDRA==", 4, Some(b"ABCD")),
    (b"QUJDRA=", 4, None),
    (b"QR==", 1, Some(b"A")),
    (b"QUJ=", 2, Some(b"AB")),
    (b"aGVsbG8=", 5, Some(b"hello")),
    (b"aGVsbG8", 5, Some(b"hello")),
    (b"aGVs bG8=", 6, Some(b"hello")),
    (b"aGVs\x0abG8=", 6, Some(b"hello")),
    (b"aGVs\x0d\x0abG8=", 6, Some(b"hello")),
    (b"aGVs\x09bG8=", 6, Some(b"hello")),
    (b"aGVs\x0cbG8=", 6, Some(b"hello")),
    (b"aGVs\x0bbG8=", 6, None),
    (b"aGVsbG8 =", 6, Some(b"hello")),
    (b"aGVsbG8= ", 6, Some(b"hello")),
    (b"aGVsbG8=\x0a\x0a", 7, Some(b"hello")),
    (b" aGVsbG8=", 6, Some(b"hello")),
    (b"QQ= =", 3, Some(b"A")),
    (b"QQ =\x0a= ", 5, Some(b"A")),
    (b"Q Q = =", 4, Some(b"A")),
    (b"QQ==QQ==", 4, None),
    (b"Q=Q=", 2, None),
    (b"=QQQ", 3, None),
    (b"QQ=A", 3, None),
    (b"aGVsbG8\xc3\xa9", 6, None),
    (b"aGVs\x00bG8=", 6, None),
    (b"QUJD\x0a", 3, Some(b"ABC")),
    (b"QUJDR\x0a", 4, None),
    (b"QUJDRA\x0a=\x0a=\x0a", 8, Some(b"ABCD")),
    (b"////", 3, Some(b"\xff\xff\xff")),
    (b"++++", 3, Some(b"\xfb\xef\xbe")),
    (b"-_-_", 3, None),
    (
        b"SGVsbG8gd29ybGQsIHRoaXMgaXMgYSBsb25nZXIgcGF5bG9hZCBmb3IgdGhlIFNJTUQgbGFuZQ==",
        55,
        Some(b"Hello world, this is a longer payload for the SIMD lane"),
    ),
    (
        b"SGVsbG8gd29ybGQsIHRoaXMgaXMgYSBs\x0ab25nZXIgcGF5bG9hZCBmb3IgdGhlIFNJTUQgbGFuZQ==\x0a",
        58,
        Some(b"Hello world, this is a longer payload for the SIMD lane"),
    ),
];

/// `max_len`, `decode` and `decode_in_place` answer every recorded case as simdutf does.
#[test]
fn the_forgiving_lane_answers_as_simdutf() {
    for &(input, expected_max, expected) in SIMDUTF_CASES {
        let shown = String::from_utf8_lossy(input);
        assert_eq!(max_len(input), expected_max, "max_len of {shown:?}");
        let mut output = vec![0; expected_max];
        assert_eq!(decode(input, &mut output).ok(), expected, "decode of {shown:?}");
        let mut buf = input.to_vec();
        let in_place = decode_in_place(&mut buf, expected_max).map(|n| buf[..n].to_vec());
        assert_eq!(in_place.ok().as_deref(), expected, "decode_in_place of {shown:?}");
    }
}

/// rustty's in-place check: decoding a buffer on top of itself gives the same bytes and the same failures as into a
/// separate one. Lengths 0..200 cross the 16-byte and 4-byte steps and the tail; each input is also tried padded,
/// with whitespace, and with an invalid character.
#[test]
fn decode_in_place_matches_decode() {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut seed: u32 = 0x1234_5678;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        usize::try_from(seed).unwrap()
    };
    for len in 0..200 {
        let base: Vec<u8> = (0..len).map(|_| ALPHABET[next() % 64]).collect();
        let mut padded = base.clone();
        padded.extend_from_slice(b"==");
        let mut spaced = base.clone();
        let mut invalid = base.clone();
        if len > 0 {
            spaced.insert(next() % len, b'\n');
            invalid[next() % len] = b'!';
        }
        for case in [&base, &padded, &spaced, &invalid] {
            let max = max_len(case);
            let mut out = vec![0; max];
            let expected = decode(case, &mut out).map(<[u8]>::to_vec);
            let mut buf = case.clone();
            let got = decode_in_place(&mut buf, max).map(|n| buf[..n].to_vec());
            assert_eq!(expected, got, "len {len}: {:?}", String::from_utf8_lossy(case));
        }
    }
}

#[test]
fn decode_in_place_matches_decode_on_ghosttys_cases() {
    let cases: [&[u8]; 6] = [
        b"",
        b"aGVsbG8=",
        b"aGVsbG8",
        b"////",
        b"SGVsbG8gd29ybGQsIHRoaXMgaXMgYSBsb25nZXIgcGF5bG9hZCBmb3IgdGhlIFNJTUQgbGFuZQ==",
        b"!!!!",
    ];
    for case in cases {
        let max = max_len(case);
        let mut out = vec![0; max];
        let expected = decode(case, &mut out).map(<[u8]>::to_vec);
        let mut buf = case.to_vec();
        let got = decode_in_place(&mut buf, max).map(|n| buf[..n].to_vec());
        assert_eq!(expected, got, "case {:?}", String::from_utf8_lossy(case));
    }
}

/// Ghostty's `test "base64 maxLen"`.
#[test]
fn base64_max_len() {
    assert_eq!(max_len(b"aGVsbG8gd29ybGQ="), 11);
}

/// Ghostty's `test "base64 empty input"`.
#[test]
fn base64_empty_input() {
    let mut output: [u8; 0] = [];
    assert_eq!(max_len(b""), 0);
    assert_eq!(decode(b"", &mut output), Ok(&b""[..]));
}

/// Ghostty's `test "base64 decode"`.
#[test]
fn base64_decode() {
    let input = b"aGVsbG8gd29ybGQ=";
    let mut output = vec![0; max_len(input)];
    assert_eq!(decode(input, &mut output), Ok(&b"hello world"[..]));
}

/// Ghostty's `test "base64 strict decode valid"`.
#[test]
fn base64_strict_decode_valid() {
    let mut output = [0; 128];
    let cases: [(&[u8], &[u8]); 5] = [
        (b"", b""),
        (b"aGVsbG8gd29ybGQ=", b"hello world"),
        (b"bGlnaHQgdw==", b"light w"),
        (b"Zm9vYmFy", b"foobar"),
        // Every alphabet character in one input.
        (
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/",
            b"\x00\x10\x83\x10\x51\x87\x20\x92\x8b\x30\xd3\x8f\x41\x14\x93\x51\x55\x97\x61\x96\x9b\x71\xd7\x9f\x82\x18\
              \xa3\x92\x59\xa7\xa2\x9a\xab\xb2\xdb\xaf\xc3\x1c\xb3\xd3\x5d\xb7\xe3\x9e\xbb\xf3\xdf\xbf",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(decode_strict(input, &mut output, Padding::Required), Ok(expected));
        assert_eq!(decode_strict(input, &mut output, Padding::Optional), Ok(expected));
    }
}

/// Ghostty's `test "base64 strict decode invalid"`: kitty's own strict decoding cases plus padding placement, all
/// invalid under both requirements; then unpadded input, tolerated only when padding is optional.
#[test]
fn base64_strict_decode_invalid() {
    let mut output = [0; 128];
    let cases: [&[u8]; 11] = [
        b"bGlnaHQgdw=",    // one padding byte missing
        b"bGln!!Qgdw==",   // invalid characters
        b"bGlnaHQgdw==\n", // trailing whitespace
        b"\nbGlnaHQgdw==", // leading whitespace
        b"bGlnaHQg dw==",  // interior whitespace
        b"!!!!",
        b"=",
        b"==",
        b"A===",
        b"AB=C", // padding must be the suffix
        b"Zm9v YmFy",
    ];
    for case in cases {
        assert_eq!(
            decode_strict(case, &mut output, Padding::Required),
            Err(Base64Invalid),
            "{case:?}"
        );
        assert_eq!(
            decode_strict(case, &mut output, Padding::Optional),
            Err(Base64Invalid),
            "{case:?}"
        );
    }

    assert_eq!(
        decode_strict(b"bGlnaHQgdw", &mut output, Padding::Required),
        Err(Base64Invalid)
    );
    assert_eq!(
        decode_strict(b"bGlnaHQgdw", &mut output, Padding::Optional),
        Ok(&b"light w"[..])
    );
    assert_eq!(
        decode_strict(b"bGl", &mut output, Padding::Required),
        Err(Base64Invalid)
    );
    assert_eq!(decode_strict(b"bGl", &mut output, Padding::Optional), Ok(&b"li"[..]));
    assert_eq!(
        decode_strict(b"bGlna", &mut output, Padding::Optional),
        Err(Base64Invalid)
    );
}

/// Ghostty's `test "base64 streaming decode chunk boundaries"`: a stream split at every boundary, and fed one byte
/// at a time, decodes as the single-shot decode does.
#[test]
fn base64_streaming_decode_chunk_boundaries() {
    let input = b"c29tZSBsb25nZXIgZGF0YSB3aXRoIHBhZGRpbmc+Pz8=";
    let expected = b"some longer data with padding>??";
    for split in 0..=input.len() {
        let mut s = Streaming::default();
        let mut output = [0; 64];
        let mut result = s.feed(&input[..split], &mut output).unwrap().to_vec();
        result.extend_from_slice(s.feed(&input[split..], &mut output).unwrap());
        s.finish().unwrap();
        assert_eq!(&result[..], expected, "split at {split}");
    }

    let mut s = Streaming::default();
    let mut output = [0; 4];
    let mut result = Vec::new();
    for byte in input.chunks(1) {
        result.extend_from_slice(s.feed(byte, &mut output).unwrap());
    }
    s.finish().unwrap();
    assert_eq!(&result[..], expected);
}

/// Ghostty's `test "base64 streaming decode invalid"`.
#[test]
fn base64_streaming_decode_invalid() {
    let mut output = [0; 64];

    // Invalid characters are rejected wherever they appear.
    for input in [&b"!!!!"[..], b"SGVs!!!bG8=", b"\nc29tZSBkYXRh"] {
        assert_eq!(
            Streaming::default().feed(input, &mut output),
            Err(Base64Invalid),
            "{input:?}"
        );
    }

    // Data after padding in the same feed is rejected, even when the padded group completes only in that feed.
    assert_eq!(
        Streaming::default().feed(b"Z29vZA==SGVsbG8=", &mut output),
        Err(Base64Invalid)
    );
    let mut s = Streaming::default();
    s.feed(b"Z29vZA=", &mut output).unwrap();
    assert_eq!(s.feed(b"=SGVs", &mut output), Err(Base64Invalid));

    // A feed that ends exactly at its padding resets the stream, so clients padding every chunk keep working.
    let mut s = Streaming::default();
    assert_eq!(s.feed(b"Z29vZA==", &mut output), Ok(&b"good"[..]));
    assert_eq!(s.feed(b"SGVsbG8=", &mut output), Ok(&b"Hello"[..]));
    s.finish().unwrap();

    // Padding split across feeds resets too.
    let mut s = Streaming::default();
    assert_eq!(s.feed(b"Z29vZA=", &mut output), Ok(&b"goo"[..]));
    assert_eq!(s.feed(b"=", &mut output), Ok(&b"d"[..]));
    assert_eq!(s.feed(b"bW9yZQ==", &mut output), Ok(&b"more"[..]));
    s.finish().unwrap();
}

/// Ghostty's `test "base64 streaming decode invalid"`, continued: padding out of place, and a stream that ends in a
/// partial group.
#[test]
fn base64_streaming_decode_invalid_padding() {
    let mut output = [0; 64];

    // Misplaced padding within a group.
    assert_eq!(Streaming::default().feed(b"YQ=X", &mut output), Err(Base64Invalid));
    assert_eq!(Streaming::default().feed(b"=AAA", &mut output), Err(Base64Invalid));

    // A stream that ends in a partial group is missing its padding; the failed finish resets the decoder.
    let mut s = Streaming::default();
    assert_eq!(s.feed(b"SGVsbG8", &mut output), Ok(&b"Hel"[..]));
    assert_eq!(s.finish(), Err(Base64Invalid));
    assert_eq!(s.feed(b"SGVsbG8", &mut output), Ok(&b"Hel"[..]));
    assert_eq!(s.feed(b"=", &mut output), Ok(&b"lo"[..]));
    s.finish().unwrap();
}

/// An empty feed decodes nothing and keeps the carried group.
#[test]
fn an_empty_feed_keeps_the_carry() {
    let mut output = [0; 8];
    let mut s = Streaming::default();
    assert_eq!(s.feed(b"SG", &mut output), Ok(&b""[..]));
    assert_eq!(s.feed(b"", &mut output), Ok(&b""[..]));
    assert_eq!(s.max_len(b"Vs"), 3);
    assert_eq!(s.feed(b"Vs", &mut output), Ok(&b"Hel"[..]));
    s.finish().unwrap();
}
