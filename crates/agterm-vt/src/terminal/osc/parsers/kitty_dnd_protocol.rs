//! Kitty's drag and drop protocol (OSC 72). Specification: <https://sw.kovidgoyal.net/kitty/dnd-protocol/>
//!
//! This only captures the raw metadata and payload for the OSC. The actual protocol grammar (metadata keys, event
//! types, chunking) is implemented in `kitty/dnd`, since the protocol requires stateful handling that doesn't belong
//! in the stateless OSC parser.
//!
//! Ghostty: `src/terminal/osc/parsers/kitty_dnd_protocol.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/kitty_dnd_protocol.rs` at `RUSTTY_REV` (MIT).
//! Changes: none beyond Rust's spelling.

use crate::terminal::osc::{Command, Parser, State, Terminator};

/// A parsed OSC 72 sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Osc<'p> {
    /// The raw metadata that was received. Parse with `kitty::dnd::Metadata::parse`.
    pub(crate) metadata: &'p [u8],

    /// The raw payload. Its meaning and encoding depend on the event type (`t` metadata key). Null when the OSC had no
    /// `;` after the metadata; an empty payload is distinct from no payload.
    pub(crate) payload: Option<&'p [u8]>,

    /// The terminator used for this OSC, so any response can match it.
    pub(crate) terminator: Terminator,
}

/// Parse OSC 72
pub(crate) fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert_eq!(parser.state, State::S72);

    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };
    let data = cap.trailing(&parser.buffer);
    let (metadata, payload) = match data.iter().position(|&b| b == b';') {
        None => (data, None),
        Some(sep) => (&data[..sep], Some(&data[sep + 1..])),
    };
    Some(Command::KittyDndProtocol(Osc {
        metadata,
        payload,
        terminator: Terminator::init(terminator_ch),
    }))
}

#[cfg(test)]
mod tests;
