//! OSC 52: set or query the clipboard.
//!
//! Ghostty: `src/terminal/osc/parsers/clipboard_operation.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/clipboard_operation.rs` at `RUSTTY_REV` (MIT).
//! Changes: the text is a slice without Zig's NUL sentinel, which is still written, so a full capture refuses the
//! command as in Zig.

use crate::terminal::osc::{Command, Parser, State, Terminator};

/// Parse OSC 52
pub(crate) fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert_eq!(parser.state, State::S52);
    if !parser.write_sentinel() {
        return None;
    }
    let data = parser.capture.as_ref()?.trailing(&parser.buffer);
    // `data` ends with the sentinel: `;payload` is the default clipboard, `kind;payload` names one.
    let (kind, text) = match data {
        [b';', text @ .., _] => (b'c', text),
        [kind, b';', text @ .., _] => (*kind, text),
        _ => {
            parser.state = State::Invalid;
            return None;
        }
    };
    Some(Command::ClipboardContents {
        kind,
        data: text,
        terminator: Terminator::init(terminator_ch),
    })
}

#[cfg(test)]
mod tests;
