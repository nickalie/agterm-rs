//! Terminfo source format. This can be used to encode terminfo files. This cannot parse terminfo source files yet
//! because it isn't something Ghostty needs to do, but this can be added later.
//!
//! Background: <https://invisible-island.net/ncurses/man/terminfo.5.html>
//!
//! Ghostty: `src/terminfo/Source.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-terminfo/src/source.rs` at `RUSTTY_REV` (MIT).
//! Changes: the names and values are `&'static str`, as every entry is a constant (Zig's comptime `Source`), not
//! rustty's owned byte vectors; the XTGETTCAP map is built once at run time (Zig builds a `StaticStringMap` at
//! comptime), so what Zig refuses with `@compileError` (a canceled capability, a control character it cannot encode)
//! is an [`XtgettcapError`], which a test and the generator refuse for the shipped entry; the responses carry no NUL
//! sentinel, which only Zig's `[:0]const u8` callback type needed (`74efadb44`); `encode` returns the text, as an
//! entry is text and its one caller (the generator) wants it whole, instead of writing to a `std.Io.Writer`. No drift
//! from rustty's oracle to the pin.

use std::collections::HashMap;
use std::fmt;

/// A terminfo entry's source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    /// The set of names for the terminal. These match the `TERM` environment variable and are used to look up this
    /// terminal. Historically, the final name in the list was the most common name for the terminal and contains
    /// spaces and other characters. See terminfo(5) for details.
    pub(crate) names: &'static [&'static str],

    /// The set of capabilities in this terminfo file.
    pub(crate) capabilities: &'static [Capability],
}

/// A capability in a terminfo file. This also includes any "use" capabilities since they behave just like other
/// capabilities as documented in terminfo(5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Capability {
    /// The name of capability. This is the "Cap-name" value in terminfo(5).
    pub(crate) name: &'static str,
    pub(crate) value: Value,
}

/// A capability's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    /// Canceled value, i.e. suffixed with `@`.
    Canceled,

    /// Boolean values are always true if they exist so there is no value.
    Boolean,

    /// Numeric values are always "unsigned decimal integers". The size of the integer is unspecified in terminfo(5);
    /// Ghostty chose 32 bits because it is a common integer size, but this may be wrong.
    Numeric(u32),

    /// A string value, in terminfo's source form (`\E`, `^G`, `%p1%d`, ...).
    String(&'static str),
}

/// A capability XTGETTCAP cannot answer for, which Ghostty refuses at compile time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XtgettcapError {
    /// A canceled capability (Ghostty: "canceled not handled yet").
    Canceled(&'static str),
    /// A string without parameters whose `^` control character cannot be encoded: one that is not at the start
    /// (Ghostty: "handle control-char in middle of string"), ends the string, or names no control character.
    ControlChar(&'static str),
}

impl fmt::Display for XtgettcapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canceled(name) => write!(f, "capability {name}: canceled not handled yet"),
            Self::ControlChar(name) => write!(f, "capability {name}: a control character XTGETTCAP cannot encode"),
        }
    }
}

impl Source {
    /// Encode as a terminfo source file. The encoding is always done in a human-readable format with whitespace.
    /// Fields are always written in the order of the slices on this struct; this will not do any reordering.
    pub(crate) fn encode(&self) -> String {
        // Encode the names in the order specified
        let mut out = self.names.join("|");
        out.push_str(",\n");

        // Encode each of the capabilities in the order specified
        for cap in self.capabilities {
            out.push('\t');
            out.push_str(cap.name);
            match cap.value {
                Value::Canceled => out.push('@'),
                Value::Boolean => {}
                Value::Numeric(v) => {
                    out.push('#');
                    out.push_str(&v.to_string());
                }
                Value::String(v) => {
                    out.push('=');
                    out.push_str(v);
                }
            }
            out.push_str(",\n");
        }
        out
    }

    /// The XTGETTCAP responses for all of the capabilities in this terminfo, keyed by the capability's name
    /// hex-encoded in uppercase (the form a request carries once the DCS handler has uppercased it). The value is
    /// the FULL response, including the escape sequences: `ESC P 1 + r <name> [= <value>] ESC \`, both hex-encoded.
    ///
    /// Besides the capabilities, `TN` (the first name), `Co` (256) and `RGB` (8) are query-able.
    pub(crate) fn xtgettcap_map(&self) -> Result<HashMap<Vec<u8>, Vec<u8>>, XtgettcapError> {
        // We first build all of our entries with raw K=V pairs.
        let mut kvs: Vec<(&str, Vec<u8>)> = Vec::with_capacity(self.capabilities.len() + 3);
        kvs.push((
            "TN",
            self.names.first().map_or_else(Vec::new, |n| n.as_bytes().to_vec()),
        ));
        kvs.push(("Co", b"256".to_vec()));
        kvs.push(("RGB", b"8".to_vec()));
        for cap in self.capabilities {
            let value = match cap.value {
                Value::Canceled => return Err(XtgettcapError::Canceled(cap.name)),
                Value::Boolean => Vec::new(),
                Value::String(v) => string_response(v).ok_or(XtgettcapError::ControlChar(cap.name))?,
                Value::Numeric(v) => v.to_string().into_bytes(),
            };
            kvs.push((cap.name, value));
        }

        // Now go through and convert them all to hex-encoded strings: the key is just the raw hex-encoded name, the
        // value the response that names it hex-encoded too (important: hex-encoded name).
        Ok(kvs
            .into_iter()
            .map(|(name, value)| {
                let key = hexencode(name.as_bytes());
                let mut response = b"\x1bP1+r".to_vec();
                response.extend_from_slice(&key);
                if !value.is_empty() {
                    response.push(b'=');
                    response.extend_from_slice(&hexencode(&value));
                }
                response.extend_from_slice(b"\x1b\\");
                (key, response)
            })
            .collect())
    }
}

/// A string capability as XTGETTCAP reports it, before hex encoding: as written when it has parameters, else
/// encoded (`\E` as ESC, a leading `^X` as its control character). `None` for a control character that cannot be
/// encoded.
fn string_response(v: &str) -> Option<Vec<u8>> {
    // If a string contains parameters, then we do not escape anything within the string. Ghostty believes the history
    // here is that xterm initially only supported specific capabilities, none had parameters, so it returned the tcap
    // encoded form. Later, Kitty added support for more capabilities, some of which have parameters, but Kitty
    // returned them in terminfo source format. So we need to handle both cases.
    if v.contains('%') {
        return Some(v.as_bytes().to_vec());
    }

    // No parameters. Encode and return. First replace \E with the escape char (0x1B).
    let mut result = replace(v.as_bytes(), b"\\E", b"\x1b");
    // Replace '^' with the control char version of that char.
    while let Some(idx) = result.iter().position(|&b| b == b'^') {
        if idx > 0 {
            return None;
        }
        let c = *result.get(1)?;
        let replacement = match c {
            b'?' => 0x7F, // DEL, special cased from ncurses
            c => c.checked_sub(64)?,
        };
        result = replace(&result, &[b'^', c], &[replacement]);
    }
    Some(result)
}

/// Every byte as two uppercase hex digits (Zig's `std.fmt.bytesToHex(input, .upper)`).
pub(crate) fn hexencode(input: &[u8]) -> Vec<u8> {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    input
        .iter()
        .flat_map(|&b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 0xF)]])
        .collect()
}

/// `input` with every non-overlapping `needle`, left to right, replaced by `replacement` (Zig's `std.mem.replace`).
fn replace(input: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut rest = input;
    while let Some((&first, tail)) = rest.split_first() {
        if let Some(after) = rest.strip_prefix(needle).filter(|_| !needle.is_empty()) {
            out.extend_from_slice(replacement);
            rest = after;
        } else {
            out.push(first);
            rest = tail;
        }
    }
    out
}

#[cfg(test)]
mod tests;
