//! OSC 3008: Hierarchical Context Signalling (UAPI spec). Specification:
//! <https://uapi-group.org/specifications/specs/osc_context/>
//!
//! OSC 3008 allows programs to signal context changes to the terminal emulator. Each context has an identifier and
//! metadata fields. Contexts are hierarchical and form a stack.
//!
//! Ghostty: `src/terminal/osc/parsers/context_signal.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/context_signal.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's comptime `Field.Type()` is the [`FieldValue`] a read returns, whose accessors answer `None` for
//! another kind of field. Drift absorbed: `46d6c3b50` (string fields are [`UnescapedString`]s, decoding `\x3b` and
//! `\x5c`).

use crate::mem::alloc::{OutOfMemory, TryVecExt as _};
use crate::terminal::osc::{Command as OscCommand, Parser, State};

/// The log target of this parser (Ghostty's `.osc_context_signal` scope).
const LOG_SCOPE: &str = "osc_context_signal";

/// A string from a context metadata field that may contain `\x3b` and `\x5c` escape sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UnescapedString<'a> {
    /// The field's bytes, escapes undecoded.
    pub(crate) raw: &'a [u8],
}

/// An iterator that decodes `\x3b` (semicolon) and `\x5c` (backslash) escape sequences. Yields one unescaped byte at
/// a time.
#[derive(Debug, Clone)]
pub(crate) struct Unescape<'a> {
    raw: &'a [u8],
    index: usize,
}

impl Iterator for Unescape<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        let rest = self.raw.get(self.index..)?;
        // An escape is four bytes, all of which must be there (Zig's `index + 3 < len`).
        if rest.len() >= 4 {
            let decoded = match &rest[..4] {
                b"\\x3b" => Some(b';'),
                b"\\x5c" => Some(b'\\'),
                _ => None,
            };
            if let Some(c) = decoded {
                self.index += 4;
                return Some(c);
            }
        }
        let c = *rest.first()?;
        self.index += 1;
        Some(c)
    }
}

impl<'a> UnescapedString<'a> {
    /// The unescaped bytes.
    pub(crate) fn iter(&self) -> Unescape<'a> {
        Unescape {
            raw: self.raw,
            index: 0,
        }
    }

    /// A new buffer with the unescaped content (`toAllocated`).
    pub(crate) fn to_vec(self) -> Result<Vec<u8>, OutOfMemory> {
        let mut list = Vec::new();
        for c in self.iter() {
            list.try_push(c)?;
        }
        Ok(list)
    }

    /// Returns true if the unescaped string matches `other`.
    pub(crate) fn eql(&self, other: &[u8]) -> bool {
        self.iter().eq(other.iter().copied())
    }
}

/// Maximum length of a context identifier (per spec).
const MAX_CONTEXT_ID_LEN: usize = 64;

/// A single OSC 3008 context signal command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Command<'p> {
    pub(crate) action: Action,
    /// The context identifier. Must be 1-64 characters in the 32..126 byte range.
    pub(crate) id: &'p [u8],
    /// Raw unparsed metadata fields after the context ID.
    /// Fields are semicolon-separated key=value pairs.
    /// Parsed lazily via [`Command::read_option`].
    pub(crate) metadata: &'p [u8],
}

impl<'p> Command<'p> {
    /// Read a metadata field value from the raw fields string.
    /// Returns null if the field is not present or malformed.
    pub(crate) fn read_option(&self, option: Field) -> FieldValue<'p> {
        option.read(self.metadata)
    }
}

/// Context types defined by the specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextType {
    Boot,
    Container,
    Vm,
    Elevate,
    Chpriv,
    Subcontext,
    Remote,
    Shell,
    Command,
    App,
    Service,
    Session,
}

impl ContextType {
    pub(crate) fn parse(value: &[u8]) -> Option<ContextType> {
        match value {
            b"boot" => Some(ContextType::Boot),
            b"container" => Some(ContextType::Container),
            b"vm" => Some(ContextType::Vm),
            b"elevate" => Some(ContextType::Elevate),
            b"chpriv" => Some(ContextType::Chpriv),
            b"subcontext" => Some(ContextType::Subcontext),
            b"remote" => Some(ContextType::Remote),
            b"shell" => Some(ContextType::Shell),
            b"command" => Some(ContextType::Command),
            b"app" => Some(ContextType::App),
            b"service" => Some(ContextType::Service),
            b"session" => Some(ContextType::Session),
            _ => None,
        }
    }
}

/// Exit status for the `exit` end-sequence field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitStatus {
    Success,
    Failure,
    Crash,
    Interrupt,
}

impl ExitStatus {
    pub(crate) fn parse(value: &[u8]) -> Option<ExitStatus> {
        match value {
            b"success" => Some(ExitStatus::Success),
            b"failure" => Some(ExitStatus::Failure),
            b"crash" => Some(ExitStatus::Crash),
            b"interrupt" => Some(ExitStatus::Interrupt),
            _ => None,
        }
    }
}

/// Metadata fields that can appear in OSC 3008 sequences.
/// Fields are read lazily from the raw string using the `read` method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    // Start sequence fields
    Type,
    User,
    Hostname,
    Machineid,
    Bootid,
    Pid,
    Pidfdid,
    Comm,
    Cwd,
    Cmdline,
    Vm,
    Container,
    Targetuser,
    Targethost,
    Sessionid,

    // End sequence fields
    Exit,
    Status,
    Signal,
}

/// The value `Field::read` produces: the kind of the field that produced it (Zig's `Field.Type()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldValue<'a> {
    Type(Option<ContextType>),
    Exit(Option<ExitStatus>),
    Int(Option<u64>),
    Str(Option<UnescapedString<'a>>),
}

impl<'a> FieldValue<'a> {
    /// The value of a `?ContextType` field, `None` for any other kind.
    pub(crate) fn as_type(&self) -> Option<ContextType> {
        match self {
            Self::Type(v) => *v,
            _ => None,
        }
    }

    /// The value of a `?ExitStatus` field, `None` for any other kind.
    pub(crate) fn as_exit(&self) -> Option<ExitStatus> {
        match self {
            Self::Exit(v) => *v,
            _ => None,
        }
    }

    /// The value of a `?u64` field, `None` for any other kind.
    pub(crate) fn as_int(&self) -> Option<u64> {
        match self {
            Self::Int(v) => *v,
            _ => None,
        }
    }

    /// The value of a string field, `None` for any other kind.
    pub(crate) fn as_str(&self) -> Option<UnescapedString<'a>> {
        match self {
            Self::Str(v) => *v,
            _ => None,
        }
    }

    /// Whether the optional field value is absent (`null`).
    pub(crate) fn is_null(&self) -> bool {
        match self {
            Self::Type(v) => v.is_none(),
            Self::Exit(v) => v.is_none(),
            Self::Int(v) => v.is_none(),
            Self::Str(v) => v.is_none(),
        }
    }
}

impl Field {
    fn key(self) -> &'static [u8] {
        match self {
            Field::Type => b"type",
            Field::User => b"user",
            Field::Hostname => b"hostname",
            Field::Machineid => b"machineid",
            Field::Bootid => b"bootid",
            Field::Pid => b"pid",
            Field::Pidfdid => b"pidfdid",
            Field::Comm => b"comm",
            Field::Cwd => b"cwd",
            Field::Cmdline => b"cmdline",
            Field::Vm => b"vm",
            Field::Container => b"container",
            Field::Targetuser => b"targetuser",
            Field::Targethost => b"targethost",
            Field::Sessionid => b"sessionid",
            Field::Exit => b"exit",
            Field::Status => b"status",
            Field::Signal => b"signal",
        }
    }

    /// Read the field value from the raw fields string.
    ///
    /// The raw fields string contains semicolon-separated key=value pairs
    /// e.g. "type=container;user=lennart;hostname=zeta".
    ///
    /// Unknown or malformed fields are ignored per the specification.
    pub(crate) fn read(self, raw: &[u8]) -> FieldValue<'_> {
        for full in raw.split(|&b| b == b';') {
            // Parse key=value
            let Some(eql_idx) = full.iter().position(|&b| b == b'=') else {
                continue;
            };
            if full[..eql_idx] != *self.key() {
                continue;
            }
            let value = &full[eql_idx + 1..];

            return match self {
                Field::Type => FieldValue::Type(ContextType::parse(value)),
                Field::Exit => FieldValue::Exit(ExitStatus::parse(value)),
                Field::Pid | Field::Pidfdid | Field::Status => {
                    FieldValue::Int(crate::parse_int::parse_int::<u64, 10>(value).ok())
                }
                // String fields
                Field::User
                | Field::Hostname
                | Field::Machineid
                | Field::Bootid
                | Field::Comm
                | Field::Cwd
                | Field::Cmdline
                | Field::Vm
                | Field::Container
                | Field::Targetuser
                | Field::Targethost
                | Field::Sessionid
                | Field::Signal => FieldValue::Str((!value.is_empty()).then_some(UnescapedString { raw: value })),
            };
        }

        // Not found
        match self {
            Field::Type => FieldValue::Type(None),
            Field::Exit => FieldValue::Exit(None),
            Field::Pid | Field::Pidfdid | Field::Status => FieldValue::Int(None),
            _ => FieldValue::Str(None),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// OSC 3008;start=<id> — initiates, updates, or returns to a context.
    Start,
    /// OSC 3008;end=<id> — terminates a context.
    End,
}

/// Parse OSC 3008: hierarchical context signalling.
///
/// Expected data format (after "3008;" prefix has been consumed by the state machine):
///   start=<id>[;<field>=<value>]*
///   end=<id>[;<field>=<value>]*
pub(crate) fn parse(parser: &mut Parser, _: Option<u8>) -> Option<OscCommand<'_>> {
    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };
    let data = cap.trailing(&parser.buffer);
    if data.is_empty() {
        parser.state = State::Invalid;
        return None;
    }

    // Determine the action (start= or end=)
    // Determine the action (start= or end=), and skip past its prefix
    let (action, rest) = if let Some(rest) = data.strip_prefix(b"start=") {
        (Action::Start, rest)
    } else if let Some(rest) = data.strip_prefix(b"end=") {
        (Action::End, rest)
    } else {
        tracing::warn!(
            target: LOG_SCOPE,
            "OSC 3008: expected 'start=' or 'end=' prefix, got: {}",
            String::from_utf8_lossy(&data[..data.len().min(10)])
        );
        parser.state = State::Invalid;
        return None;
    };

    if rest.is_empty() {
        tracing::warn!(target: LOG_SCOPE, "OSC 3008: missing context ID");
        parser.state = State::Invalid;
        return None;
    }

    // Extract the context ID (up to the first semicolon or end of data)
    let id_end = rest.iter().position(|&b| b == b';').unwrap_or(rest.len());
    let id = &rest[0..id_end];

    // Validate context ID length (1-64 chars per spec)
    if id.is_empty() || id.len() > MAX_CONTEXT_ID_LEN {
        tracing::warn!(
            target: LOG_SCOPE,
            "OSC 3008: context ID length {} out of range (1-{})",
            id.len(),
            MAX_CONTEXT_ID_LEN
        );
        parser.state = State::Invalid;
        return None;
    }

    // Validate context ID characters (32..126 byte range per spec)
    for &c in id {
        if !(0x20..=0x7e).contains(&c) {
            tracing::warn!(
                target: LOG_SCOPE,
                "OSC 3008: invalid character 0x{c:0>2x} in context ID"
            );
            parser.state = State::Invalid;
            return None;
        }
    }

    // Extract raw metadata fields (everything after the ID)
    let metadata = rest.get(id_end + 1..).unwrap_or(b"");

    Some(OscCommand::ContextSignal(Command { action, id, metadata }))
}

#[cfg(test)]
mod tests;
