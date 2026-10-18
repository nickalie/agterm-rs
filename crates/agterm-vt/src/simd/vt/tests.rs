//! Ghostty's `vt.zig` tests and rustty's additions, plus a differential test against the standard library's lossy
//! UTF-8 decode, which follows the same maximal-subpart substitution.

use super::{DecodeResult, utf8_decode_until_control_seq as decode};

#[test]
fn decode_tight_buffers_stay_in_bounds() {
    for s in [
        &b"A"[..],
        &b"hello\xf0\x90"[..],
        &b"hello\xc2"[..],
        &b"\x1b[48;2;10;20;30mA"[..],
        &b"world"[..],
        &b"\xff"[..],
        &b"\xe2\x28"[..],
    ] {
        let mut output = vec![0_u32; s.len()];
        let res = decode(s, &mut output);
        assert!(res.consumed <= s.len() && res.decoded <= output.len());
    }
}

/// The ASCII run writes whole chunks only when the whole chunk is ASCII: with the output exactly as long as the
/// input, every length and stop position across the vector widths stays in bounds.
#[test]
fn decode_ascii_runs_with_an_output_of_exactly_the_input_length() {
    for len in 0..=257_usize {
        for stop_at in 0..=len {
            for stop_byte in [0x1B_u8, 0xC3] {
                let mut input = vec![b'a'; len];
                if stop_at < len {
                    input[stop_at] = stop_byte;
                }
                let mut output = vec![0_u32; input.len()];
                let res = decode(&input, &mut output);
                assert!(res.consumed <= input.len());
                assert!(res.decoded <= output.len());
                assert!(
                    output[..res.decoded.min(stop_at)]
                        .iter()
                        .all(|&cp| cp == u32::from(b'a'))
                );
            }
        }
    }
}

#[test]
fn decode_no_escape() {
    let mut output = [0_u32; 1024];
    let s = "hello".repeat(128);
    assert_eq!(
        DecodeResult {
            consumed: s.len(),
            decoded: s.len()
        },
        decode(s.as_bytes(), &mut output)
    );
}

#[test]
fn decode_ascii_to_escape() {
    let mut output = [0_u32; 1024];
    let prefix = "hello".repeat(64);
    let s = format!("{prefix}\x1b{}", "world".repeat(64));
    assert_eq!(
        DecodeResult {
            consumed: prefix.len(),
            decoded: prefix.len()
        },
        decode(s.as_bytes(), &mut output)
    );
}

#[test]
fn decode_immediate_esc_sequence() {
    let mut output = [0_u32; 64];
    assert_eq!(
        DecodeResult {
            consumed: 0,
            decoded: 0
        },
        decode(b"\x1b[?5s", &mut output)
    );
}

#[test]
fn decode_incomplete_utf_8() {
    let mut output = [0_u32; 64];

    // A 2-byte sequence cut off by the end of the input stays pending.
    assert_eq!(
        DecodeResult {
            consumed: 5,
            decoded: 5
        },
        decode(b"hello\xc2", &mut output)
    );

    // E0 wants A0-BF next; 00 is not, so E0 is a maximal subpart of one byte, then NUL.
    let res = decode(b"hello\xe0\x00", &mut output);
    assert_eq!(
        DecodeResult {
            consumed: 7,
            decoded: 7
        },
        res
    );
    assert_eq!(&output[5..7], &[0xFFFD, 0x00]);

    // A 4-byte sequence cut off by the end of the input (F0 90 is valid so far).
    assert_eq!(
        DecodeResult {
            consumed: 5,
            decoded: 5
        },
        decode(b"hello\xf0\x90", &mut output)
    );
}

#[test]
fn decode_invalid_utf_8() {
    let mut output = [0_u32; 64];
    // An ill-formed 2-byte sequence: only the lead is replaced.
    assert_eq!(
        DecodeResult {
            consumed: 7,
            decoded: 7
        },
        decode(b"hello\xc2\x01", &mut output)
    );
    assert_eq!(&output[5..7], &[0xFFFD, 0x01]);
}

/// Bytes F5-FF are each replaced, per the maximal subpart rule.
#[test]
fn decode_invalid_leading_byte_is_replaced() {
    let mut output = [0_u32; 64];
    assert_eq!(
        DecodeResult {
            consumed: 6,
            decoded: 6
        },
        decode(b"hello\xFF", &mut output)
    );
    assert_eq!(output[5], 0xFFFD);
}

#[test]
fn decode_invalid_continuation_in_3_byte_sequence() {
    let mut output = [0_u32; 64];
    // E2 wants two continuation bytes and 28 is not one: "hello", U+FFFD, "(", "world".
    let res = decode(b"hello\xe2\x28world", &mut output);
    assert_eq!(res.decoded, 12);
    assert_eq!(&output[5..8], &[0xFFFD, u32::from('('), u32::from('w')]);
}

#[test]
fn decode_invalid_continuation_in_4_byte_sequence() {
    let mut output = [0_u32; 64];
    // F0 90 is a valid prefix and 28 breaks it: the prefix is one maximal subpart.
    let res = decode(b"hello\xf0\x90\x28world", &mut output);
    assert_eq!(res.decoded, 12);
    assert_eq!(&output[5..8], &[0xFFFD, u32::from('('), u32::from('w')]);
}

#[test]
fn decode_multiple_consecutive_invalid_bytes() {
    let mut output = [0_u32; 64];
    let (a, b) = (u32::from('a'), u32::from('b'));
    // Each lone continuation byte is its own maximal subpart.
    assert_eq!(decode(b"a\x80\x80b", &mut output).decoded, 4);
    assert_eq!(&output[..4], &[a, 0xFFFD, 0xFFFD, b]);
    // C0 is an invalid lead (below C2): each byte is replaced.
    assert_eq!(decode(b"a\xc0\xc0b", &mut output).decoded, 4);
    assert_eq!(&output[..4], &[a, 0xFFFD, 0xFFFD, b]);
}

#[test]
fn decode_unexpected_continuation_byte_as_lead() {
    let mut output = [0_u32; 64];
    assert_eq!(decode(b"a\x80b", &mut output).decoded, 3);
    assert_eq!(&output[..3], &[u32::from('a'), 0xFFFD, u32::from('b')]);
}

#[test]
fn decode_overlong_2_byte_encoding() {
    let mut output = [0_u32; 64];
    // Table 3-8: C0 AF is FFFD FFFD.
    assert_eq!(decode(b"a\xc0\xafb", &mut output).decoded, 4);
    assert_eq!(&output[..4], &[u32::from('a'), 0xFFFD, 0xFFFD, u32::from('b')]);
}

#[test]
fn decode_surrogate_half() {
    let mut output = [0_u32; 64];
    // ED A0 80 encodes U+D800; after ED the next byte must be 80-9F (Table 3-9): FFFD FFFD FFFD.
    assert_eq!(decode(b"a\xed\xa0\x80b", &mut output).decoded, 5);
    assert_eq!(&output[..5], &[u32::from('a'), 0xFFFD, 0xFFFD, 0xFFFD, u32::from('b')]);
}

#[test]
fn decode_valid_multibyte_surrounded_by_invalid() {
    let mut output = [0_u32; 64];
    assert_eq!(decode(b"\x80\xc3\xa9\x80", &mut output).decoded, 3);
    assert_eq!(&output[..3], &[0xFFFD, 0x00E9, 0xFFFD]);
}

/// A sequence that is valid so far but cut off by an ESC never completes, so it is consumed as one U+FFFD; only the
/// end of the input leaves one pending.
#[test]
fn decode_partial_utf8_before_escape() {
    let mut output = [0_u32; 64];

    let res = decode(b"hi\xc2\x1b[0m", &mut output);
    assert_eq!(
        DecodeResult {
            consumed: 3,
            decoded: 3
        },
        res
    );
    assert_eq!(output[2], 0xFFFD);

    let res = decode(b"\xe0\xa0\x1bX", &mut output);
    assert_eq!(
        DecodeResult {
            consumed: 2,
            decoded: 1
        },
        res
    );
    assert_eq!(output[0], 0xFFFD);

    assert_eq!(
        DecodeResult {
            consumed: 2,
            decoded: 2
        },
        decode(b"hi\xc2", &mut output)
    );
}

#[test]
fn decode_invalid_byte_before_escape() {
    let mut output = [0_u32; 64];
    assert_eq!(
        DecodeResult {
            consumed: 3,
            decoded: 3
        },
        decode(b"hi\x80\x1b[0m", &mut output)
    );
    assert_eq!(&output[..3], &[u32::from('h'), u32::from('i'), 0xFFFD]);
}

/// Unicode Table 3-8, U+FFFD for non-shortest form sequences: C0 AF E0 80 BF F0 81 82 41.
#[test]
fn table_3_8_non_shortest_form_sequences() {
    let mut output = [0_u32; 64];
    assert_eq!(
        DecodeResult {
            consumed: 9,
            decoded: 9
        },
        decode(b"\xC0\xAF\xE0\x80\xBF\xF0\x81\x82\x41", &mut output)
    );
    assert!(output[..8].iter().all(|&cp| cp == 0xFFFD));
    assert_eq!(output[8], 0x41);
}

/// Unicode Table 3-9, U+FFFD for ill-formed sequences for surrogates: ED A0 80 ED BF BF ED AF 41.
#[test]
fn table_3_9_surrogate_sequences() {
    let mut output = [0_u32; 64];
    assert_eq!(
        DecodeResult {
            consumed: 9,
            decoded: 9
        },
        decode(b"\xED\xA0\x80\xED\xBF\xBF\xED\xAF\x41", &mut output)
    );
    assert!(output[..8].iter().all(|&cp| cp == 0xFFFD));
    assert_eq!(output[8], 0x41);
}

/// Unicode Table 3-10, U+FFFD for other ill-formed sequences: F4 91 92 93 FF 41 80 BF 42.
#[test]
fn table_3_10_other_ill_formed_sequences() {
    let mut output = [0_u32; 64];
    assert_eq!(
        DecodeResult {
            consumed: 9,
            decoded: 9
        },
        decode(b"\xF4\x91\x92\x93\xFF\x41\x80\xBF\x42", &mut output)
    );
    assert_eq!(
        &output[..9],
        &[0xFFFD, 0xFFFD, 0xFFFD, 0xFFFD, 0xFFFD, 0x41, 0xFFFD, 0xFFFD, 0x42]
    );
}

/// Unicode Table 3-11, U+FFFD for truncated sequences: E1 80 | E2 | F0 91 92 | F1 BF | 41.
#[test]
fn table_3_11_truncated_sequences() {
    let mut output = [0_u32; 64];
    assert_eq!(
        DecodeResult {
            consumed: 9,
            decoded: 5
        },
        decode(b"\xE1\x80\xE2\xF0\x91\x92\xF1\xBF\x41", &mut output)
    );
    assert_eq!(&output[..5], &[0xFFFD, 0xFFFD, 0xFFFD, 0xFFFD, 0x41]);
}

/// `SplitMix64`, the seeding primitive of Zig's `DefaultPrng`: enough for a differential corpus, fixed seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value in `min..=max`.
    fn range(&mut self, min: u8, max: u8) -> u8 {
        let span = u64::from(max - min) + 1;
        min + u8::try_from(self.next() % span).unwrap()
    }

    fn byte(&mut self) -> u8 {
        self.next().to_le_bytes()[0]
    }

    /// One byte of the corpus, in one of three styles: mostly ASCII with specials, heavy multi-byte and invalid
    /// UTF-8, or fully random (rustty's mixture for Ghostty's lane test).
    fn corpus_byte(&mut self, style: u8) -> u8 {
        match style {
            0 => match self.range(0, 20) {
                0 => 0x1B,
                1 => self.range(0, 0x1F),
                2 => self.byte(),
                _ => self.range(0x20, 0x7E),
            },
            1 => match self.range(0, 3) {
                0 => self.range(0x80, 0xBF),
                1 => self.range(0xC0, 0xFF),
                2 => 0x1B,
                _ => self.range(0x20, 0x7E),
            },
            _ => self.byte(),
        }
    }
}

/// Ghostty holds the SIMD lane equal to the scalar one over a random corpus; with the kernel gone, the scalar lane
/// is held to the standard library's lossy decode instead, which substitutes maximal subparts too. Over 10,000
/// random mixtures of ASCII, escapes, controls and valid and invalid UTF-8: the decode stops at the first ESC; the
/// consumed bytes decode to exactly the lossy decode's code points; and bytes left before the ESC or the end are
/// only an incomplete sequence at the true end of the input.
#[test]
fn decode_matches_the_standard_lossy_decode() {
    let mut rng = Rng(0xf00d_face);
    let mut output = [0_u32; 257];
    for _ in 0..10_000 {
        let len = usize::from(rng.range(0, 255)) + usize::from(rng.range(0, 2));
        let style = rng.range(0, 2);
        let input: Vec<u8> = (0..len).map(|_| rng.corpus_byte(style)).collect();

        let res = decode(&input, &mut output);
        let end = input.iter().position(|&b| b == 0x1B).unwrap_or(input.len());
        let expected: Vec<u32> = String::from_utf8_lossy(&input[..res.consumed])
            .chars()
            .map(u32::from)
            .collect();
        assert_eq!(&output[..res.decoded], &expected[..], "input={input:x?}");
        if res.consumed < end {
            assert_eq!(
                end,
                input.len(),
                "only the true end leaves bytes pending, input={input:x?}"
            );
            let rest = &input[res.consumed..];
            let err = std::str::from_utf8(rest).unwrap_err();
            assert_eq!(
                (err.valid_up_to(), err.error_len()),
                (0, None),
                "pending bytes: {rest:x?}"
            );
        } else {
            assert_eq!(res.consumed, end, "input={input:x?}");
        }
    }
}
