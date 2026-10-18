//! OSC 7: report the working directory (a file URL, not checked here).
//!
//! Ghostty: `src/terminal/osc/parsers/report_pwd.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/report_pwd.rs` at `RUSTTY_REV` (MIT).
//! Changes: the text is a slice without Zig's NUL sentinel, which is still written, so a full capture refuses the
//! command as in Zig.

use crate::terminal::osc::{Command, Parser};

/// Parse OSC 7
pub(crate) fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    parser.terminated_trailing().map(|value| Command::ReportPwd { value })
}

#[cfg(test)]
mod tests;
