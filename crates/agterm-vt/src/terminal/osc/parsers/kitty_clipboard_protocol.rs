//! Kitty's clipboard protocol (OSC 5522). Specification: <https://sw.kovidgoyal.net/kitty/clipboard/>,
//! <https://rockorager.dev/misc/bracketed-paste-mime/>
//!
//! Ghostty: `src/terminal/osc/parsers/kitty_clipboard_protocol.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/kitty_clipboard_protocol.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's comptime `Option.Type()` is the [`OptionValue`] a read returns, whose accessors answer `None` for
//! another kind of option.

use crate::terminal::osc::{Command, Parser, State, Terminator};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Osc<'p> {
    /// The raw metadata that was received. It can be parsed by using the `read_option` method.
    pub(crate) metadata: &'p [u8],
    /// The raw payload. It may be Base64 encoded, check the `e` option.
    pub(crate) payload: Option<&'p [u8]>,
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
pub(crate) enum Location {
    Primary,
}

impl Location {
    pub(crate) fn init(s: &[u8]) -> Option<Location> {
        match s {
            b"primary" => Some(Location::Primary),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation {
    Read,
    Walias,
    Wdata,
    Write,
}

impl Operation {
    pub(crate) fn init(s: &[u8]) -> Option<Operation> {
        match s {
            b"read" => Some(Operation::Read),
            b"walias" => Some(Operation::Walias),
            b"wdata" => Some(Operation::Wdata),
            b"write" => Some(Operation::Write),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Data,
    Done,
    Ebusy,
    Efbig,
    Einval,
    Eio,
    Enosys,
    Eperm,
    Ok,
}

impl Status {
    pub(crate) fn init(s: &[u8]) -> Option<Status> {
        match s {
            b"DATA" => Some(Status::Data),
            b"DONE" => Some(Status::Done),
            b"EBUSY" => Some(Status::Ebusy),
            b"EFBIG" => Some(Status::Efbig),
            b"EINVAL" => Some(Status::Einval),
            b"EIO" => Some(Status::Eio),
            b"ENOSYS" => Some(Status::Enosys),
            b"EPERM" => Some(Status::Eperm),
            b"OK" => Some(Status::Ok),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptionKey {
    Id,
    Loc,
    Mime,
    Name,
    Password,
    Pw,
    Status,
    Type,
}

/// The dependent value produced by `OptionKey::read`. The active variant matches
/// the `OptionKey::Type()` of the option that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptionValue<'a> {
    Str(Option<&'a [u8]>),
    Loc(Option<Location>),
    Status(Option<Status>),
    Type(Option<Operation>),
}

impl<'a> OptionValue<'a> {
    /// The value of a `?[]const u8` option, `None` for any other kind.
    pub(crate) fn as_bytes(&self) -> Option<&'a [u8]> {
        match self {
            OptionValue::Str(v) => *v,
            _ => None,
        }
    }

    /// The value of a `?Location` option, `None` for any other kind.
    pub(crate) fn as_loc(&self) -> Option<Location> {
        match self {
            OptionValue::Loc(v) => *v,
            _ => None,
        }
    }

    /// The value of a `?Status` option, `None` for any other kind.
    pub(crate) fn as_status(&self) -> Option<Status> {
        match self {
            OptionValue::Status(v) => *v,
            _ => None,
        }
    }

    /// The value of a `?Operation` option, `None` for any other kind.
    pub(crate) fn as_type(&self) -> Option<Operation> {
        match self {
            OptionValue::Type(v) => *v,
            _ => None,
        }
    }

    /// Whether the optional option value is absent (`null`).
    pub(crate) fn is_null(&self) -> bool {
        match self {
            OptionValue::Str(v) => v.is_none(),
            OptionValue::Loc(v) => v.is_none(),
            OptionValue::Status(v) => v.is_none(),
            OptionValue::Type(v) => v.is_none(),
        }
    }
}

impl OptionKey {
    fn tag_name(self) -> &'static [u8] {
        match self {
            OptionKey::Id => b"id",
            OptionKey::Loc => b"loc",
            OptionKey::Mime => b"mime",
            OptionKey::Name => b"name",
            OptionKey::Password => b"password",
            OptionKey::Pw => b"pw",
            OptionKey::Status => b"status",
            OptionKey::Type => b"type",
        }
    }

    /// Read the option value from the raw metadata string.
    pub(crate) fn read(self, metadata: &[u8]) -> OptionValue<'_> {
        let Some(value) =
            crate::terminal::osc::kitty_metadata::ValueIterator::init(self.tag_name(), None, metadata).next()
        else {
            return self.null_value();
        };

        match self {
            OptionKey::Id => OptionValue::Str(parse_identifier(value)),
            OptionKey::Loc => OptionValue::Loc(Location::init(value)),
            OptionKey::Mime | OptionKey::Name | OptionKey::Password | OptionKey::Pw => OptionValue::Str(Some(value)),
            OptionKey::Status => OptionValue::Status(Status::init(value)),
            OptionKey::Type => OptionValue::Type(Operation::init(value)),
        }
    }

    /// The `null` / not-found value for this option, with the variant matching
    /// `self`'s dependent type.
    fn null_value(self) -> OptionValue<'static> {
        match self {
            OptionKey::Id | OptionKey::Mime | OptionKey::Name | OptionKey::Password | OptionKey::Pw => {
                OptionValue::Str(None)
            }
            OptionKey::Loc => OptionValue::Loc(None),
            OptionKey::Status => OptionValue::Status(None),
            OptionKey::Type => OptionValue::Type(None),
        }
    }
}

/// Characters that are valid in identifiers.
const VALID_IDENTIFIER_CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_+.";

/// Whether `s` is a non-empty identifier.
fn is_valid_identifier(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|b| VALID_IDENTIFIER_CHARACTERS.contains(b))
}

/// `s` when it is a valid identifier.
fn parse_identifier(s: &[u8]) -> Option<&[u8]> {
    is_valid_identifier(s).then_some(s)
}

/// Parse OSC 5522
pub(crate) fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert_eq!(parser.state, State::S5522);

    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };
    let data = cap.trailing(&parser.buffer);
    let (metadata, payload) = match data.iter().position(|&b| b == b';') {
        None => (data, None),
        Some(start) => (&data[..start], Some(&data[start + 1..])),
    };
    Some(Command::KittyClipboardProtocol(Osc {
        metadata,
        payload,
        terminator: Terminator::init(terminator_ch),
    }))
}

#[cfg(test)]
mod tests;
