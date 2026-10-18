//! OSC 777: rxvt extensions, of which only `notify;title;body` (a desktop notification) is known.
//!
//! Ghostty: `src/terminal/osc/parsers/rxvt_extension.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/rxvt_extension.rs` at `RUSTTY_REV` (MIT).
//! Changes: the title and body are slices without Zig's NUL sentinels, which are still written in place.

use crate::terminal::osc::{Command, Parser, State};

/// The log target of this parser (Ghostty's `.osc_rxvt_extension` scope).
const LOG_SCOPE: &str = "osc_rxvt_extension";

/// Parse OSC 777
pub(crate) fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    // ensure that we are sentinel terminated
    if !parser.write_sentinel() {
        return None;
    }
    let data = parser.capture.as_mut()?.trailing_mut(&mut parser.buffer);
    let Some(k) = data.iter().position(|&b| b == b';') else {
        parser.state = State::Invalid;
        return None;
    };
    let ext = &data[..k];
    if ext != b"notify" {
        tracing::warn!(target: LOG_SCOPE, "unknown rxvt extension: {}", String::from_utf8_lossy(ext));
        parser.state = State::Invalid;
        return None;
    }
    let Some(t) = data[k + 1..].iter().position(|&b| b == b';').map(|off| k + 1 + off) else {
        tracing::warn!(target: LOG_SCOPE, "rxvt notify extension is missing the title");
        parser.state = State::Invalid;
        return None;
    };
    data[t] = 0;
    let data: &[u8] = data;
    Some(Command::ShowDesktopNotification {
        title: &data[k + 1..t],
        body: &data[t + 1..data.len() - 1],
    })
}

#[cfg(test)]
mod tests;
