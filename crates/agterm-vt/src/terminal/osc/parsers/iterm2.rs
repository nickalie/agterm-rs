//! OSC 1337: iTerm2's escape codes (<https://iterm2.com/documentation-escape-codes.html>), of which `Copy` (the
//! clipboard) and `CurrentDir` (the working directory) are implemented and the others logged.
//!
//! Ghostty: `src/terminal/osc/parsers/iterm2.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/iterm2.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's `Key` enum and its case-insensitive map are one table of the key names ([`KEYS`]), since only two
//! keys do anything and the rest log their name. The text is slices without Zig's NUL sentinels, which are still
//! written.

use crate::terminal::osc::{Command, Parser, State, Terminator};

/// The log target of this parser (Ghostty's `.osc_iterm2` scope).
const LOG_SCOPE: &str = "osc_iterm2";

/// Every key iTerm2 defines (Zig's `Key` fields), matched ASCII-case-insensitively.
const KEYS: [&str; 36] = [
    "AddAnnotation",
    "AddHiddenAnnotation",
    "Block",
    "Button",
    "ClearCapturedOutput",
    "ClearScrollback",
    "Copy",
    "CopyToClipboard",
    "CurrentDir",
    "CursorShape",
    "Custom",
    "Disinter",
    "EndCopy",
    "File",
    "FileEnd",
    "FilePart",
    "HighlightCursorLine",
    "MultipartFile",
    "OpenURL",
    "PopKeyLabels",
    "PushKeyLabels",
    "RemoteHost",
    "ReportCellSize",
    "ReportVariable",
    "RequestAttention",
    "RequestUpload",
    "SetBackgroundImageFile",
    "SetBadgeFormat",
    "SetColors",
    "SetKeyLabel",
    "SetMark",
    "SetProfile",
    "SetUserVar",
    "ShellIntegrationVersion",
    "StealFocus",
    "UnicodeVersion",
];

/// Parse OSC 1337
pub(crate) fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    debug_assert_eq!(parser.state, State::S1337);

    if !parser.write_sentinel() {
        return None;
    }
    let data = parser.capture.as_mut()?.trailing_mut(&mut parser.buffer);
    let (key, value) = match data.iter().position(|&b| b == b'=') {
        None => (&data[..data.len() - 1], None),
        Some(index) => {
            data[index] = 0;
            let data: &[u8] = data;
            (&data[..index], Some(&data[index + 1..data.len() - 1]))
        }
    };

    let key = KEYS
        .iter()
        .copied()
        .find(|name| name.as_bytes().eq_ignore_ascii_case(key))?;
    match key {
        "Copy" => copy(value?),
        "CurrentDir" => {
            let value = value.filter(|value| !value.is_empty())?;
            Some(Command::ReportPwd { value })
        }
        _ => {
            tracing::debug!(target: LOG_SCOPE, "unimplemented OSC 1337: {key}");
            None
        }
    }
}

/// `Copy=:base64`: set the clipboard.
fn copy(value: &[u8]) -> Option<Command<'_>> {
    // base64 value must be prefixed by a colon
    let value = value.strip_prefix(b":")?;

    // Sending a blank entry to clear the clipboard, or a '?' to query it, is an OSC 52-ism, make sure that is invalid
    // here.
    if value.is_empty() || value == b"?" {
        return None;
    }

    // It would be better to check for valid base64 data here, but that would mean parsing the base64 data twice in
    // the "normal" case.
    Some(Command::ClipboardContents {
        kind: b'c',
        data: value,
        // OSC 1337 Copy never carries a read response, so Zig's field default rides.
        terminator: Terminator::St,
    })
}

#[cfg(test)]
mod tests;
