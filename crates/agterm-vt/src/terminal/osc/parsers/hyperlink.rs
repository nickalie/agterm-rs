//! OSC 8: hyperlinks. `8;params;uri` starts one (`params` are `key=value` pairs separated by `:`, of which only `id`
//! is known), and an empty URI ends it.
//!
//! Ghostty: `src/terminal/osc/parsers/hyperlink.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/hyperlink.rs` at `RUSTTY_REV` (MIT).
//! Changes: the text is slices without Zig's NUL sentinels, which are still written in place (the parameters are split
//! at them as in Zig, so an unknown key logs as Zig's does).

use crate::terminal::osc::{Command, Parser, State};

/// The log target of this parser (Ghostty's `.osc_hyperlink` scope).
const LOG_SCOPE: &str = "osc_hyperlink";

/// Parse OSC 8 hyperlinks
pub(crate) fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    if !parser.write_sentinel() {
        return None;
    }
    let data = parser.capture.as_mut()?.trailing_mut(&mut parser.buffer);
    let Some(s) = data.iter().position(|&b| b == b';') else {
        parser.state = State::Invalid;
        return None;
    };

    data[s] = 0;
    let kvs_len = s + 1;
    for b in &mut data[..kvs_len] {
        if *b == b':' {
            *b = 0;
        }
    }
    let data: &[u8] = data;
    let uri = &data[s + 1..data.len() - 1];
    let id = parse_params(&data[..kvs_len]);

    if uri.is_empty() {
        if id.is_some() {
            parser.state = State::Invalid;
            return None;
        }
        return Some(Command::HyperlinkEnd);
    }

    Some(Command::HyperlinkStart { id, uri })
}

/// The non-empty `id` among the NUL-separated `key=value` parameters `kvs` (which end with a NUL); other keys are
/// logged and ignored.
fn parse_params(kvs: &[u8]) -> Option<&[u8]> {
    let mut id = None;
    let mut kv_start: usize = 0;
    while kv_start < kvs.len() {
        let Some(kv_end) = kvs[kv_start + 1..]
            .iter()
            .position(|&b| b == 0)
            .map(|off| kv_start + 1 + off)
        else {
            break;
        };
        let kv = &kvs[kv_start..=kv_end];
        let Some(v) = kv.iter().position(|&b| b == b'=') else {
            break;
        };
        let key = &kv[..v];
        let value = &kv[v + 1..kv.len() - 1];
        if key == b"id" {
            if !value.is_empty() {
                id = Some(value);
            }
        } else {
            tracing::warn!(
                target: LOG_SCOPE,
                "unknown hyperlink option: '{}'",
                String::from_utf8_lossy(key)
            );
        }
        kv_start = kv_end + 1;
    }
    id
}

#[cfg(test)]
mod tests;
