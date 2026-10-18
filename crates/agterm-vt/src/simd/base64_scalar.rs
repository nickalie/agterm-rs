//! Zig's standard base64 decoder: the scalar decoder under [`super::base64`], which is Ghostty's copy of it with the
//! padding checks taken out, because Kitty graphics needs a decoder that does not care about padding, and Zig's own
//! `std.base64.standard` and `standard_no_pad` decoders, which OSC 7501 (`program_status`) decodes its title and
//! message with.
//!
//! Ghostty: `src/simd/base64_scalar.zig` at `LIBGHOSTTY_VT_REV` (copied there from Zig 0.14.1's `std/base64.zig`),
//! and `std/base64.zig`'s `Base64Decoder` of Zig 0.16.0, the version the pin builds with, for the standard decoders.
//! rustty: `crates/rustty-simd/src/base64_scalar.rs` and the `std.base64` decoders of
//! `crates/rustty-vt/src/apc/glyph/request.rs` at `RUSTTY_REV` (MIT).
//! Changes: one type for both, since Ghostty's copy differs from Zig's only in the check it comments out (the unused
//! bits of a partial group): [`Base64Decoder::new`] takes whether to make it. The decoders are built at compile time.
//! One safe core loop serves a separate destination and decoding a buffer on top of itself (rustty used raw pointers
//! for the latter): its source and destination are positions in [`Io`]. `NoSpaceLeft` is gone, since `decode` never
//! returns it.

/// The 64 characters of the standard base64 alphabet (`std.base64.standard_alphabet_chars`).
const STANDARD_ALPHABET: [u8; 64] = *b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The decoder `base64.zig` uses (`scalar_decoder`): the standard alphabet, no padding character, the unused bits of
/// a partial group not checked.
pub(super) static SCALAR_DECODER: Base64Decoder = Base64Decoder::new(&STANDARD_ALPHABET, None, TailBits::Loose);

/// `std.base64.standard.Decoder`: the standard alphabet with `=` padding.
pub(crate) static STANDARD_DECODER: Base64Decoder =
    Base64Decoder::new(&STANDARD_ALPHABET, Some(b'='), TailBits::Checked);

/// `std.base64.standard_no_pad.Decoder`: the standard alphabet without padding.
pub(crate) static STANDARD_NO_PAD_DECODER: Base64Decoder =
    Base64Decoder::new(&STANDARD_ALPHABET, None, TailBits::Checked);

/// Whether `c` is one of the 64 standard alphabet characters (padding is not one).
pub(super) fn is_alphabet_char(c: u8) -> bool {
    SCALAR_DECODER.char_to_index[usize::from(c)] != INVALID_CHAR
}

/// The value of an alphabet character, `None` for any other byte.
pub(super) fn char_value(c: u8) -> Option<u8> {
    let d = SCALAR_DECODER.char_to_index[usize::from(c)];
    (d != INVALID_CHAR).then_some(d)
}

/// `char_to_index` of a byte outside the alphabet.
const INVALID_CHAR: u8 = 0xff;
/// The bits `fast_char_to_index` sets for a byte outside the alphabet.
const INVALID_CHAR_TST: u32 = 0xff00_0000;

/// A decode error.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Error {
    /// A byte outside the alphabet, or a padding character where a decoder has none.
    InvalidCharacter,
    /// A length that cannot be base64, or padding (or the bits of a partial group) that does not close it.
    InvalidPadding,
}

/// What a decoder does with the unused bits of a trailing partial group.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum TailBits {
    /// Zig's check: a partial group leaves at most 4 unused bits, all zero.
    Checked,
    /// Ghostty's copy: dropped, not checked.
    Loose,
}

/// Zig's `Base64Decoder`.
pub(crate) struct Base64Decoder {
    /// An alphabet character's value (`'A'` is 0), `INVALID_CHAR` for any other byte.
    char_to_index: [u8; 256],
    /// Each character's 6 bits already shifted into place for each of the four positions of a group, so a group
    /// decodes with four lookups and three ORs, and any invalid character leaves `INVALID_CHAR_TST` bits set.
    fast_char_to_index: [[u32; 256]; 4],
    /// The padding character, if the encoding pads.
    pad_char: Option<u8>,
    /// Whether the unused bits of a partial group are checked.
    tail_bits: TailBits,
}

impl Base64Decoder {
    const fn new(alphabet: &[u8; 64], pad_char: Option<u8>, tail_bits: TailBits) -> Self {
        let mut result = Self {
            char_to_index: [INVALID_CHAR; 256],
            fast_char_to_index: [[INVALID_CHAR_TST; 256]; 4],
            pad_char,
            tail_bits,
        };
        // A `u8` counter: the 64 indices fit it, and it widens losslessly where a `u32` or an index is needed.
        let mut i: u8 = 0;
        while (i as usize) < alphabet.len() {
            let c = alphabet[i as usize];
            assert!(
                result.char_to_index[c as usize] == INVALID_CHAR,
                "an alphabet character appears twice"
            );
            if let Some(pad) = pad_char {
                assert!(c != pad, "the padding character is not in the alphabet");
            }
            let c = c as usize;
            let ci = i as u32;
            result.fast_char_to_index[0][c] = ci << 2;
            result.fast_char_to_index[1][c] = (ci >> 4) | ((ci & 0x0f) << 12);
            result.fast_char_to_index[2][c] = ((ci & 0x3) << 22) | ((ci & 0x3c) << 6);
            result.fast_char_to_index[3][c] = ci << 16;
            result.char_to_index[c] = i;
            i += 1;
        }
        result
    }

    /// The most bytes `source_len` characters decode to (`calcSizeUpperBound`); fewer when the input ends in padding.
    /// With padding, the input is whole groups; without, one character past a whole group cannot be decoded. Without
    /// padding the bound is exact.
    pub(crate) const fn calc_size_upper_bound(&self, source_len: usize) -> Result<usize, Error> {
        let leftover = source_len % 4;
        let whole = source_len / 4 * 3;
        if self.pad_char.is_some() {
            if leftover != 0 {
                return Err(Error::InvalidPadding);
            }
            return Ok(whole);
        }
        if leftover == 1 {
            return Err(Error::InvalidPadding);
        }
        Ok(whole + leftover * 3 / 4)
    }

    /// The exact decoded size of `source` (`calcSizeForSlice`): the upper bound less one byte per trailing padding
    /// character, of the last two.
    pub(crate) fn calc_size_for_slice(&self, source: &[u8]) -> Result<usize, Error> {
        let mut result = self.calc_size_upper_bound(source.len())?;
        if let Some(pad) = self.pad_char {
            if source.last() == Some(&pad) {
                result -= 1;
            }
            if source.len() >= 2 && source[source.len() - 2] == pad {
                result -= 1;
            }
        }
        Ok(result)
    }

    /// Decode `source` into `dest`. `dest` holds at least [`Self::calc_size_for_slice`] of it; a longer `dest` lets
    /// more of it go through the fast steps, and its bytes past the decoded size are scratch.
    ///
    /// # Errors
    ///
    /// [`Error`] when `source` is not base64 of this decoder's kind. `dest` may be partly written.
    pub(crate) fn decode(&self, dest: &mut [u8], source: &[u8]) -> Result<(), Error> {
        self.decode_io(&mut Split { source, dest })
    }

    /// Decode `buf[..source_len]` on top of itself into `buf[..dest_len]`, as Ghostty's Kitty graphics payload decode
    /// does. Safe because every step reads its source bytes before it writes, and a write never reaches a byte not
    /// yet read: the 16-byte step reads `[k, k + 16)` and writes `[3k/4, 3k/4 + 16)`, the 4-byte step reads
    /// `[k, k + 4)` and writes `[3k/4, 3k/4 + 4)`, and the tail writes byte `3k/4` after reading byte `k`.
    pub(super) fn decode_in_place(&self, buf: &mut [u8], dest_len: usize, source_len: usize) -> Result<(), Error> {
        assert!(
            dest_len <= buf.len() && source_len <= buf.len(),
            "the decode stays inside the buffer"
        );
        self.decode_io(&mut InPlace {
            buf,
            dest_len,
            source_len,
        })
    }

    /// The decode: the fast steps, then one character at a time up to the end or the first padding character, then
    /// the checks of the partial group and the padding.
    fn decode_io(&self, io: &mut impl Io) -> Result<(), Error> {
        let source_len = io.source_len();
        if self.pad_char.is_some() && !source_len.is_multiple_of(4) {
            return Err(Error::InvalidPadding);
        }
        let (mut src_idx, mut dest_idx) = self.decode_fast(io)?;
        let mut acc: u32 = 0;
        let mut acc_len: u32 = 0;
        let mut leftover_idx = None;
        while src_idx < source_len {
            let c = io.read(src_idx);
            let d = self.char_to_index[usize::from(c)];
            if d == INVALID_CHAR {
                if self.pad_char != Some(c) {
                    return Err(Error::InvalidCharacter);
                }
                leftover_idx = Some(src_idx);
                break;
            }
            // Zig's accumulator is a `u12`: only the low 12 bits are kept.
            acc = ((acc << 6) | u32::from(d)) & 0xfff;
            acc_len += 6;
            if acc_len >= 8 {
                acc_len -= 8;
                io.write(dest_idx, &[(acc >> acc_len).to_le_bytes()[0]]);
                dest_idx += 1;
            }
            src_idx += 1;
        }
        if self.tail_bits == TailBits::Checked && (acc_len > 4 || acc & ((1 << acc_len) - 1) != 0) {
            return Err(Error::InvalidPadding);
        }
        match (leftover_idx, self.pad_char) {
            (Some(at), Some(pad)) => check_padding(io, at, pad, acc_len / 2),
            _ => Ok(()),
        }
    }

    /// The fast steps: 16 characters to 12 bytes while both sides have room, then 4 to 3. They store 16 and 4 bytes
    /// for 12 and 3, so they run only while the destination has that room, and never take the last group, which may
    /// hold padding. Returns where the source and the destination stand.
    fn decode_fast(&self, io: &mut impl Io) -> Result<(usize, usize), Error> {
        let (source_len, dest_len) = (io.source_len(), io.dest_len());
        let mut dest_idx = 0;
        let mut src_idx = 0;
        while src_idx + 16 < source_len && dest_idx + 15 < dest_len {
            let mut bits: u128 = 0;
            for group in 0..4 {
                let group_bits = self.group_bits(io, src_idx + group * 4)?;
                bits |= u128::from(group_bits) << (24 * group);
            }
            io.write(dest_idx, &bits.to_le_bytes());
            src_idx += 16;
            dest_idx += 12;
        }
        while src_idx + 4 < source_len && dest_idx + 3 < dest_len {
            let bits = self.group_bits(io, src_idx)?;
            io.write(dest_idx, &bits.to_le_bytes());
            src_idx += 4;
            dest_idx += 3;
        }
        Ok((src_idx, dest_idx))
    }

    /// The 24 decoded bits of the four characters at `at`, little-endian in the low three bytes.
    fn group_bits(&self, io: &impl Io, at: usize) -> Result<u32, Error> {
        let bits = self.fast_char_to_index[0][usize::from(io.read(at))]
            | self.fast_char_to_index[1][usize::from(io.read(at + 1))]
            | self.fast_char_to_index[2][usize::from(io.read(at + 2))]
            | self.fast_char_to_index[3][usize::from(io.read(at + 3))];
        if bits & INVALID_CHAR_TST != 0 {
            return Err(Error::InvalidCharacter);
        }
        Ok(bits)
    }
}

/// Zig's check of the padding from `at` on: only padding characters, as many as the partial group asks for
/// (`padding_len`). A byte other than padding is `InvalidPadding`, except 0xFF, which Zig's check compares with its
/// invalid-character marker and so reports as `InvalidCharacter`.
fn check_padding(io: &impl Io, at: usize, pad: u8, padding_len: u32) -> Result<(), Error> {
    let mut padding_chars: u32 = 0;
    for idx in at..io.source_len() {
        let c = io.read(idx);
        if c != pad {
            return Err(if c == INVALID_CHAR {
                Error::InvalidCharacter
            } else {
                Error::InvalidPadding
            });
        }
        padding_chars += 1;
    }
    if padding_chars == padding_len {
        Ok(())
    } else {
        Err(Error::InvalidPadding)
    }
}

/// Where a decode reads its characters and writes its bytes: positions into a source and a destination, which may
/// be one buffer.
trait Io {
    fn source_len(&self) -> usize;
    fn dest_len(&self) -> usize;
    fn read(&self, at: usize) -> u8;
    fn write(&mut self, at: usize, bytes: &[u8]);
}

/// A source and a separate destination.
struct Split<'a> {
    source: &'a [u8],
    dest: &'a mut [u8],
}

impl Io for Split<'_> {
    fn source_len(&self) -> usize {
        self.source.len()
    }

    fn dest_len(&self) -> usize {
        self.dest.len()
    }

    fn read(&self, at: usize) -> u8 {
        self.source[at]
    }

    fn write(&mut self, at: usize, bytes: &[u8]) {
        self.dest[at..at + bytes.len()].copy_from_slice(bytes);
    }
}

/// One buffer, decoded on top of itself: the source is `buf[..source_len]`, the destination `buf[..dest_len]`.
struct InPlace<'a> {
    buf: &'a mut [u8],
    dest_len: usize,
    source_len: usize,
}

impl Io for InPlace<'_> {
    fn source_len(&self) -> usize {
        self.source_len
    }

    fn dest_len(&self) -> usize {
        self.dest_len
    }

    fn read(&self, at: usize) -> u8 {
        self.buf[at]
    }

    fn write(&mut self, at: usize, bytes: &[u8]) {
        self.buf[at..at + bytes.len()].copy_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests;
