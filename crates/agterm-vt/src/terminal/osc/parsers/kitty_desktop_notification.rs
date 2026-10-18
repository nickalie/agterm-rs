//! Kitty's desktop notification protocol (OSC 99). Specification:
//! <https://sw.kovidgoyal.net/kitty/desktop-notifications/>
//!
//! Ghostty: `src/terminal/osc/parsers/kitty_desktop_notification.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/kitty_desktop_notification.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's comptime `Option.Type()` is the [`OptionValue`] a read returns, whose accessors answer `None` for
//! another kind of option; the `n` and `t` options, which Zig reads as iterators, read as one here.

use crate::terminal::osc::{Command, Parser, State, Terminator, encoding, kitty_metadata};

/// The log target of this parser (Ghostty's `.kitty_desktop_notification` scope).
const LOG_SCOPE: &str = "kitty_desktop_notification";

/// The longest plain payload.
pub(crate) const MAX_PLAIN_PAYLOAD_BYTES: usize = 2048;
/// The longest base64-encoded payload.
pub(crate) const MAX_ENCODED_PAYLOAD_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Osc<'p> {
    /// The raw metadata that was received. It can be parsed by using the `read_option` method.
    pub(crate) metadata: &'p [u8],
    /// The raw payload. It may be Base64 encoded, check the `e` option.
    pub(crate) payload: &'p [u8],
    /// The terminator that was used in case we need to send a response.
    pub(crate) terminator: Terminator,
}

impl<'p> Osc<'p> {
    /// Decode an option from the metadata.
    pub(crate) fn read_option(&self, key: OptionKey) -> OptionValue<'p> {
        key.read(self.metadata)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Action {
    pub(crate) focus: bool,
    pub(crate) report: bool,
}

impl Action {
    pub(crate) const DEFAULT: Action = Action {
        focus: true,
        report: false,
    };

    /// The action list `s` (`focus,-report`); anything unknown in it gives the default.
    pub(crate) fn init(s: &[u8]) -> Self {
        parse_packed_struct(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Occasion {
    Always,
    Invisible,
    Unfocused,
}

impl Occasion {
    pub(crate) const DEFAULT: Occasion = Occasion::Always;

    /// The occasion named `s`, the default for an unknown name.
    pub(crate) fn init(s: &[u8]) -> Self {
        match s {
            b"always" => Occasion::Always,
            b"invisible" => Occasion::Invisible,
            b"unfocused" => Occasion::Unfocused,
            _ => Occasion::DEFAULT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Payload {
    Alive,
    Body,
    Buttons,
    Close,
    Icon,
    Query,
    Title,
    /// This is a special value to indicate that an unknown payload value was
    /// specified and it should be ignored.
    Unknown,
}

impl Payload {
    pub(crate) const DEFAULT: Payload = Payload::Title;

    /// The payload type named `s`. The string `query` is not allowed, it should be a single question mark if you want
    /// a query.
    pub(crate) fn init(s: &[u8]) -> Self {
        match s {
            b"?" => Self::Query,
            b"alive" => Self::Alive,
            b"body" => Self::Body,
            b"buttons" => Self::Buttons,
            b"close" => Self::Close,
            b"icon" => Self::Icon,
            b"title" => Self::Title,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Urgency {
    Low,
    Normal,
    High,
}

impl Urgency {
    pub(crate) const DEFAULT: Urgency = Urgency::Normal;

    /// The urgency digit `s`, the default for anything else.
    pub(crate) fn init(s: &[u8]) -> Self {
        match s {
            b"0" => Self::Low,
            b"1" => Self::Normal,
            b"2" => Self::High,
            _ => Self::DEFAULT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptionKey {
    /// What action(s) should be taken when a notification is clicked.
    A,
    /// Should a notification be sent to the application when the notification
    /// is closed?
    C,
    /// Are we done with the notification, and it is ready to be sent?
    D,
    /// Is the payload encoded with Base64?
    E,
    /// The name of the application that is sending the notification.
    F,
    /// Identifier for icon data. Only used when the payload is icon data.
    G,
    /// Identifier for the notification.
    I,
    /// Icon name.
    N,
    /// When to honor the notification request.
    O,
    /// Type of the payload.
    P,
    /// The sound name to play with the notification.
    S,
    /// The type of the notification.
    T,
    /// The urgency of the notification.
    U,
    /// When to auto-close the notification.
    W,
}

/// The dependent value produced by `OptionKey::read`. The active variant matches
/// the `OptionKey::Type()` of the option that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OptionValue<'a> {
    Action(Action),
    Bool(bool),
    OptBytes(Option<&'a [u8]>),
    Iterator(kitty_metadata::ValueIterator<'a>),
    Occasion(Occasion),
    Payload(Payload),
    Bytes(&'a [u8]),
    Urgency(Urgency),
    Int(i32),
}

impl<'a> OptionValue<'a> {
    /// Helper for the `.a => Action` option.; `None` for any other kind.
    pub(crate) fn as_action(&self) -> Option<Action> {
        match self {
            OptionValue::Action(v) => Some(*v),
            _ => None,
        }
    }

    /// Helper for the `bool` options (`.c`/`.d`/`.e`).; `None` for any other kind.
    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            OptionValue::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// Helper for the `?[]const u8` options (`.f`/`.g`/`.i`).; `None` for any other kind.
    pub(crate) fn as_opt_bytes(&self) -> Option<&'a [u8]> {
        match self {
            OptionValue::OptBytes(v) => *v,
            _ => None,
        }
    }

    /// Helper for the iterator options (`.n`/`.t`): consumes the value,; `None` for any other kind.
    /// yielding the `kitty_metadata.ValueIterator` Ghostty returns
    /// directly (`var it = osc.readOption(.n);`).
    pub(crate) fn into_iterator(self) -> Option<kitty_metadata::ValueIterator<'a>> {
        match self {
            OptionValue::Iterator(v) => Some(v),
            _ => None,
        }
    }

    /// Helper for the `.o => Occasion` option.; `None` for any other kind.
    pub(crate) fn as_occasion(&self) -> Option<Occasion> {
        match self {
            OptionValue::Occasion(v) => Some(*v),
            _ => None,
        }
    }

    /// Helper for the `.p => Payload` option.; `None` for any other kind.
    pub(crate) fn as_payload(&self) -> Option<Payload> {
        match self {
            OptionValue::Payload(v) => Some(*v),
            _ => None,
        }
    }

    /// Helper for the `.s => []const u8` option.; `None` for any other kind.
    pub(crate) fn as_bytes(&self) -> Option<&'a [u8]> {
        match self {
            OptionValue::Bytes(v) => Some(v),
            _ => None,
        }
    }

    /// Helper for the `.u => Urgency` option.; `None` for any other kind.
    pub(crate) fn as_urgency(&self) -> Option<Urgency> {
        match self {
            OptionValue::Urgency(v) => Some(*v),
            _ => None,
        }
    }

    /// Helper for the `.w => i32` option.; `None` for any other kind.
    pub(crate) fn as_int(&self) -> Option<i32> {
        match self {
            OptionValue::Int(v) => Some(*v),
            _ => None,
        }
    }
}

impl OptionKey {
    fn tag_name(self) -> &'static [u8] {
        match self {
            OptionKey::A => b"a",
            OptionKey::C => b"c",
            OptionKey::D => b"d",
            OptionKey::E => b"e",
            OptionKey::F => b"f",
            OptionKey::G => b"g",
            OptionKey::I => b"i",
            OptionKey::N => b"n",
            OptionKey::O => b"o",
            OptionKey::P => b"p",
            OptionKey::S => b"s",
            OptionKey::T => b"t",
            OptionKey::U => b"u",
            OptionKey::W => b"w",
        }
    }

    /// The protocol default of the option.
    pub(crate) fn default(self) -> OptionValue<'static> {
        match self {
            OptionKey::A => OptionValue::Action(Action::DEFAULT),
            OptionKey::C | OptionKey::E => OptionValue::Bool(false),
            OptionKey::D => OptionValue::Bool(true),
            OptionKey::F | OptionKey::G | OptionKey::I => OptionValue::OptBytes(None),
            // The iterator options have no default: theirs iterates over nothing.
            OptionKey::N | OptionKey::T => {
                OptionValue::Iterator(kitty_metadata::ValueIterator::init(self.tag_name(), None, b""))
            }
            OptionKey::O => OptionValue::Occasion(Occasion::DEFAULT),
            OptionKey::P => OptionValue::Payload(Payload::DEFAULT),
            OptionKey::S => OptionValue::Bytes(b"system"),
            OptionKey::U => OptionValue::Urgency(Urgency::DEFAULT),
            OptionKey::W => OptionValue::Int(-1),
        }
    }

    /// Read the option value from the raw metadata string.
    ///
    /// Unknown and malformed values are ignored. Optional values return null;
    /// all other values return the protocol default.
    pub(crate) fn read(self, metadata: &[u8]) -> OptionValue<'_> {
        let mut it =
            kitty_metadata::ValueIterator::init(self.tag_name(), Some(VALID_METADATA_VALUE_CHARACTERS), metadata);
        if matches!(self, OptionKey::T | OptionKey::N) {
            return OptionValue::Iterator(it);
        }

        let Some(value) = it.next() else {
            return self.default();
        };

        // metadata value
        match self {
            OptionKey::A => OptionValue::Action(Action::init(value)),
            OptionKey::C | OptionKey::E => OptionValue::Bool(parse_bool(value).unwrap_or(false)),
            OptionKey::D => OptionValue::Bool(parse_bool(value).unwrap_or(true)),
            OptionKey::F => OptionValue::OptBytes(Some(value)),
            OptionKey::G | OptionKey::I => OptionValue::OptBytes(parse_identifier(value)),
            OptionKey::N | OptionKey::T => self.default(),
            OptionKey::O => OptionValue::Occasion(Occasion::init(value)),
            OptionKey::P => OptionValue::Payload(Payload::init(value)),
            OptionKey::S => OptionValue::Bytes(value),
            OptionKey::U => OptionValue::Urgency(Urgency::init(value)),
            // negative values less than -1 are not allowed (the default is -1)
            OptionKey::W => OptionValue::Int(
                crate::parse_int::parse_int::<i32, 10>(value)
                    .ok()
                    .filter(|&tmp| tmp >= -1)
                    .unwrap_or(-1),
            ),
        }
    }
}

/// Parse the protocol's booleans
fn parse_bool(s: &[u8]) -> Option<bool> {
    match s {
        b"0" => Some(false),
        b"1" => Some(true),
        _ => None,
    }
}

/// This is similar to the packed struct parser used in the configs. The
/// differences are that a literal `true` or `false` value does not turn on/off
/// all the values, and the negation prefix is `-` not `no-`.
fn parse_packed_struct(s: &[u8]) -> Action {
    let mut result = Action::DEFAULT;

    // We split each value by ","
    for raw in s.split(|&b| b == b',') {
        // Determine the field we're looking for and the value. If the field is prefixed with "-" then we set the
        // value to false.
        let trimmed = trim_whitespace(raw);
        let (part, value) = trimmed.strip_prefix(b"-").map_or((trimmed, true), |part| (part, false));
        match part {
            b"focus" => result.focus = value,
            b"report" => result.report = value,
            // No field matched
            _ => return Action::DEFAULT,
        }
    }

    result
}

/// Characters that are valid in a metadata value. Including `=` is technically
/// against the spec but is needed since Base64 encoded values (with padding)
/// are valid for some options. Including `?` is technically against the spec
/// but is needed since it is a valid value for the `p` option.
const VALID_METADATA_EXTRA_CHARACTERS: &[u8] = b"/,(){}[]*&^%$#@!`~=?";
/// The identifier characters and the extra ones (`valid_identifier_characters ++ valid_metadata_extra_characters`).
const VALID_METADATA_VALUE_CHARACTERS_ARR: [u8; VALID_IDENTIFIER_CHARACTERS.len()
    + VALID_METADATA_EXTRA_CHARACTERS.len()] = {
    let mut out = [0u8; VALID_IDENTIFIER_CHARACTERS.len() + VALID_METADATA_EXTRA_CHARACTERS.len()];
    let mut i = 0;
    while i < VALID_IDENTIFIER_CHARACTERS.len() {
        out[i] = VALID_IDENTIFIER_CHARACTERS[i];
        i += 1;
    }
    let mut j = 0;
    while j < VALID_METADATA_EXTRA_CHARACTERS.len() {
        out[VALID_IDENTIFIER_CHARACTERS.len() + j] = VALID_METADATA_EXTRA_CHARACTERS[j];
        j += 1;
    }
    out
};
const VALID_METADATA_VALUE_CHARACTERS: &[u8] = &VALID_METADATA_VALUE_CHARACTERS_ARR;

/// Characters that are valid in identifiers.
const VALID_IDENTIFIER_CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_+.";

/// Whether `s` is an identifier. Unlike the OSC 5522 parser's, the empty string is one here.
fn is_valid_identifier(s: &[u8]) -> bool {
    s.iter().all(|b| VALID_IDENTIFIER_CHARACTERS.contains(b))
}

/// `s` when it is a valid identifier.
fn parse_identifier(s: &[u8]) -> Option<&[u8]> {
    is_valid_identifier(s).then_some(s)
}

/// `std.ascii.whitespace`.
const ASCII_WHITESPACE: &[u8] = &[b' ', b'\t', b'\n', b'\r', 0x0B, 0x0C];

/// The slice without leading and trailing ASCII whitespace (`std.mem.trim`).
fn trim_whitespace(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|b| !ASCII_WHITESPACE.contains(b)).unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|b| !ASCII_WHITESPACE.contains(b))
        .map_or(start, |i| i + 1);
    &s[start..end]
}

/// Parse OSC 99
pub(crate) fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert_eq!(parser.state, State::S99);

    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };
    let data = cap.trailing(&parser.buffer);

    let Some(payload_start) = data.iter().position(|&b| b == b';') else {
        tracing::warn!(target: LOG_SCOPE, "missing semicolon before payload");
        parser.state = State::Invalid;
        return None;
    };

    let metadata = &data[..payload_start];
    let payload = &data[payload_start + 1..];

    let max_payload_bytes = if OptionKey::E.read(metadata).as_bool() == Some(true) {
        MAX_ENCODED_PAYLOAD_BYTES
    } else {
        MAX_PLAIN_PAYLOAD_BYTES
    };
    if payload.len() > max_payload_bytes {
        tracing::warn!(
            target: LOG_SCOPE,
            "payload is too large: size={} max={}",
            payload.len(),
            max_payload_bytes
        );
        parser.state = State::Invalid;
        return None;
    }

    // Payload has to be an escape-code-safe UTF-8 string.
    if !encoding::is_safe_utf8(payload) {
        tracing::warn!(target: LOG_SCOPE, "payload is not escape code safe UTF-8");
        parser.state = State::Invalid;
        return None;
    }

    Some(Command::KittyDesktopNotification(Osc {
        metadata,
        payload,
        terminator: Terminator::init(terminator_ch),
    }))
}

#[cfg(test)]
mod tests;
