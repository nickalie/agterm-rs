//! OSC 21: Kitty's color protocol, `21;key=value;...` (query `?`, reset with no value, or set a color).
//!
//! Ghostty: `src/terminal/osc/parsers/kitty_color.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/kitty_color.rs` at `RUSTTY_REV` (MIT).
//! Changes: the request list is the parser's (`Parser::kitty_color`), which the command borrows until the next reset,
//! as Zig's `parser.command` owns it.

use crate::terminal::color::Rgb;
use crate::terminal::kitty::color::{Kind, Osc, Request};
use crate::terminal::osc::{Command, Parser, State, Terminator};

/// The log target of this parser (Ghostty's `.osc_kitty_color` scope).
const LOG_SCOPE: &str = "osc_kitty_color";

/// Parse OSC 21, the Kitty Color Protocol.
pub(crate) fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert_eq!(parser.state, State::S21);

    if !parser.alloc {
        parser.state = State::Invalid;
        return None;
    }
    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };
    let osc = parser.kitty_color.insert(Osc::new(Terminator::init(terminator_ch)));
    let data = cap.trailing(&parser.buffer);
    for kv in data.split(|&b| b == b';') {
        if osc.list.len() >= Kind::MAX * 2 {
            tracing::warn!(
                target: LOG_SCOPE,
                "exceeded limit for number of keys in kitty color protocol, ignoring"
            );
            parser.state = State::Invalid;
            return None;
        }
        let (k, rest) = kv
            .iter()
            .position(|&b| b == b'=')
            .map_or((kv, &[][..]), |i| (&kv[..i], &kv[i + 1..]));
        if k.is_empty() {
            tracing::warn!(target: LOG_SCOPE, "zero length key in kitty color protocol");
            continue;
        }
        let Some(key) = Kind::parse(k) else {
            tracing::warn!(
                target: LOG_SCOPE,
                "unknown key in kitty color protocol: {}",
                String::from_utf8_lossy(k)
            );
            continue;
        };
        let value = trim_spaces(rest);
        let request = match value {
            b"" => Request::Reset(key),
            b"?" => Request::Query(key),
            _ => {
                let Ok(color) = Rgb::parse(value) else {
                    tracing::warn!(
                        target: LOG_SCOPE,
                        "invalid color format in kitty color protocol: {}",
                        String::from_utf8_lossy(value)
                    );
                    continue;
                };
                Request::Set { key, color }
            }
        };
        if osc.append(request).is_err() {
            tracing::warn!(target: LOG_SCOPE, "unable to append kitty color protocol option: error.OutOfMemory");
        }
    }
    Some(Command::KittyColorProtocol(osc))
}

/// `std.mem.trim(u8, value, " ")`: the slice without leading and trailing spaces.
fn trim_spaces(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&b| b != b' ').unwrap_or(s.len());
    let end = s.iter().rposition(|&b| b != b' ').map_or(start, |i| i + 1);
    &s[start..end]
}

#[cfg(test)]
mod tests;
