//! Finds clipboard reads in a VT stream before the terminal parses them (`terminal.h`'s clipboard read callback), so a
//! reader can stop right before the byte that makes Ghostty dispatch one.
//!
//! The read callback is synchronous: the stream waits inside [`Terminal::vt_write`](crate::Terminal::vt_write) until it
//! returns. An embedder whose answer takes time (a consent prompt, a client across a socket) cannot wait there with the
//! terminal's lock held, so it feeds the stream up to the dispatch byte [`Scanner::find`] reports, gets the answer
//! without the lock, arms it, and feeds that one byte, so [`Effects::clipboard_read`](crate::Effects::clipboard_read)
//! answers at once from what was armed. The pane core (`agterm-term`) does so, and the session server is to.
//!
//! The scanner mirrors the states of Ghostty's VT parser that decide whether an OSC is seen at all
//! (`src/terminal/parse_table.zig` at `LIBGHOSTTY_VT_REV`, its "anywhere" transitions included, with the per-state
//! overrides winning as they do there), and the two classifiers mirror Ghostty's OSC 52 and OSC 5522 parsers
//! (`osc/parsers/clipboard_operation.zig`, `kitty/clipboard_command.zig`). A disagreement fails closed: a read the
//! scanner misses reaches the callback with no answer armed and is denied; a candidate Ghostty then rejects only costs
//! an answer that is discarded unused.

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use crate::effects::ClipboardLocation;

/// The most OSC body bytes kept for classification; a longer OSC is never a read candidate (a read's selector,
/// metadata and MIME list are far shorter).
pub(crate) const MAX_BODY: usize = 4096;

/// What a read asks for: data (needs the user's consent under an `ask` policy), or only the list of available types
/// (Kitty's `.` target, served without a prompt).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReadScope {
    Data,
    ListOnly,
}

/// A clipboard read found in the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadCandidate {
    pub location: ClipboardLocation,
    pub scope: ReadScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    SosPmApc,
    Osc,
}

/// The scanner's state across PTY reads.
#[derive(Debug)]
pub struct Scanner {
    state: State,
    body: Vec<u8>,
    overflow: bool,
}

impl Default for Scanner {
    fn default() -> Self {
        Self {
            state: State::Ground,
            body: Vec::with_capacity(64),
            overflow: false,
        }
    }
}

const CAN: u8 = 0x18;
const SUB: u8 = 0x1A;
const ESC: u8 = 0x1B;
const BEL: u8 = 0x07;

/// C0 controls that execute (or are ignored) without leaving a state: everything below 0x20 but CAN, SUB and ESC.
fn is_inert_c0(byte: u8) -> bool {
    byte < 0x20 && !matches!(byte, CAN | SUB | ESC)
}

/// Ghostty's "anywhere" transitions (`parse_table.zig`), for states that do not override the byte.
fn anywhere(byte: u8) -> Option<State> {
    match byte {
        CAN | SUB | 0x80..=0x8F | 0x91..=0x97 | 0x99 | 0x9A | 0x9C => Some(State::Ground),
        ESC => Some(State::Escape),
        0x98 | 0x9E | 0x9F => Some(State::SosPmApc),
        0x9B => Some(State::Csi),
        0x90 => Some(State::DcsEntry),
        0x9D => Some(State::Osc),
        _ => None,
    }
}

impl Scanner {
    /// Advance over `bytes` until the dispatch byte of a clipboard read: returns its offset (the byte is consumed, so
    /// the next call starts after it) and the read, or `None` when `bytes` holds none.
    pub fn find(&mut self, bytes: &[u8]) -> Option<(usize, ReadCandidate)> {
        for (offset, &byte) in bytes.iter().enumerate() {
            if let Some(candidate) = self.step(byte) {
                return Some((offset, candidate));
            }
        }
        None
    }

    /// One byte; `Some` when it dispatched an OSC that is a clipboard read.
    fn step(&mut self, byte: u8) -> Option<ReadCandidate> {
        if self.state == State::Osc {
            return self.osc(byte);
        }
        let next = self.transition(byte);
        if next == State::Osc {
            self.body.clear();
            self.overflow = false;
        }
        self.state = next;
        None
    }

    fn osc(&mut self, byte: u8) -> Option<ReadCandidate> {
        match byte {
            // BEL ends the OSC; ESC leaves it too, and Ghostty dispatches the OSC on that ESC (`Parser.zig`), before
            // knowing whether a `\` follows.
            BEL | ESC => {
                self.state = if byte == ESC { State::Escape } else { State::Ground };
                (!self.overflow).then(|| classify(&self.body)).flatten()
            }
            CAN | SUB => {
                self.state = State::Ground;
                None
            }
            // other C0 controls are ignored inside an OSC; every byte from 0x20 up (C1 included) is data
            _ if byte < 0x20 => None,
            _ => {
                if self.body.len() < MAX_BODY {
                    self.body.push(byte);
                } else {
                    self.overflow = true;
                }
                None
            }
        }
    }

    fn transition(&self, byte: u8) -> State {
        let state = self.state;
        match state {
            // Ghostty decodes UTF-8 in ground: only ESC leaves it (C1 decoded from UTF-8 is ignored).
            State::Ground => {
                if byte == ESC {
                    State::Escape
                } else {
                    State::Ground
                }
            }
            State::Escape => escape(byte).or_else(|| anywhere(byte)).unwrap_or(state),
            State::EscapeIntermediate => match byte {
                _ if is_inert_c0(byte) => state,
                0x20..=0x2F | 0x7F => state,
                0x30..=0x7E => State::Ground,
                _ => anywhere(byte).unwrap_or(state),
            },
            State::Csi => match byte {
                0x40..=0x7E => State::Ground,
                _ => anywhere(byte).unwrap_or(state),
            },
            State::DcsEntry | State::DcsParam | State::DcsIntermediate => dcs(state, byte),
            // payload: high bytes stay in the string (overriding the C1 transitions); 7-bit ST and CAN/SUB end it
            State::DcsPassthrough | State::DcsIgnore => match byte {
                CAN | SUB => State::Ground,
                ESC => State::Escape,
                _ => state,
            },
            State::SosPmApc => match byte {
                _ if is_inert_c0(byte) => state,
                0x20..=0x7F => state,
                _ => anywhere(byte).unwrap_or(state),
            },
            State::Osc => state,
        }
    }
}

/// The escape state's own transitions (`parse_table.zig` "escape").
fn escape(byte: u8) -> Option<State> {
    Some(match byte {
        _ if is_inert_c0(byte) => State::Escape,
        0x7F => State::Escape,
        0x20..=0x2F => State::EscapeIntermediate,
        b'X' | b'^' | b'_' => State::SosPmApc,
        b'P' => State::DcsEntry,
        b'[' => State::Csi,
        b']' => State::Osc,
        0x30..=0x7E => State::Ground,
        _ => return None,
    })
}

/// The DCS header states (entry, param, intermediate) up to the passthrough or ignore state.
fn dcs(state: State, byte: u8) -> State {
    match (state, byte) {
        (_, _) if is_inert_c0(byte) => state,
        (_, 0x7F) => state,
        (_, 0x40..=0x7E) => State::DcsPassthrough,
        (State::DcsEntry, 0x3A) | (State::DcsParam, 0x3A | 0x3C..=0x3F) | (State::DcsIntermediate, 0x30..=0x3F) => {
            State::DcsIgnore
        }
        (State::DcsEntry | State::DcsParam, 0x30..=0x3F) => State::DcsParam,
        (_, 0x20..=0x2F) => State::DcsIntermediate,
        _ => anywhere(byte).unwrap_or(state),
    }
}

/// Whether a dispatched OSC body is a clipboard read Ghostty would hand to the read callback.
fn classify(body: &[u8]) -> Option<ReadCandidate> {
    if let Some(rest) = body.strip_prefix(b"52;") {
        osc_52(rest)
    } else if let Some(rest) = body.strip_prefix(b"5522;") {
        kitty(rest)
    } else {
        None
    }
}

/// OSC 52: an empty or one-byte selector, then `;`, then exactly `?` (`clipboard_operation.zig`).
fn osc_52(rest: &[u8]) -> Option<ReadCandidate> {
    let (kind, payload) = match rest {
        [b';', payload @ ..] => (b'c', payload),
        [kind, b';', payload @ ..] => (*kind, payload),
        _ => return None,
    };
    if payload != b"?" {
        return None;
    }
    let location = match kind {
        b's' => ClipboardLocation::Selection,
        b'p' => ClipboardLocation::Primary,
        _ => ClipboardLocation::Standard,
    };
    Some(ReadCandidate {
        location,
        scope: ReadScope::Data,
    })
}

const MAX_MIME_LEN: usize = 256;
const MAX_PW_LEN: usize = 128;
const MAX_NAME_LEN: usize = 256;

/// Strict RFC 4648 with required padding, as Ghostty decodes OSC 5522 values; non-zero trailing bits are tolerated so
/// that a disagreement there errs towards a candidate (a discarded answer), not a miss (a denied read).
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::RequireCanonical)
        .with_decode_allow_trailing_bits(true),
);

/// The metadata fields a read depends on; the last duplicate wins (`Metadata.Raw.parse`).
#[derive(Default)]
struct Metadata<'a> {
    op: Option<&'a [u8]>,
    loc: &'a [u8],
    mime: &'a [u8],
    pw: &'a [u8],
    name: &'a [u8],
}

fn metadata(raw: &[u8]) -> Option<Metadata<'_>> {
    let mut fields = Metadata::default();
    for record in raw.split(|byte| *byte == b':') {
        let equals = record.iter().position(|byte| *byte == b'=')?;
        let (key, value) = (&record[..equals], &record[equals + 1..]);
        match key {
            b"type" => fields.op = Some(value),
            b"loc" => fields.loc = value,
            b"mime" => fields.mime = value,
            b"pw" => fields.pw = value,
            b"name" => fields.name = value,
            _ => {}
        }
    }
    Some(fields)
}

/// Why a metadata value did not decode (`decodeValue`).
#[derive(Debug, PartialEq, Eq)]
enum ValueError {
    Overflow,
    Invalid,
}

fn decode_value(value: &[u8], max_len: usize) -> Result<(), ValueError> {
    if value.len() > max_len.div_ceil(3) * 4 {
        return Err(ValueError::Overflow);
    }
    let decoded = BASE64.decode(value).map_err(|_| ValueError::Invalid)?;
    if std::str::from_utf8(&decoded).is_err() {
        return Err(ValueError::Invalid);
    }
    if decoded.len() > max_len {
        return Err(ValueError::Overflow);
    }
    Ok(())
}

/// OSC 5522 with `type=read`: metadata and payload that Ghostty would accept, and whether any data type is asked for.
fn kitty(rest: &[u8]) -> Option<ReadCandidate> {
    let (raw, payload) = match rest.iter().position(|byte| *byte == b';') {
        Some(split) => (&rest[..split], &rest[split + 1..]),
        None => (rest, &[][..]),
    };
    let fields = metadata(raw)?;
    if fields.op != Some(b"read") {
        return None;
    }
    // an invalid mime or name drops the request; an over-long password only stops matching grants
    decode_value(fields.mime, MAX_MIME_LEN).ok()?;
    decode_value(fields.name, MAX_NAME_LEN).ok()?;
    if decode_value(fields.pw, MAX_PW_LEN) == Err(ValueError::Invalid) {
        return None;
    }
    let mimes = BASE64.decode(payload).ok()?;
    let mimes = std::str::from_utf8(&mimes).ok()?;
    let data = mimes.split_ascii_whitespace().filter(|mime| *mime != ".").count();
    Some(ReadCandidate {
        location: if fields.loc == b"primary" {
            ClipboardLocation::Primary
        } else {
            ClipboardLocation::Standard
        },
        scope: if data == 0 {
            ReadScope::ListOnly
        } else {
            ReadScope::Data
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATA: ReadCandidate = ReadCandidate {
        location: ClipboardLocation::Standard,
        scope: ReadScope::Data,
    };

    fn reads(stream: &[u8]) -> Vec<(usize, ReadCandidate)> {
        let mut scanner = Scanner::default();
        let mut found = Vec::new();
        let mut start = 0;
        while let Some((offset, read)) = scanner.find(&stream[start..]) {
            found.push((start + offset, read));
            start += offset + 1;
        }
        found
    }

    fn osc(body: &str) -> Vec<u8> {
        format!("\x1b]{body}\x07").into_bytes()
    }

    #[test]
    fn an_osc_52_read_stops_at_bel_or_at_the_esc_of_st() {
        assert_eq!(reads(b"ab\x1b]52;c;?\x07cd"), [(10, DATA)]);
        assert_eq!(reads(b"\x1b]52;c;?\x1b\\"), [(8, DATA)]);
        // ESC followed by something other than `\` still dispatches the OSC
        assert_eq!(reads(b"\x1b]52;c;?\x1b[0m"), [(8, DATA)]);
    }

    #[test]
    fn osc_52_selectors_follow_ghostty() {
        let location = |body: &str| reads(&osc(body)).first().map(|(_, read)| read.location);
        assert_eq!(location("52;;?"), Some(ClipboardLocation::Standard));
        assert_eq!(location("52;c;?"), Some(ClipboardLocation::Standard));
        assert_eq!(location("52;s;?"), Some(ClipboardLocation::Selection));
        assert_eq!(location("52;p;?"), Some(ClipboardLocation::Primary));
        assert_eq!(location("52;x;?"), Some(ClipboardLocation::Standard));
        // two selector bytes, writes, a missing payload, other OSCs: not reads
        for body in [
            "52;cp;?",
            "52;c;aGk=",
            "52;c;",
            "52;",
            "52",
            "2;52;c;?",
            "520;c;?",
            "7;52;c;?",
        ] {
            assert!(reads(&osc(body)).is_empty(), "{body}");
        }
    }

    #[test]
    fn an_osc_inside_other_strings_or_cancelled_is_not_a_read() {
        // CAN and SUB cancel the OSC
        assert!(reads(b"\x1b]52;c;?\x18\x07").is_empty());
        assert!(reads(b"\x1b]52;c;?\x1a\x07").is_empty());
        // `]` as an escape final after an intermediate, or inside a CSI, is no OSC introducer
        assert!(reads(b"\x1b ]52;c;?\x07").is_empty());
        assert!(reads(b"\x1b[]52;c;?\x07").is_empty());
        // C1 bytes in ground are UTF-8, not controls
        assert!(reads(b"\x9d52;c;?\x07").is_empty());
    }

    #[test]
    fn esc_ends_other_strings_and_the_next_sequence_counts() {
        // an APC, a DCS and an SOS end at ESC; the OSC after it is live in Ghostty
        assert_eq!(reads(b"\x1b_Gx\x1b]52;c;?\x07").len(), 1);
        assert_eq!(reads(b"\x1bPq#0\x1b]52;c;?\x07").len(), 1);
        assert_eq!(reads(b"\x1bXsos\x1b]52;c;?\x07").len(), 1);
        // a C0 control in the escape state executes and keeps it: the `]` still opens an OSC
        assert_eq!(reads(b"\x1b\x07]52;c;?\x07").len(), 1);
        // C0 controls inside an OSC are ignored, not data
        assert_eq!(reads(b"\x1b]52;\x05c;?\x07").len(), 1);
        // an 8-bit OSC introducer counts outside ground (here: after ESC)
        assert_eq!(reads(b"\x1b\x9d52;c;?\x07").len(), 1);
    }

    #[test]
    fn the_state_carries_across_reads() {
        let stream = b"x\x1b]52;c;?\x07y\x1b]5522;type=read;dGV4dC9wbGFpbg==\x1b\\z";
        let whole = reads(stream);
        assert_eq!(whole.len(), 2);
        for split in 0..=stream.len() {
            let mut scanner = Scanner::default();
            let mut found = Vec::new();
            for (base, part) in [(0, &stream[..split]), (split, &stream[split..])] {
                let mut start = 0;
                while let Some((offset, read)) = scanner.find(&part[start..]) {
                    found.push((base + start + offset, read));
                    start += offset + 1;
                }
            }
            assert_eq!(found, whole, "split at {split}");
        }
    }

    #[test]
    fn a_body_over_the_cap_is_not_a_read() {
        let long = format!("5522;type=read:x={};dGV4dC9wbGFpbg==", "a".repeat(MAX_BODY));
        assert!(reads(&osc(&long)).is_empty());
        // and the scanner is back in ground afterwards
        let mut stream = osc(&long);
        stream.extend_from_slice(b"\x1b]52;c;?\x07");
        assert_eq!(reads(&stream).len(), 1);
    }

    #[test]
    fn kitty_reads_need_type_read_as_the_last_type() {
        let candidate = |body: &str| reads(&osc(body)).first().map(|(_, read)| *read);
        // "text/plain"
        assert_eq!(candidate("5522;type=read;dGV4dC9wbGFpbg=="), Some(DATA));
        assert_eq!(candidate("5522;type=write:type=read;dGV4dC9wbGFpbg=="), Some(DATA));
        assert_eq!(candidate("5522;type=read:type=write;dGV4dC9wbGFpbg=="), None);
        assert_eq!(candidate("5522;type=write;dGV4dC9wbGFpbg=="), None);
        // a record without `=` makes the metadata invalid
        assert_eq!(candidate("5522;type=read:junk;dGV4dC9wbGFpbg=="), None);
        // unknown keys are ignored; the last loc wins
        assert_eq!(
            candidate("5522;type=read:loc=primary:what=ever;dGV4dC9wbGFpbg==").map(|read| read.location),
            Some(ClipboardLocation::Primary)
        );
        assert_eq!(
            candidate("5522;type=read:loc=primary:loc=clipboard;dGV4dC9wbGFpbg==").map(|read| read.location),
            Some(ClipboardLocation::Standard)
        );
    }

    #[test]
    fn kitty_listing_alone_needs_no_consent() {
        let scope = |body: &str| reads(&osc(body)).first().map(|(_, read)| read.scope);
        // "." / " . " / no payload / an empty payload
        assert_eq!(scope("5522;type=read;Lg=="), Some(ReadScope::ListOnly));
        assert_eq!(scope("5522;type=read;IC4g"), Some(ReadScope::ListOnly));
        assert_eq!(scope("5522;type=read"), Some(ReadScope::ListOnly));
        assert_eq!(scope("5522;type=read;"), Some(ReadScope::ListOnly));
        // ". text/plain"
        assert_eq!(scope("5522;type=read;LiB0ZXh0L3BsYWlu"), Some(ReadScope::Data));
    }

    #[test]
    fn kitty_reads_ghostty_would_drop_are_not_candidates() {
        let candidate = |body: &str| reads(&osc(body)).first().map(|(_, read)| *read);
        // payload: unpadded, not base64, not UTF-8 (0xff)
        assert_eq!(candidate("5522;type=read;dGV4dC9wbGFpbg"), None);
        assert_eq!(candidate("5522;type=read;***"), None);
        assert_eq!(candidate("5522;type=read;/w=="), None);
        // an invalid mime or name value drops the request
        assert_eq!(candidate("5522;type=read:name=***;Lg=="), None);
        assert_eq!(candidate("5522;type=read:mime=***;Lg=="), None);
        // an over-long name drops it, an over-long password does not
        let long = BASE64.encode("n".repeat(MAX_NAME_LEN + 1));
        assert_eq!(candidate(&format!("5522;type=read:name={long};Lg==")), None);
        let long_pw = BASE64.encode("p".repeat(MAX_PW_LEN + 1));
        assert!(candidate(&format!("5522;type=read:pw={long_pw};Lg==")).is_some());
        assert_eq!(candidate("5522;type=read:pw=***;Lg=="), None);
    }
}
