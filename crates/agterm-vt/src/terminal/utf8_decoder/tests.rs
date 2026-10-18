//! Ghostty's `UTF8Decoder.zig` tests, and rustty's checks of the padded table and the reshaped `next`.

use super::{ACCEPT_STATE, CHAR_CLASSES, REFERENCE_TRANSITIONS, REJECT_STATE, TRANSITIONS, Utf8Decoder};

/// The padded table starts with Ghostty's table, and every padding entry is the reject state.
#[test]
fn transitions_are_the_reference_table_padded_with_reject() {
    assert_eq!(&TRANSITIONS[..REFERENCE_TRANSITIONS.len()], &REFERENCE_TRANSITIONS[..]);
    for (i, &entry) in TRANSITIONS.iter().enumerate().skip(REFERENCE_TRANSITIONS.len()) {
        assert_eq!(entry, REJECT_STATE, "padding entry {i}");
    }
}

/// A verbatim port of Ghostty's branchy `next` body, the oracle for the reshaped one.
fn reference_next(d: &mut Utf8Decoder, byte: u8) -> (Option<u32>, bool) {
    let char_class = CHAR_CLASSES[usize::from(byte)];
    let initial_state = d.state;
    if d.state == ACCEPT_STATE {
        d.accumulator = (0xFF_u32 >> char_class) & u32::from(byte);
    } else {
        d.accumulator <<= 6;
        d.accumulator |= u32::from(byte & 0x3F);
    }
    d.state = REFERENCE_TRANSITIONS[usize::from(d.state) + usize::from(char_class)];
    if d.state == ACCEPT_STATE {
        let cp = d.accumulator;
        d.accumulator = 0;
        (Some(cp), true)
    } else if d.state == REJECT_STATE {
        d.accumulator = 0;
        d.state = ACCEPT_STATE;
        (Some(0xFFFD), initial_state == ACCEPT_STATE)
    } else {
        (None, true)
    }
}

/// Feed `bytes` to both decoders step by step; the returns and the states after each step agree.
fn assert_steps_agree(bytes: impl IntoIterator<Item = u8>) {
    let mut a = Utf8Decoder::default();
    let mut b = Utf8Decoder::default();
    for (i, byte) in bytes.into_iter().enumerate() {
        assert_eq!(a.next(byte), reference_next(&mut b, byte), "step {i}");
        assert_eq!((a.state, a.accumulator), (b.state, b.accumulator), "step {i}");
    }
}

/// The reshaped `next` returns and leaves exactly what Ghostty's body does: over every reachable state, accumulator
/// patterns that tell shift, mask and select errors apart, and every byte; over every 2-byte sequence from a fresh
/// decoder; and over a long pseudo-random byte stream.
#[test]
fn the_reshaped_next_matches_the_reference_control_flow() {
    let mut states: Vec<u8> = REFERENCE_TRANSITIONS.to_vec();
    states.push(ACCEPT_STATE);
    states.sort_unstable();
    states.dedup();
    for &state in &states {
        for accumulator in [0_u32, 0x15555, 0x0AAAA, 0x1F_FFFF] {
            for byte in 0..=255_u8 {
                let mut a = Utf8Decoder { accumulator, state };
                let mut b = a;
                assert_eq!(
                    a.next(byte),
                    reference_next(&mut b, byte),
                    "state={state} acc={accumulator:#x} {byte:#x}"
                );
                assert_eq!((a.state, a.accumulator), (b.state, b.accumulator));
            }
        }
    }

    for b1 in 0..=255_u8 {
        for b2 in 0..=255_u8 {
            assert_steps_agree([b1, b2]);
        }
    }

    let mut x: u32 = 0x243F_6A88;
    assert_steps_agree((0..1_000_000).map(|_| {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x.to_le_bytes()[1]
    }));
}

/// Decode `bytes` the way the stream does: pass a byte again when it was not consumed.
fn decode_all(bytes: &[u8]) -> Vec<u32> {
    let mut d = Utf8Decoder::default();
    let mut out = Vec::new();
    for &byte in bytes {
        loop {
            let (cp, consumed) = d.next(byte);
            out.extend(cp);
            if consumed {
                break;
            }
        }
    }
    out
}

/// Ghostty's `test "ascii"`.
#[test]
fn ascii() {
    let mut d = Utf8Decoder::default();
    for byte in "Hello, World!".bytes() {
        assert_eq!(d.next(byte), (Some(u32::from(byte)), true));
    }
}

/// Ghostty's `test "well formed utf-8"`: 4, 3, 2 and 1 bytes, every byte consumed the first time.
#[test]
fn well_formed_utf_8() {
    let mut d = Utf8Decoder::default();
    let mut out = Vec::new();
    for byte in "😄✤ÁA".bytes() {
        let (cp, consumed) = d.next(byte);
        assert!(consumed);
        out.extend(cp);
    }
    assert_eq!(out, [0x1F604, 0x2724, 0xC1, 0x41]);
}

/// Ghostty's `test "partially invalid utf-8"`: an illegally terminated sequence, a valid one, an illegal surrogate.
#[test]
fn partially_invalid_utf_8() {
    assert_eq!(
        decode_all(b"\xF0\x9F\xF0\x9F\x98\x84\xED\xA0\x80"),
        [0xFFFD, 0x1F604, 0xFFFD, 0xFFFD, 0xFFFD]
    );
}
