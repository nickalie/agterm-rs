//! OSC 7501: Program Status Protocol
//!
//! Specification: <https://www.superlogical.com/rex/docs/build/program-status>
//!
//! A program uses this protocol to tell the terminal what it is doing:
//! idle, working, done, waiting on the user, or failed. Each report is a
//! list of `key=value` pairs separated by colons:
//!
//! ```text
//! ESC ] 7501 ; state=working:app=cargo:progress=40 ESC \
//! ```
//!
//! A terminal keeps one record per id, and each report replaces its
//! record completely. This parser only reads and validates one report at
//! a time. Keeping records is up to the code that handles the command.
//! In libghostty-vt, that is the `program_status` effect of
//! `stream_terminal.Handler`.
//!
//! A program can check whether the terminal supports the protocol by sending `?` as the body. A terminal that supports
//! it replies with `?` and may list the states and kinds it accepts. See [`Command::Query`] and [`QUERY_REPLY`].
//! Checking is optional, so a program may also send reports without asking first.
//!
//! Example:
//!
//! ```text
//! p.next_slice(b"7501;state=working:id=sync:progress=40");
//! let Command::ProgramStatus(Command::Report(report)) = p.end(Some(0x1b))?;
//! report.state; // Working
//! report.read_option(OptionKey::Id); // "sync"
//! report.read_option(OptionKey::Progress); // 40
//! report.read_option(OptionKey::App); // null, the program didn't send one
//! ```
//!
//! Ghostty: `src/terminal/osc/parsers/program_status.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/program_status.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's comptime `Option.Type()` is the [`OptionValue`] a read returns. The base64 decoders of `title` and
//! `msg` are Zig's `std.base64.standard` and `standard_no_pad` (`simd::base64_scalar`). The C form is not here. Drift
//! absorbed: `b7207b2ef` (protocol revision 0.4: the support query's reply lists the states and kinds accepted,
//! [`QUERY_REPLY`]).

use crate::simd::base64_scalar::{Base64Decoder, STANDARD_DECODER, STANDARD_NO_PAD_DECODER};
use crate::terminal::osc::encoding;
use crate::terminal::osc::kitty_metadata::ValueIterator;
use crate::terminal::osc::{Command as OscCommand, Parser, State as ParserState, Terminator};

/// The log target of this parser (Ghostty's `.osc_program_status` scope).
const LOG_SCOPE: &str = "osc_program_status";

// Size limits from the specification. Every limit counts bytes, not
// characters. A report that breaks any limit is discarded as a whole.

/// The longest whole sequence, from `ESC ]` through the terminator.
pub(crate) const MAX_SEQUENCE_BYTES: usize = 4096;

/// The longest body, everything after `7501;`. This assumes the
/// longer terminator (`ESC \`), so a sequence ended with BEL is limited
/// to one byte less than the specification allows. The specification
/// lets terminals choose lower limits.
pub(crate) const MAX_BODY_BYTES: usize = MAX_SEQUENCE_BYTES - b"\x1b]7501;".len() - b"\x1b\\".len();

/// The longest key, including keys this parser doesn't know.
pub(crate) const MAX_KEY_BYTES: usize = 16;

/// The longest `msg` value before and after base64 decoding.
pub(crate) const MAX_MSG_ENCODED_BYTES: usize = 2732;
pub(crate) const MAX_MSG_BYTES: usize = 2048;

/// The longest `title` value before and after base64 decoding.
pub(crate) const MAX_TITLE_ENCODED_BYTES: usize = 256;
pub(crate) const MAX_TITLE_BYTES: usize = 192;

/// The longest `app` value.
pub(crate) const MAX_APP_BYTES: usize = 32;

/// The longest `id` value, the longest part between two slashes, and the
/// most parts an id can have.
pub(crate) const MAX_ID_BYTES: usize = 128;
pub(crate) const MAX_ID_SEGMENT_BYTES: usize = 32;
pub(crate) const MAX_ID_DEPTH: usize = 8;

/// The body of the reply to a support query.
///
/// A program asks whether the terminal supports the protocol by sending `?` as the body. A terminal that does answers
/// with another OSC 7501 whose body is this value, ended with the same terminator the query used:
///
/// ```text
/// program:  ESC ] 7501 ; ? ESC \
/// terminal: ESC ] 7501 ; ?:states=idle,working,done,blocked,error:kinds=permission,question,auth ESC \
/// ```
///
/// The specification only requires the `?`. The rest lists the states and kinds this parser accepts. `clear` is left
/// out because it isn't a state. A program that wants a state or kind from a later revision of the specification
/// checks these lists and falls back to one that is present. Zig builds the lists from `State` and `Kind`; a test holds
/// this value to them.
pub(crate) const QUERY_REPLY: &str = "?:states=idle,working,done,blocked,error:kinds=permission,question,auth";

/// A parsed OSC 7501 sequence: either a support query or a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command<'p> {
    /// The program asked whether the terminal supports the protocol by sending `OSC 7501 ; ? ST`. A terminal that
    /// supports it answers with [`QUERY_REPLY`], ended with this terminator so the program recognizes it. A program
    /// that gets no reply treats the protocol as unsupported. Asking is optional, so reports may arrive without a query
    /// first.
    Query(Terminator),

    /// A valid status report for one record.
    Report(Report<'p>),
}

/// The value of the required `state` key.
///
/// - `idle`: at rest, waiting for the user's next instruction.
/// - `working`: running. May carry a progress percentage.
/// - `done`: finished a piece of work the user hasn't looked at yet.
/// - `blocked`: can't continue until the user does something. The
///   report's `kind` says what. May carry a progress percentage.
/// - `error`: failed and stopped.
/// - `clear`: not a real state. Removes the addressed record and every
///   record beneath it. Without an id, it removes every record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

impl State {
    /// Every state, in Zig's order.
    pub(crate) const ALL: [Self; 6] = [
        Self::Idle,
        Self::Working,
        Self::Done,
        Self::Blocked,
        Self::Error,
        Self::Clear,
    ];

    /// The state's name (`@tagName`).
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
            Self::Clear => "clear",
        }
    }

    /// `std.meta.stringToEnum(State, s)`.
    pub(crate) fn from_name(s: &[u8]) -> Option<State> {
        match s {
            b"idle" => Some(State::Idle),
            b"working" => Some(State::Working),
            b"done" => Some(State::Done),
            b"blocked" => Some(State::Blocked),
            b"error" => Some(State::Error),
            b"clear" => Some(State::Clear),
            _ => None,
        }
    }
}

/// The value of the `kind` key, which says what a blocked program needs
/// from the user.
///
/// - `permission`: approval to do something.
/// - `question`: an answer the user has to type.
/// - `auth`: a login, token, or other credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Permission,
    Question,
    Auth,
}

impl Kind {
    /// Every kind, in Zig's order.
    pub(crate) const ALL: [Self; 3] = [Self::Permission, Self::Question, Self::Auth];

    /// The kind's name (`@tagName`).
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }

    /// `std.meta.stringToEnum(Kind, s)`.
    pub(crate) fn from_name(s: &[u8]) -> Option<Kind> {
        match s {
            b"permission" => Some(Kind::Permission),
            b"question" => Some(Kind::Question),
            b"auth" => Some(Kind::Auth),
            _ => None,
        }
    }
}

/// A status report that passed every check in the specification.
///
/// `state` is always present. Read everything else with `read_option`, and the text values with `write_text`:
///
/// ```text
/// if let OptionValue::Progress(Some(percent)) = report.read_option(OptionKey::Progress) { show_progress(percent) }
/// let mut buf = Vec::new();
/// report.write_text(OptionKey::Msg, &mut buf)?;
/// ```
///
/// The values point into the parser's buffer, so they are only valid until the next call to the parser. Copy anything
/// you want to keep.
///
/// A key the program didn't send reads as null. A report replaces its record completely, so a null value means the
/// record no longer has that value, not that the old value should be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Report<'p> {
    /// What the program is doing.
    pub(crate) state: State,

    /// Everything after `7501;`, already validated by the parser. Read
    /// it with `read_option` and `write_text`.
    pub(crate) data: &'p [u8],
}

/// The value `Report::read_option` produces. The active variant matches
/// `OptionKey::value_type` of the option that was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptionValue<'a> {
    Kind(Option<Kind>),
    Progress(Option<u8>),
    Str(Option<&'a [u8]>),
}

impl<'p> Report<'p> {
    /// Read the value of a key, or null if the program didn't send it.
    /// If the program sent a key more than once, the last value wins.
    pub(crate) fn read_option(&self, option: OptionKey) -> OptionValue<'p> {
        let v = last_value(option.tag_name(), self.data);

        match option {
            // The parser already discarded reports with an invalid id.
            OptionKey::Id => OptionValue::Str(v),
            OptionKey::App => OptionValue::Str(v.filter(|v| is_name(v))),
            OptionKey::Kind => OptionValue::Kind(match v {
                Some(v) if self.state == State::Blocked => Kind::from_name(v),
                _ => None,
            }),
            OptionKey::Progress => OptionValue::Progress(match v {
                Some(v) if matches!(self.state, State::Working | State::Blocked) => parse_progress(v),
                _ => None,
            }),
            OptionKey::Title | OptionKey::Msg => OptionValue::Str(v.filter(|v| !v.is_empty())),
        }
    }

    /// Write the decoded text of `title` or `msg` to `writer`. Writes
    /// nothing if the program didn't send it.
    ///
    /// The text is valid UTF-8 with no control characters, but it is
    /// still untrusted text from the program. It is at most
    /// `MAX_TITLE_BYTES` or `MAX_MSG_BYTES` long.
    pub(crate) fn write_text(&self, option: OptionKey, writer: &mut dyn std::io::Write) -> std::io::Result<()> {
        debug_assert!(option == OptionKey::Title || option == OptionKey::Msg);
        let OptionValue::Str(Some(encoded)) = self.read_option(option) else {
            return Ok(());
        };

        // The parser already checked that this decodes, so it can't fail.
        let mut buf = [0_u8; MAX_MSG_BYTES];
        match decode_text(encoded, &mut buf) {
            Ok(text) => writer.write_all(text),
            Err(_) => Ok(()),
        }
    }
}

/// The keys of a report, other than `state`.
///
/// - `id`: the record this report is about. Without it, the report is
///   about the root record, the program itself. A program that reports on
///   several things at once, like a deploy to several regions, gives each
///   one an id. A `/` makes one record the child of another, so
///   `deploy/us-east` is a child of `deploy`. The parent doesn't have to
///   exist.
/// - `kind`: what a blocked program needs from the user. Only read when
///   the state is `blocked`. An unknown kind reads as null.
/// - `progress`: how far along the work is, from 0 to 100. Only read when the state is `working` or `blocked`. Null
///   means indeterminate: the program is busy but has no percentage to report. A value outside that range reads as
///   null too.
/// - `app`: a stable name for the program that a machine can match on, such as `cargo`, `terraform`, or `claude-code`.
///   It is a key for grouping, filtering, choosing an icon, and so on, not a label. The label is `title`. A record
///   without `app` takes it from its nearest ancestor that has one.
/// - `title`: a short label for the record, meant for people. Programs
///   that report several records use this to tell them apart.
/// - `msg`: one line of text for people, saying what the record is
///   doing, waiting for, or has finished. Don't try to read meaning into
///   it.
///
/// `read_option` returns `title` and `msg` still encoded as base64. Use
/// `write_text` to read their text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptionKey {
    Id,
    Kind,
    Progress,
    App,
    Title,
    Msg,
}

impl OptionKey {
    /// `@tagName(option)`.
    fn tag_name(self) -> &'static [u8] {
        match self {
            OptionKey::Id => b"id",
            OptionKey::Kind => b"kind",
            OptionKey::Progress => b"progress",
            OptionKey::App => b"app",
            OptionKey::Title => b"title",
            OptionKey::Msg => b"msg",
        }
    }
}

/// Parse OSC 7501. The body is everything after `7501;`.
///
/// The parser is forgiving about single pairs and strict about the
/// report as a whole, as the specification requires. It skips a pair
/// that is malformed or has a key it doesn't know, and keeps going:
///
/// ```text
/// state=idle:future=yes:oops   ->  a report with state idle
/// ```
///
/// It returns null, so nothing from the report is applied, when:
///
/// - `state` is missing or isn't a state this parser knows.
/// - Any key or value is longer than its limit, even one that a later
///   pair with the same key replaces.
/// - The id isn't made of valid names joined by single slashes. This
///   throws out the report instead of skipping the pair, so a report
///   meant for one record never falls back to the root record. For
///   `state=clear`, that would remove every record.
/// - `title` or `msg` isn't valid base64, or decodes to text that isn't
///   UTF-8 or contains a control character.
pub(crate) fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<OscCommand<'_>> {
    debug_assert_eq!(parser.state, ParserState::S7501);

    let Some(cap) = parser.capture.as_ref() else {
        parser.state = ParserState::Invalid;
        return None;
    };

    let data = cap.trailing(&parser.buffer);
    if data.len() > MAX_BODY_BYTES {
        tracing::warn!(target: LOG_SCOPE, "OSC 7501 sequence too long len={}", data.len());
        parser.state = ParserState::Invalid;
        return None;
    }

    if data == b"?" {
        return Some(OscCommand::ProgramStatus(Command::Query(Terminator::init(
            terminator_ch,
        ))));
    }
    match validate(data) {
        Ok(state) => Some(OscCommand::ProgramStatus(Command::Report(Report { state, data }))),
        Err(err) => {
            tracing::warn!(target: LOG_SCOPE, "invalid OSC 7501 report err={}", err.name());
            parser.state = ParserState::Invalid;
            None
        }
    }
}

/// Why a report was discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Error {
    InvalidState,
    InvalidId,
    InvalidText,
    TooLong,
}

impl Error {
    /// The Zig error's name, as `{}` prints it.
    const fn name(self) -> &'static str {
        match self {
            Self::InvalidState => "error.InvalidState",
            Self::InvalidId => "error.InvalidId",
            Self::InvalidText => "error.InvalidText",
            Self::TooLong => "error.TooLong",
        }
    }
}

/// Check a report body against the specification and return its state.
fn validate(data: &[u8]) -> Result<State, Error> {
    // A key that is too long breaks a limit, even a key we don't know.
    for pair in data.split(|&b| b == b':') {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            continue;
        };
        let key = trim_whitespace(&pair[0..eq]);
        if key.len() > MAX_KEY_BYTES {
            return Err(Error::TooLong);
        }
    }

    // Every value is checked against its limit, even one a later pair
    // replaces, because any limit violation discards the whole report.
    for v in ValueIterator::init(b"id", Some(VALUE_BYTES), data) {
        validate_id(v)?;
    }
    for (key, limit) in [
        (&b"app"[..], MAX_APP_BYTES),
        (b"title", MAX_TITLE_ENCODED_BYTES),
        (b"msg", MAX_MSG_ENCODED_BYTES),
    ] {
        if ValueIterator::init(key, Some(VALUE_BYTES), data).any(|v| v.len() > limit) {
            return Err(Error::TooLong);
        }
    }

    let mut buf = [0_u8; MAX_MSG_BYTES];
    if let Some(v) = last_value(b"title", data)
        && decode_text(v, &mut buf)?.len() > MAX_TITLE_BYTES
    {
        return Err(Error::TooLong);
    }
    if let Some(v) = last_value(b"msg", data) {
        decode_text(v, &mut buf)?;
    }

    // An unknown state discards the report rather than guessing, so a
    // state added in a later revision never turns into something else.
    last_value(b"state", data)
        .and_then(State::from_name)
        .ok_or(Error::InvalidState)
}

/// The last value of `key` in a report body. Pairs without an `=` and
/// values with bytes outside the value alphabet are skipped.
fn last_value<'a>(key: &'static [u8], data: &'a [u8]) -> Option<&'a [u8]> {
    ValueIterator::init(key, Some(VALUE_BYTES), data).last()
}

/// The bytes allowed in a name and a value. A name is the form of an
/// `app` value and of each part of an `id`.
const NAME_BYTES: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.+-";
/// The bytes allowed in a value: a name's, and `,/=` (`name_bytes ++ ",/="`).
const VALUE_BYTES: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.+-,/=";

/// `std.ascii.whitespace`, trimmed from both ends by `std.mem.trim`.
fn trim_whitespace(s: &[u8]) -> &[u8] {
    const WS: &[u8] = &[b' ', b'\t', b'\n', b'\r', 0x0B, 0x0C];
    let start = s.iter().position(|b| !WS.contains(b)).unwrap_or(s.len());
    let end = s.iter().rposition(|b| !WS.contains(b)).map_or(start, |i| i + 1);
    &s[start..end]
}

/// Whether `s` is a name: non-empty, of the name bytes only.
fn is_name(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|b| NAME_BYTES.contains(b))
}

/// Validate an id: names joined by single slashes, within the limits.
fn validate_id(value: &[u8]) -> Result<(), Error> {
    if value.len() > MAX_ID_BYTES {
        return Err(Error::TooLong);
    }
    let mut depth: usize = 0;
    for segment in value.split(|&b| b == b'/') {
        depth += 1;
        if depth > MAX_ID_DEPTH {
            return Err(Error::TooLong);
        }
        if segment.len() > MAX_ID_SEGMENT_BYTES {
            return Err(Error::TooLong);
        }
        if !is_name(segment) {
            return Err(Error::InvalidId);
        }
    }
    Ok(())
}

/// Parse a whole number from 0 to 100. Anything else is treated as
/// absent. This doesn't use std.fmt because it accepts underscores.
fn parse_progress(value: &[u8]) -> Option<u8> {
    if value.is_empty() {
        return None;
    }
    let mut n: u8 = 0;
    for &c in value {
        if !c.is_ascii_digit() {
            return None;
        }
        // n <= 100 before this step, so n * 10 + 9 fits in a u16 and the result is checked before it is kept.
        let next = u16::from(n) * 10 + u16::from(c - b'0');
        n = u8::try_from(next).ok().filter(|&n| n <= 100)?;
    }
    Some(n)
}

/// Decode a base64 title or msg into `buf` and check that the text is
/// safe to show. Padding is optional. The caller must have checked the
/// encoded length against `MAX_MSG_ENCODED_BYTES`.
fn decode_text<'b>(encoded: &[u8], buf: &'b mut [u8; MAX_MSG_BYTES]) -> Result<&'b [u8], Error> {
    let decoder: &Base64Decoder = if encoded.ends_with(b"=") {
        &STANDARD_DECODER
    } else {
        &STANDARD_NO_PAD_DECODER
    };

    let len = decoder.calc_size_for_slice(encoded).map_err(|_| Error::InvalidText)?;
    if len > buf.len() {
        return Err(Error::TooLong);
    }

    let text = &mut buf[0..len];
    decoder.decode(text, encoded).map_err(|_| Error::InvalidText)?;
    if !encoding::is_safe_utf8(text) {
        return Err(Error::InvalidText);
    }
    Ok(text)
}

#[cfg(test)]
mod tests;
