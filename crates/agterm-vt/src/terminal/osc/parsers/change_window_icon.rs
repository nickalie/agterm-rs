//! OSC 1: change the window icon (its name, which Ghostty parses and ignores).
//!
//! Ghostty: `src/terminal/osc/parsers/change_window_icon.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/change_window_icon.rs` at `RUSTTY_REV` (MIT).
//! Changes: the text is a slice without Zig's NUL sentinel, which is still written, so a full capture refuses the
//! command as in Zig.

use crate::terminal::osc::{Command, Parser};

/// Parse OSC 1
pub(crate) fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    parser.terminated_trailing().map(Command::ChangeWindowIcon)
}

#[cfg(test)]
mod tests;
