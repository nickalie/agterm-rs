//! A DFA-based, non-allocating, error-replacing UTF-8 decoder, a byte at a time: the stream's decoder between its
//! bulk decodes.
//!
//! Based largely on Bjoern Hoehrmann's work, with changes for error replacement; see
//! <http://bjoern.hoehrmann.de/utf-8/decoder/dfa> (MIT).
//!
//! Ghostty: `src/terminal/UTF8Decoder.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/utf8_decoder.rs` at `RUSTTY_REV` (MIT).
//! Changes: the transition table is padded to 512 entries with the reject state, so the index (a state plus a byte
//! class, both below 256) is in bounds for every byte with no check; `next` computes both accumulator candidates and
//! stores the state per arm, which keeps the hot accept path free of a loop-carried state load (rustty's reshape,
//! held to a verbatim port of Ghostty's body by an exhaustive test). The accumulator is a `u32` for Zig's `u21`, and
//! the type is `Utf8Decoder` after Rust's naming.

/// The byte classes (`char_classes`, Zig `[256]u4`).
#[rustfmt::skip]
const CHAR_CLASSES: [u8; 256] = [
   0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
   0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
   0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
   0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
   1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,  9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,
   7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,  7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
   8,8,2,2,2,2,2,2,2,2,2,2,2,2,2,2,  2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,
  10,3,3,3,3,3,3,3,3,3,3,3,3,4,3,3, 11,6,6,6,5,8,8,8,8,8,8,8,8,8,8,8,
];

/// Ghostty's `transitions` table, verbatim: the next state for a state plus a byte class.
#[rustfmt::skip]
const REFERENCE_TRANSITIONS: [u8; 108] = [
   0,12,24,36,60,96,84,12,12,12,48,72, 12,12,12,12,12,12,12,12,12,12,12,12,
  12, 0,12,12,12,12,12, 0,12, 0,12,12, 12,24,12,12,12,12,12,24,12,24,12,12,
  12,12,12,12,12,12,12,24,12,12,12,12, 12,24,12,12,12,12,12,12,12,24,12,12,
  12,12,12,12,12,12,12,36,12,36,12,12, 12,36,12,12,12,12,12,36,12,36,12,12,
  12,36,12,12,12,12,12,12,12,12,12,12,
];

/// [`REFERENCE_TRANSITIONS`] padded with the reject state to 512 entries. Reachable states are 0, 12, ..., 96 and
/// byte classes at most 11, so the largest reachable index is 107; the padding only makes `state + class` (each
/// below 256) in bounds for every pair, so the lookup needs no check.
const TRANSITIONS: [u8; 512] = {
    let mut table = [REJECT_STATE; 512];
    let mut i = 0;
    while i < REFERENCE_TRANSITIONS.len() {
        table[i] = REFERENCE_TRANSITIONS[i];
        i += 1;
    }
    table
};

/// The DFA's accept state.
const ACCEPT_STATE: u8 = 0;
/// The DFA's reject state.
const REJECT_STATE: u8 = 12;

/// The decoder: the code point being accumulated and the DFA state.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Utf8Decoder {
    /// The code point accumulated so far (Zig `u21`).
    accumulator: u32,
    /// The DFA state. The stream's fast path reads it to know whether a sequence is open.
    pub(crate) state: u8,
}

impl Utf8Decoder {
    /// Take the next byte of the UTF-8 sequence and return the code point it completes, if any, and whether the
    /// byte was consumed.
    ///
    /// The byte is not consumed only when an ill-formed sequence ends at it: a replacement character is returned,
    /// and the caller passes the same byte again before going on.
    #[inline]
    pub(crate) fn next(&mut self, byte: u8) -> (Option<u32>, bool) {
        let char_class = CHAR_CLASSES[usize::from(byte)];
        let initial_state = self.state;

        // Both candidates of Ghostty's `if`, computed unconditionally and then selected: the same stored value.
        let cont_acc = (self.accumulator << 6) | u32::from(byte & 0x3F);
        let first_acc = (0xFF_u32 >> char_class) & u32::from(byte);
        let acc = if initial_state == ACCEPT_STATE {
            first_acc
        } else {
            cont_acc
        };

        let next_state = TRANSITIONS[usize::from(initial_state) + usize::from(char_class)];
        if next_state == ACCEPT_STATE {
            // The code point is complete.
            self.state = ACCEPT_STATE;
            self.accumulator = 0;
            (Some(acc), true)
        } else if next_state == REJECT_STATE {
            self.accumulator = 0;
            self.state = ACCEPT_STATE;
            // A replacement character; the byte was consumed only when it was the first of its sequence.
            (Some(0xFFFD), initial_state == ACCEPT_STATE)
        } else {
            // In the middle of a sequence.
            self.state = next_state;
            self.accumulator = acc;
            (None, true)
        }
    }
}

#[cfg(test)]
mod tests;
