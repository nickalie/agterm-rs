//! The scalar decoder's own checks, and Zig's `std/base64.zig` decoder tests for the standard decoders.

use super::{
    Base64Decoder, Error, SCALAR_DECODER, STANDARD_ALPHABET, STANDARD_DECODER, STANDARD_NO_PAD_DECODER, char_value,
    is_alphabet_char,
};

#[test]
fn the_alphabet_maps_to_its_indices_and_nothing_else_is_in_it() {
    for (i, &c) in STANDARD_ALPHABET.iter().enumerate() {
        assert_eq!(char_value(c), Some(u8::try_from(i).unwrap()));
        assert!(is_alphabet_char(c));
    }
    for c in [b'=', b' ', b'\n', b'-', b'_', 0x00, 0x80, 0xff] {
        assert_eq!(char_value(c), None, "{c:#x}");
        assert!(!is_alphabet_char(c));
    }
}

#[test]
fn calc_size_refuses_a_single_leftover_character() {
    let size = |n| SCALAR_DECODER.calc_size_upper_bound(n);
    assert_eq!(size(0), Ok(0));
    assert_eq!(size(2), Ok(1));
    assert_eq!(size(3), Ok(2));
    assert_eq!(size(4), Ok(3));
    assert_eq!(size(5), Err(Error::InvalidPadding));
    assert_eq!(size(16), Ok(12));
}

/// Every length crosses the 16-character and 4-character steps and the tail the same way into a separate buffer as
/// on top of itself, and an invalid character fails both wherever it sits.
#[test]
fn decode_in_place_matches_decode_into_a_separate_buffer() {
    let mut seed: u32 = 0x1234_5678;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for len in (0..200).filter(|len| len % 4 != 1) {
        let mut source: Vec<u8> = (0..len)
            .map(|_| STANDARD_ALPHABET[usize::try_from(next() % 64).unwrap()])
            .collect();
        let size = SCALAR_DECODER.calc_size_upper_bound(len).unwrap();
        let mut dest = vec![0; size];
        SCALAR_DECODER.decode(&mut dest, &source).unwrap();
        let mut buf = source.clone();
        SCALAR_DECODER.decode_in_place(&mut buf, size, len).unwrap();
        assert_eq!(&buf[..size], &dest[..], "len {len}");

        if len > 0 {
            let at = usize::try_from(next()).unwrap() % len;
            source[at] = b'!';
            assert_eq!(SCALAR_DECODER.decode(&mut dest, &source), Err(Error::InvalidCharacter));
            assert_eq!(
                SCALAR_DECODER.decode_in_place(&mut source, size, len),
                Err(Error::InvalidCharacter)
            );
        }
    }
}

/// A destination longer than the decoded size changes nothing in the decoded bytes; it only lets more go through the
/// fast steps.
#[test]
fn a_longer_destination_decodes_the_same_bytes() {
    let source = b"SGVsbG8gd29ybGQsIHRoaXMgaXMgYSBsb25nZXIgcGF5bG9hZA";
    let size = SCALAR_DECODER.calc_size_upper_bound(source.len()).unwrap();
    let mut exact = vec![0; size];
    SCALAR_DECODER.decode(&mut exact, source).unwrap();
    let mut long = vec![0; source.len()];
    SCALAR_DECODER.decode(&mut long, source).unwrap();
    assert_eq!(&long[..size], &exact[..]);
    assert_eq!(&exact[..], b"Hello world, this is a longer payload");
}

/// The scalar decoder drops the unused bits of a partial group, where Zig's checks them.
#[test]
fn only_the_standard_decoders_check_the_unused_bits() {
    let mut dest = [0; 4];
    assert_eq!(SCALAR_DECODER.decode(&mut dest[..1], b"QR"), Ok(()));
    assert_eq!(dest[0], b'A');
    assert_eq!(
        STANDARD_NO_PAD_DECODER.decode(&mut dest[..1], b"QR"),
        Err(Error::InvalidPadding)
    );
    assert_eq!(STANDARD_NO_PAD_DECODER.decode(&mut dest[..1], b"QQ"), Ok(()));
    assert_eq!(
        STANDARD_DECODER.decode(&mut dest[..1], b"QR=="),
        Err(Error::InvalidPadding)
    );
    assert_eq!(STANDARD_DECODER.decode(&mut dest[..1], b"QQ=="), Ok(()));
}

/// Decode `encoded` with `decoder` the way Zig's `testAllApis` does: into a buffer of `calcSizeForSlice`.
fn decode_all(decoder: &Base64Decoder, encoded: &[u8]) -> Result<Vec<u8>, Error> {
    let mut buffer = vec![0; decoder.calc_size_for_slice(encoded)?];
    decoder.decode(&mut buffer, encoded)?;
    Ok(buffer)
}

/// The decoder half of Zig's `testAllApis` cases, padded and unpadded (Zig tests the unpadded ones with the URL-safe
/// alphabet; these inputs use none of the characters the two alphabets differ in).
#[test]
fn the_standard_decoders_decode_zigs_cases() {
    let cases: [(&[u8], &[u8]); 12] = [
        (b"", b""),
        (b"f", b"Zg=="),
        (b"fo", b"Zm8="),
        (b"foo", b"Zm9v"),
        (b"foob", b"Zm9vYg=="),
        (b"fooba", b"Zm9vYmE="),
        (b"foobar", b"Zm9vYmFy"),
        (b"foobarfoobarfoo", b"Zm9vYmFyZm9vYmFyZm9v"),
        (b"foobarfoobarfoob", b"Zm9vYmFyZm9vYmFyZm9vYg=="),
        (b"foobarfoobarfooba", b"Zm9vYmFyZm9vYmFyZm9vYmE="),
        (b"foobarfoobarfoobar", b"Zm9vYmFyZm9vYmFyZm9vYmFy"),
        (b"comptime", b"Y29tcHRpbWU="),
    ];
    for (decoded, encoded) in cases {
        assert_eq!(
            decode_all(&STANDARD_DECODER, encoded).as_deref(),
            Ok(decoded),
            "{encoded:?}"
        );
        let unpadded = encoded
            .strip_suffix(b"==")
            .or(encoded.strip_suffix(b"="))
            .unwrap_or(encoded);
        assert_eq!(
            decode_all(&STANDARD_NO_PAD_DECODER, unpadded).as_deref(),
            Ok(decoded),
            "{unpadded:?}"
        );
    }
}

/// The decoder half of Zig's `testError` cases.
#[test]
fn the_standard_decoders_refuse_zigs_error_cases() {
    use Error::{InvalidCharacter, InvalidPadding};
    let padded: [(&[u8], Error); 19] = [
        (b"A", InvalidPadding),
        (b"AA", InvalidPadding),
        (b"AAA", InvalidPadding),
        (b"A..A", InvalidCharacter),
        (b"AA=A", InvalidPadding),
        (b"AA/=", InvalidPadding),
        (b"A/==", InvalidPadding),
        (b"A===", InvalidPadding),
        (b"====", InvalidPadding),
        (b"Zm9vYmFyZm9vYmFyA..A", InvalidCharacter),
        (b"Zm9vYmFyZm9vYmFyAA=A", InvalidPadding),
        (b"Zm9vYmFyZm9vYmFyAA/=", InvalidPadding),
        (b"Zm9vYmFyZm9vYmFyA/==", InvalidPadding),
        (b"Zm9vYmFyZm9vYmFyA===", InvalidPadding),
        (b"A..AZm9vYmFyZm9vYmFy", InvalidCharacter),
        (b"Zm9vYmFyZm9vAA=A", InvalidPadding),
        (b"Zm9vYmFyZm9vAA/=", InvalidPadding),
        (b"Zm9vYmFyZm9vA/==", InvalidPadding),
        (b"Zm9vYmFyZm9vA===", InvalidPadding),
    ];
    for (encoded, error) in padded {
        assert_eq!(decode_all(&STANDARD_DECODER, encoded), Err(error), "{encoded:?}");
    }
    let unpadded: [(&[u8], Error); 10] = [
        (b"A", InvalidPadding),
        (b"AAA=", InvalidCharacter),
        (b"A..A", InvalidCharacter),
        (b"AA=A", InvalidCharacter),
        (b"AA/=", InvalidCharacter),
        (b"A/==", InvalidCharacter),
        (b"A===", InvalidCharacter),
        (b"====", InvalidCharacter),
        (b"Zm9vYmFyZm9vYmFyA..A", InvalidCharacter),
        (b"A..AZm9vYmFyZm9vYmFy", InvalidCharacter),
    ];
    for (encoded, error) in unpadded {
        assert_eq!(decode_all(&STANDARD_NO_PAD_DECODER, encoded), Err(error), "{encoded:?}");
    }
}

/// `calcSizeForSlice` takes one byte off per trailing padding character, of the last two; a 0xFF byte where padding
/// should continue is reported as an invalid character, as Zig's check compares it with its invalid marker.
#[test]
fn calc_size_for_slice_and_the_padding_check_follow_zig() {
    assert_eq!(STANDARD_DECODER.calc_size_for_slice(b"Zg=="), Ok(1));
    assert_eq!(STANDARD_DECODER.calc_size_for_slice(b"Zm8="), Ok(2));
    assert_eq!(STANDARD_DECODER.calc_size_for_slice(b"Zm9v"), Ok(3));
    assert_eq!(STANDARD_DECODER.calc_size_for_slice(b"Zm9"), Err(Error::InvalidPadding));
    assert_eq!(STANDARD_NO_PAD_DECODER.calc_size_for_slice(b"Zm8"), Ok(2));
    assert_eq!(STANDARD_NO_PAD_DECODER.calc_size_for_slice(b"Zm8="), Ok(3));

    let mut dest = [0; 3];
    assert_eq!(
        STANDARD_DECODER.decode(&mut dest, b"QQ=\xff"),
        Err(Error::InvalidCharacter)
    );
    assert_eq!(STANDARD_DECODER.decode(&mut dest, b"QQ=!"), Err(Error::InvalidPadding));
}
