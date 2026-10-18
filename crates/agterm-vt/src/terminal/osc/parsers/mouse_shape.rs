//! OSC 22: set the mouse pointer shape (its name).
//!
//! Ghostty: `src/terminal/osc/parsers/mouse_shape.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/mouse_shape.rs` at `RUSTTY_REV` (MIT).
//! Changes: the text is a slice without Zig's NUL sentinel, which is still written, so a full capture refuses the
//! command as in Zig.

use crate::terminal::osc::{Command, Parser, State};

/// Parse OSC 22
pub(crate) fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    debug_assert_eq!(parser.state, State::S22);
    parser.terminated_trailing().map(|value| Command::MouseShape { value })
}

#[cfg(test)]
mod tests;
