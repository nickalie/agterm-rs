//! Base64 decoding for the terminal's protocols: the forgiving decode Kitty graphics payloads go through, the
//! strict RFC 4648 decode of the Kitty clipboard and OSC 52, and a streaming strict decode for payloads split
//! across packets.
//!
//! Ghostty: `src/simd/base64.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-simd/src/base64.rs` at `RUSTTY_REV` (MIT).
//! Changes: one lane, scalar, behaving as the lane libghostty-vt ships. Ghostty's `maxLen` and `decode` call simdutf
//! (`maximal_binary_length_from_base64`, `base64_to_binary` with its defaults: WHATWG forgiving base64, loose last
//! chunk) when built with SIMD, the default, and its scalar decoder otherwise; the scalar lane differs on input that
//! is not plain base64 (it rejects whitespace, accepts any run of trailing `=`, and answers an empty success with a
//! warning for a one-character remainder). Here `max_len` is simdutf's bound, and `decode` and `decode_in_place`
//! apply simdutf's rules (simdutf 9.0.0, `scalar::base64::find_end` and `base64_to_binary_details_impl`) around
//! Ghostty's scalar decoder: ASCII whitespace (HT, LF, FF, CR, SP) is skipped; at most two `=` close the input,
//! with whitespace around them allowed, and with any padding the significant characters fill whole groups; a
//! one-character remainder, any other byte and a third `=` are errors; unused bits of a partial group are not
//! checked. So nothing logs on this lane, as on Ghostty's SIMD one. `decode_strict` and `Streaming` are Ghostty's,
//! over that `decode`. The in-place decode takes the buffer once, since Rust cannot alias it as input and output.

use crate::simd::base64_scalar::{SCALAR_DECODER, char_value, is_alphabet_char};

/// The input is not valid base64 (Zig's `error{Base64Invalid}`).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) struct Base64Invalid;

/// The most bytes `input` can decode to: simdutf's `maximal_binary_length_from_base64`. A bound for the output
/// buffer, never a validity check: up to two trailing `=` are not counted, and whitespace is counted as data.
pub(crate) fn max_len(input: &[u8]) -> usize {
    let padding = match input {
        [.., b'=', b'='] => 2,
        [.., b'='] => 1,
        _ => 0,
    };
    let actual = input.len() - padding;
    let remainder = actual % 4;
    if remainder <= 1 {
        actual / 4 * 3
    } else {
        actual / 4 * 3 + remainder - 1
    }
}

/// Decode forgiving base64 from `input` into `output`, which holds at least [`max_len`] of it, and return the decoded
/// bytes.
///
/// # Errors
///
/// [`Base64Invalid`] when `input` is not forgiving base64 (see the module header). `output` may be partly written.
pub(crate) fn decode<'a>(input: &[u8], output: &'a mut [u8]) -> Result<&'a [u8], Base64Invalid> {
    let end = find_end(input);
    let data = &input[..end.data_len];
    let len = if data.iter().copied().any(is_ignorable) {
        decode_skipping_whitespace(data, output)?
    } else {
        decode_plain(data, output)?
    };
    check_padding(len, end.padding)?;
    Ok(&output[..len])
}

/// [`decode`] of `buf` on top of itself, `decode(buf, buf[..max_len])`; returns the decoded length. `max_len` is
/// [`max_len`] of `buf`. Ghostty's Kitty graphics payload decode passes one buffer as input and output, since the
/// decoded bytes never overtake the characters still to read; this does the same without copying the payload.
///
/// # Errors
///
/// [`Base64Invalid`] as [`decode`] has it. `buf` may be changed.
///
/// # Panics
///
/// If `max_len` is longer than `buf`.
pub(crate) fn decode_in_place(buf: &mut [u8], max_len: usize) -> Result<usize, Base64Invalid> {
    assert!(max_len <= buf.len(), "max_len is the bound of this buffer");
    let end = find_end(buf);
    let data_len = compact_whitespace(&mut buf[..end.data_len]);
    let len = SCALAR_DECODER
        .calc_size_upper_bound(data_len)
        .map_err(|_| Base64Invalid)?;
    debug_assert!(len <= max_len, "max_len bounds the decoded size");
    SCALAR_DECODER
        .decode_in_place(buf, max_len, data_len)
        .map_err(|_| Base64Invalid)?;
    check_padding(len, end.padding)?;
    Ok(len)
}

/// Whether `c` is ASCII whitespace, which forgiving base64 skips (simdutf's ignorable bytes for the default
/// alphabet: HT, LF, FF, CR, SP; not VT).
fn is_ignorable(c: u8) -> bool {
    matches!(c, b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

/// The data region of forgiving base64 input and the padding after it.
struct End {
    /// The length of the input before its padding and trailing whitespace.
    data_len: usize,
    /// The number of `=` that close the input: 0, 1 or 2.
    padding: usize,
}

/// simdutf's `find_end`: skip trailing whitespace; a last `=` is padding, and after more whitespace a `=` before it
/// is the second. A third `=` stays in the data, where it is an invalid character.
fn find_end(input: &[u8]) -> End {
    let trim = |mut len: usize| {
        while len > 0 && is_ignorable(input[len - 1]) {
            len -= 1;
        }
        len
    };
    let mut data_len = trim(input.len());
    let mut padding = 0;
    if data_len > 0 && input[data_len - 1] == b'=' {
        padding = 1;
        data_len = trim(data_len - 1);
        if data_len > 0 && input[data_len - 1] == b'=' {
            padding = 2;
            data_len -= 1;
        }
    }
    End { data_len, padding }
}

/// simdutf's check of padding against the decoded length `len`: with padding, the significant characters fill whole
/// groups, so `len % 3` plus one plus the padding is four (and `len % 3` is not 0). No padding is always fine.
fn check_padding(len: usize, padding: usize) -> Result<(), Base64Invalid> {
    if padding == 0 {
        return Ok(());
    }
    let rem = len % 3;
    if rem == 0 || rem + 1 + padding != 4 {
        return Err(Base64Invalid);
    }
    Ok(())
}

/// Decode a data region without whitespace through Ghostty's scalar decoder.
fn decode_plain(data: &[u8], output: &mut [u8]) -> Result<usize, Base64Invalid> {
    let len = SCALAR_DECODER
        .calc_size_upper_bound(data.len())
        .map_err(|_| Base64Invalid)?;
    SCALAR_DECODER.decode(output, data).map_err(|_| Base64Invalid)?;
    Ok(len)
}

/// Decode a data region with whitespace in it, a character at a time, skipping the whitespace.
fn decode_skipping_whitespace(data: &[u8], output: &mut [u8]) -> Result<usize, Base64Invalid> {
    let mut acc: u32 = 0;
    let mut acc_len: u32 = 0;
    let mut chars = 0;
    let mut len = 0;
    for c in data.iter().copied().filter(|&c| !is_ignorable(c)) {
        let d = char_value(c).ok_or(Base64Invalid)?;
        acc = ((acc << 6) | u32::from(d)) & 0xfff;
        acc_len += 6;
        chars += 1;
        if acc_len >= 8 {
            acc_len -= 8;
            output[len] = (acc >> acc_len).to_le_bytes()[0];
            len += 1;
        }
    }
    if chars % 4 == 1 {
        return Err(Base64Invalid);
    }
    Ok(len)
}

/// Move the non-whitespace bytes of `buf` to its front, in order, and return how many there are. Without whitespace
/// it only scans.
fn compact_whitespace(buf: &mut [u8]) -> usize {
    let Some(first) = buf.iter().copied().position(is_ignorable) else {
        return buf.len();
    };
    let mut write = first;
    for read in first + 1..buf.len() {
        if !is_ignorable(buf[read]) {
            buf[write] = buf[read];
            write += 1;
        }
    }
    write
}

/// Whether strict decoding requires the input padded to a multiple of four bytes (RFC 4648 section 3.2). The Kitty
/// clipboard protocol requires padding; legacy OSC 52 tolerates a missing-padding tail, because it has no way to
/// report errors to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Padding {
    Required,
    Optional,
}

/// Decode strict RFC 4648 standard-alphabet base64: bytes outside the alphabet (whitespace included) and misplaced
/// padding are errors instead of being skipped, and padding is checked per `padding`. The decoding the Kitty
/// clipboard protocol specifies: <https://sw.kovidgoyal.net/kitty/clipboard/#encoding-of-payloads>.
///
/// `output` holds at least [`max_len`] of `input`.
///
/// # Errors
///
/// [`Base64Invalid`] when `input` is not strict base64 under `padding`.
pub(crate) fn decode_strict<'a>(
    input: &[u8],
    output: &'a mut [u8],
    padding: Padding,
) -> Result<&'a [u8], Base64Invalid> {
    // Padding can only be a suffix of at most two bytes; a '=' anywhere else fails the decode below.
    let pad = usize::from(input.last() == Some(&b'=')) + usize::from(input.len() > 1 && input[input.len() - 2] == b'=');
    let groups_complete = input.len().is_multiple_of(4);
    let refused = match padding {
        Padding::Required => !groups_complete,
        // Present padding must still complete a group; only absent padding is tolerated. A single leftover byte can
        // never carry a decodable value.
        Padding::Optional => (pad > 0 && !groups_complete) || input.len() % 4 == 1,
    };
    if refused {
        return Err(Base64Invalid);
    }

    // The forgiving decode rejects every invalid byte but whitespace, which it skips. Skipped bytes make the decoded
    // length fall short of the exact length the input's length implies, so comparing the two rejects whitespace
    // without another pass over the input.
    let decoded = decode(input, output)?;
    let whole = input.len() / 4 * 3;
    let expected = match input.len() % 4 {
        0 => whole - pad,
        2 => whole + 1,
        _ => whole + 2,
    };
    if decoded.len() != expected {
        return Err(Base64Invalid);
    }
    Ok(decoded)
}

/// A streaming strict base64 decoder for one payload split across chunks at arbitrary byte boundaries (the Kitty
/// clipboard protocol's wdata packets): the concatenated chunks must be valid RFC 4648 standard-alphabet base64.
///
/// Padding ends a feed: a padded group followed by more data in the same feed is an error. A feed that ends exactly
/// at its padding resets the decoder, so the next feed starts a fresh stream, as kitty's implementation does (it
/// resets its streaming decoder at the end of each chunk); clients that pad every chunk keep working.
#[derive(Debug, Default)]
pub(crate) struct Streaming {
    /// A partial group carried between feeds: a group decodes once all four of its characters are in.
    carry: [u8; 4],
    carry_len: usize,
}

impl Streaming {
    /// The most bytes one feed of `input` can decode to.
    pub(crate) fn max_len(&self, input: &[u8]) -> usize {
        (self.carry_len + input.len()) / 4 * 3
    }

    /// Decode the complete groups of `input`, after any carried characters, into `output`, which holds at least
    /// [`Self::max_len`] of it, and carry the rest to the next feed.
    ///
    /// # Errors
    ///
    /// [`Base64Invalid`] for a byte outside the alphabet, padding out of place, or data after padding.
    pub(crate) fn feed<'a>(&mut self, input: &[u8], output: &'a mut [u8]) -> Result<&'a [u8], Base64Invalid> {
        debug_assert!(output.len() >= self.max_len(input));
        if input.is_empty() {
            return Ok(&output[..0]);
        }
        let mut rem = input;
        let mut written = 0;
        if self.carry_len > 0 {
            let Carry::Decoded { len, padded } = self.complete_carry(&mut rem, output)? else {
                return Ok(&output[..0]);
            };
            written = len;
            if padded {
                return padding_ends_feed(rem.is_empty(), &output[..written]);
            }
        }

        // Every complete group goes through the strict single-shot decode, which also holds padding to a closing
        // suffix; padding then must end the feed.
        let bulk = &rem[..rem.len() - rem.len() % 4];
        written += decode_strict(bulk, &mut output[written..], Padding::Required)?.len();
        if bulk.last() == Some(&b'=') {
            return padding_ends_feed(bulk.len() == rem.len(), &output[..written]);
        }

        // Carry the trailing partial group, checked now so garbage is reported on the feed that holds it.
        let tail = &rem[bulk.len()..];
        if !tail.iter().enumerate().all(|(pos, &c)| valid_partial_char(pos, c)) {
            return Err(Base64Invalid);
        }
        self.carry[..tail.len()].copy_from_slice(tail);
        self.carry_len = tail.len();
        Ok(&output[..written])
    }

    /// Fill the carried partial group from the front of `rem` (advancing it), and decode it once it is complete.
    fn complete_carry(&mut self, rem: &mut &[u8], output: &mut [u8]) -> Result<Carry, Base64Invalid> {
        let take = (4 - self.carry_len).min(rem.len());
        let incoming = &rem[..take];
        if !incoming
            .iter()
            .zip(self.carry_len..)
            .all(|(&c, pos)| valid_partial_char(pos, c))
        {
            return Err(Base64Invalid);
        }
        self.carry[self.carry_len..self.carry_len + take].copy_from_slice(incoming);
        self.carry_len += take;
        *rem = &rem[take..];
        if self.carry_len < 4 {
            return Ok(Carry::Pending);
        }
        self.carry_len = 0;
        let len = decode_strict(&self.carry, output, Padding::Required)?.len();
        Ok(Carry::Decoded {
            len,
            padded: self.carry[3] == b'=',
        })
    }

    /// Finish the stream: the fed chunks must have formed complete groups, so a carried partial group is an error
    /// (the stream was not padded). The decoder is ready for a fresh stream afterwards either way.
    ///
    /// # Errors
    ///
    /// [`Base64Invalid`] when a partial group is left.
    pub(crate) fn finish(&mut self) -> Result<(), Base64Invalid> {
        let carry_len = self.carry_len;
        *self = Self::default();
        if carry_len == 0 { Ok(()) } else { Err(Base64Invalid) }
    }
}

/// What filling the carried group gave.
enum Carry {
    /// Still short of four characters.
    Pending,
    /// The group decoded to `len` bytes; `padded` when it ended in padding.
    Decoded { len: usize, padded: bool },
}

/// The result of a feed whose decode ended in padding: the decoded bytes when the padding ended the feed
/// (`at_end`), an error when data follows it.
fn padding_ends_feed(at_end: bool, decoded: &[u8]) -> Result<&[u8], Base64Invalid> {
    if at_end { Ok(decoded) } else { Err(Base64Invalid) }
}

/// Whether `c` is valid at position `pos` of a partial group still waiting for its other characters: the first two
/// positions take alphabet characters only, and only the last two may open the padding.
fn valid_partial_char(pos: usize, c: u8) -> bool {
    is_alphabet_char(c) || (pos >= 2 && c == b'=')
}

#[cfg(test)]
mod tests;
