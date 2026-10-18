//! Kitty's text sizing protocol (OSC 66). Specification: <https://sw.kovidgoyal.net/kitty/text-sizing-protocol/>
//!
//! Ghostty: `src/terminal/osc/parsers/kitty_text_sizing.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/kitty_text_sizing.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's `u3` and `u4` fields are `u8`s held to their ranges; the text is a slice without Zig's NUL
//! sentinel, which is still written. A value is the text up to the next `=`, as Zig's `splitScalar` gives it (rustty
//! read the rest of the pair). The C form (`C`, `cval`) is not here.

use crate::fmt::IntType;
use crate::terminal::osc::{Command, Parser, State, encoding};

/// The log target of this parser (Ghostty's `.kitty_text_sizing` scope).
const LOG_SCOPE: &str = "kitty_text_sizing";

/// The longest payload the protocol allows.
pub(crate) const MAX_PAYLOAD_LENGTH: usize = 4096;

/// The vertical alignment of the text in its cells (`v`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VAlign {
    Top,
    Bottom,
    Center,
}

/// The horizontal alignment of the text in its cells (`h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HAlign {
    Left,
    Right,
    Center,
}

/// A parsed OSC 66 sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Osc<'p> {
    /// 1 - 7 (Zig `u3`)
    pub(crate) scale: u8,
    /// 0 - 7 (0 means default) (Zig `u3`)
    pub(crate) width: u8,
    /// Zig `u4`
    pub(crate) numerator: u8,
    /// Zig `u4`
    pub(crate) denominator: u8,
    pub(crate) valign: VAlign,
    pub(crate) halign: HAlign,
    pub(crate) text: &'p [u8],
}

/// Why a key-value pair was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateError {
    UnknownKey,
    InvalidValue,
}

impl Osc<'_> {
    /// Apply the pair `key=value`.
    fn update(&mut self, key: u8, value: &[u8]) -> Result<(), UpdateError> {
        // All values are numeric, so we can do a small hack here
        let v = crate::parse_int::parse_int_type::<10>(IntType::unsigned(4), value)
            .ok()
            .and_then(|v| u8::try_from(v).ok())
            .ok_or(UpdateError::InvalidValue)?;
        let u3 = |v: u8| if v <= 7 { Ok(v) } else { Err(UpdateError::InvalidValue) };

        match key {
            b's' => {
                if v == 0 {
                    return Err(UpdateError::InvalidValue);
                }
                self.scale = u3(v)?;
            }
            b'w' => self.width = u3(v)?,
            b'n' => self.numerator = v,
            b'd' => self.denominator = v,
            b'v' => {
                self.valign = match v {
                    0 => VAlign::Top,
                    1 => VAlign::Bottom,
                    2 => VAlign::Center,
                    _ => return Err(UpdateError::InvalidValue),
                };
            }
            b'h' => {
                self.halign = match v {
                    0 => HAlign::Left,
                    1 => HAlign::Right,
                    2 => HAlign::Center,
                    _ => return Err(UpdateError::InvalidValue),
                };
            }
            _ => return Err(UpdateError::UnknownKey),
        }
        Ok(())
    }

    /// Apply the `:`-separated `key=value` pairs of `args`, logging and skipping the bad ones.
    fn apply_args(&mut self, args: &[u8]) {
        for kv in args.split(|&b| b == b':') {
            let mut it = kv.split(|&b| b == b'=');
            let Some(k) = it.next() else {
                tracing::warn!(target: LOG_SCOPE, "missing key");
                continue;
            };
            let [key] = *k else {
                tracing::warn!(target: LOG_SCOPE, "key must be a single character");
                continue;
            };
            let Some(value) = it.next() else {
                tracing::warn!(target: LOG_SCOPE, "missing value");
                continue;
            };
            match self.update(key, value) {
                Ok(()) => {}
                Err(UpdateError::UnknownKey) => {
                    tracing::warn!(target: LOG_SCOPE, "unknown key: '{}'", char::from(key));
                }
                Err(UpdateError::InvalidValue) => {
                    tracing::warn!(
                        target: LOG_SCOPE,
                        "invalid value for key '{}': error.InvalidValue",
                        char::from(key)
                    );
                }
            }
        }
    }
}

/// Parse OSC 66
pub(crate) fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    debug_assert_eq!(parser.state, State::S66);

    // Write a NUL byte to ensure that `text` is NUL-terminated
    if !parser.write_sentinel() {
        return None;
    }
    let data = parser.capture.as_ref()?.trailing(&parser.buffer);

    let Some(payload_start) = data.iter().position(|&b| b == b';') else {
        tracing::warn!(target: LOG_SCOPE, "missing semicolon before payload");
        parser.state = State::Invalid;
        return None;
    };
    let payload = &data[payload_start + 1..data.len() - 1];

    // Payload has to be a URL-safe UTF-8 string, and be under the size limit.
    if payload.len() > MAX_PAYLOAD_LENGTH {
        tracing::warn!(target: LOG_SCOPE, "payload is too long");
        parser.state = State::Invalid;
        return None;
    }
    if !encoding::is_safe_utf8(payload) {
        tracing::warn!(target: LOG_SCOPE, "payload is not escape code safe UTF-8");
        parser.state = State::Invalid;
        return None;
    }

    let mut cmd = Osc {
        scale: 1,
        width: 0,
        numerator: 0,
        denominator: 0,
        valign: VAlign::Top,
        halign: HAlign::Left,
        text: payload,
    };

    // Parse any arguments if given
    if payload_start > 0 {
        cmd.apply_args(&data[..payload_start]);
    }

    Some(Command::KittyTextSizing(cmd))
}

#[cfg(test)]
mod tests;
