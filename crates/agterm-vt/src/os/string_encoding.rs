//! The two string encodings a shell's command line arrives in (OSC 133's `cmdline` and `cmdline_url` options):
//! `bash`'s `printf %q` quoting and URL percent encoding, decoded into a writer.
//!
//! Ghostty: `src/os/string_encoding.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-os/src/string_encoding.rs` at `RUSTTY_REV` (MIT).
//! Changes: the decoders write to a `std::io::Write`, and their error is [`DecodeError`]: Zig's
//! `std.Io.Writer.Error || error{DecodeError}`. `urlPercentEncode` is not here: the terminal only decodes.

use std::io::Write;

/// A decode failed: the writer refused a byte, or the input is not in the encoding.
#[derive(Debug)]
pub(crate) enum DecodeError {
    /// The writer failed (`std.Io.Writer.Error`).
    Write(std::io::Error),
    /// The input is not in the encoding (`error.DecodeError`).
    Decode,
}

impl From<std::io::Error> for DecodeError {
    fn from(err: std::io::Error) -> Self {
        Self::Write(err)
    }
}

/// The ASCII control bytes `printf %q` escapes stand for (`std.ascii.control_code`).
mod control_code {
    pub(super) const ESC: u8 = 0x1b;
    pub(super) const LF: u8 = 0x0a;
    pub(super) const CR: u8 = 0x0d;
    pub(super) const HT: u8 = 0x09;
    pub(super) const VT: u8 = 0x0b;
}

/// The data inside `buf`'s `$'...'` or `'...'` quoting, or `buf` itself when it is not quoted.
fn strip_quotes(buf: &[u8]) -> Result<&[u8], DecodeError> {
    // Strip off `$''` quoting.
    if let Some(rest) = buf.strip_prefix(b"$'") {
        return rest.strip_suffix(b"'").ok_or(DecodeError::Decode);
    }
    // Strip off `''` quoting.
    if let Some(rest) = buf.strip_prefix(b"'") {
        return rest.strip_suffix(b"'").ok_or(DecodeError::Decode);
    }
    Ok(buf)
}

/// Decode data from the buffer that has been encoded in the same way that `bash`'s `printf %q` encodes a string and
/// write it to the writer. If an error is returned garbage may have been written to the writer.
pub(crate) fn printf_q_decode(writer: &mut impl Write, buf: &[u8]) -> Result<(), DecodeError> {
    let data = strip_quotes(buf)?;
    let mut src: usize = 0;
    while src < data.len() {
        if data[src] != b'\\' {
            writer.write_all(&data[src..=src])?;
            src += 1;
            continue;
        }
        let Some(&escaped) = data.get(src + 1) else {
            return Err(DecodeError::Decode);
        };
        let byte = match escaped {
            b' ' | b'\\' | b'"' | b'\'' | b'$' => escaped,
            b'e' => control_code::ESC,
            b'n' => control_code::LF,
            b'r' => control_code::CR,
            b't' => control_code::HT,
            b'v' => control_code::VT,
            _ => return Err(DecodeError::Decode),
        };
        writer.write_all(&[byte])?;
        src += 2;
    }
    Ok(())
}

/// The value of a hex digit, `None` for any other byte.
fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Decode data from the buffer that has been URL percent encoded and write it to the writer. If an error is returned
/// garbage may have been written to the writer.
pub(crate) fn url_percent_decode(writer: &mut impl Write, buf: &[u8]) -> Result<(), DecodeError> {
    let mut src: usize = 0;
    while src < buf.len() {
        if buf[src] != b'%' {
            writer.write_all(&buf[src..=src])?;
            src += 1;
            continue;
        }
        if src + 2 >= buf.len() {
            return Err(DecodeError::Decode);
        }
        let (Some(high), Some(low)) = (hex(buf[src + 1]), hex(buf[src + 2])) else {
            return Err(DecodeError::Decode);
        };
        writer.write_all(&[(high << 4) | low])?;
        src += 3;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
