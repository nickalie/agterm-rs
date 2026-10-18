//! An OSC sequence whose number the parser does not implement, kept when `Parser::unknown_max_bytes` is nonzero.
//!
//! Ghostty: `src/terminal/osc/parsers/unknown.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/unknown.rs` at `RUSTTY_REV` (MIT).
//! Changes: none beyond Rust's spelling.

use crate::terminal::osc::{Command, Parser, State, Terminator, Unknown};

/// Build `Command::Unknown` from the bytes collected for a sequence whose number the parser does not implement. Only
/// reached when `Parser::unknown_max_bytes` is nonzero; unknown states always have an active capture.
pub(crate) fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert!(parser.state == State::Unknown || parser.state == State::UnknownTruncated);

    let truncated = parser.state == State::UnknownTruncated;
    let cap = parser.capture.as_ref()?;
    Some(Command::Unknown(Unknown {
        content: cap.trailing(&parser.buffer),
        truncated,
        terminator: Terminator::init(terminator_ch),
    }))
}

#[cfg(test)]
mod tests;
