//! APC (Application Program Command) handling: identifying the protocol of an APC string, the Kitty graphics command
//! parser, and the bounded capture of unknown APCs.
//!
//! Ghostty: `src/terminal/apc.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/apc/mod.rs` at `RUSTTY_REV` (MIT).
//! Changes: the glyph protocol (`apc/glyph`) is not here: its identifier (`25a1`) is still recognized and its APCs
//! ignored, as Ghostty does when it is built without the protocol. Kitty graphics is always built (libghostty-vt
//! builds it). The parser is the command parser module itself (`kitty::graphics_command`), not the `kitty/graphics`
//! aggregate. Buffers are `Vec`s grown fallibly; there is no allocator argument. The per-protocol switch (`enabled`,
//! `enable`) went with the glyph protocol, the only one it was ever turned off for, and `maxDefaultBytes` with the
//! snapshot codec, its only user.

use crate::mem::alloc::TryVecExt as _;
use crate::terminal::kitty::graphics_command;

/// The log target of this module (Ghostty's `.terminal_apc` scope).
const LOG_SCOPE: &str = "terminal_apc";

/// The APC identifier of the glyph protocol, whose APCs are ignored.
const GLYPH_IDENTIFIER: &[u8; 4] = b"25a1";

/// APC command handler. This should be hooked into a terminal stream handler. The start/feed/end functions are meant
/// to be called from the stream's `apc_start`, `apc_put`, and `apc_end` functions, respectively.
pub(crate) struct Handler {
    state: State,

    /// Maximum content bytes retained for unsupported APC identifiers. Zero drops and ignores unknown APC values.
    pub(crate) unknown_max_bytes: usize,

    /// Maximum bytes each APC protocol can buffer. This is to prevent malicious input from causing us to allocate too
    /// much memory.
    pub(crate) max_bytes: [usize; Protocol::COUNT],
}

impl Default for Handler {
    fn default() -> Self {
        Self {
            state: State::Inactive,
            unknown_max_bytes: 0,
            max_bytes: [Protocol::Kitty.default_max_bytes()],
        }
    }
}

impl Handler {
    /// Begin an APC.
    pub(crate) fn start(&mut self) {
        self.state = State::Identify(Identify::default());
    }

    /// Feed one byte of the APC.
    pub(crate) fn feed(&mut self, byte: u8) {
        match &mut self.state {
            // We're ignoring this APC command, likely because we don't recognize it so there is no need to store the
            // data in memory. (Inactive is unreachable: the stream starts every APC first.)
            State::Inactive | State::Ignore => {}

            // Unsupported APC content is retained only when enabled.
            State::Unknown(unknown) => unknown.append(&[byte]),

            // We identify the APC command by the first byte.
            State::Identify(id) => {
                let id = *id;
                self.identify(id, byte);
            }

            State::Kitty(p) => {
                if let Err(err) = p.feed(byte) {
                    tracing::warn!(target: LOG_SCOPE, "kitty graphics protocol error: {}", error_name(err));
                    self.state = State::Ignore;
                }
            }
        }
    }

    /// One byte of an APC whose protocol is not identified yet, `id` holding the bytes so far.
    fn identify(&mut self, mut id: Identify, byte: u8) {
        // Kitty graphics is detected immediately on the `G` byte, since commands begin immediately after with no
        // termination character after the 'G'.
        if id.len == 0 && byte == b'G' {
            self.state = State::Kitty(Box::new(graphics_command::Parser::new(
                self.max_bytes[Protocol::Kitty as usize],
            )));
            return;
        }

        // If we hit `;` then identify...
        if byte == b';' {
            if id.prefix() == GLYPH_IDENTIFIER {
                self.state = State::Ignore;
            } else {
                self.begin_unknown(id.prefix(), &[byte]);
            }
            return;
        }

        // If we're out of identification space, the identifier is unsupported.
        let expected_idx = usize::from(id.len);
        if expected_idx >= id.buf.len() {
            self.begin_unknown(id.prefix(), &[byte]);
            return;
        }

        id.buf[expected_idx] = byte;
        id.len += 1;
        self.state = State::Identify(id);

        // Once the buffered input is no longer a prefix of a known protocol, it is an unsupported identifier.
        if self.unknown_max_bytes > 0 && byte != GLYPH_IDENTIFIER[expected_idx] {
            self.begin_unknown(id.prefix(), &[]);
        }
    }

    /// Transition from protocol identification to bounded unknown capture.
    fn begin_unknown(&mut self, prefix: &[u8], suffix: &[u8]) {
        let max_bytes = self.unknown_max_bytes;
        if max_bytes == 0 {
            self.state = State::Ignore;
            return;
        }

        let mut unknown = UnknownBuilder::new(max_bytes);
        unknown.append(prefix);
        unknown.append(suffix);
        self.state = State::Unknown(unknown);
    }

    /// Feed a slice of bytes to the handler. This is equivalent to calling feed for each byte in order, but protocol
    /// payload bytes are passed through in bulk so large payloads (e.g. Kitty graphics images) avoid per-byte dispatch
    /// overhead.
    pub(crate) fn feed_slice(&mut self, bytes: &[u8]) {
        let mut rem = bytes;
        while let Some((&first, rest)) = rem.split_first() {
            match &mut self.state {
                // We're ignoring this APC command; drop the whole slice.
                State::Inactive | State::Ignore => return,

                // We're capturing an unknown APC command, so store it.
                State::Unknown(unknown) => {
                    unknown.append(rem);
                    return;
                }

                // Identification consumes at most a few bytes; step through them one at a time until the state
                // changes.
                State::Identify(_) => {
                    self.feed(first);
                    rem = rest;
                }

                State::Kitty(p) => {
                    if let Err(err) = p.feed_slice(rem) {
                        tracing::warn!(target: LOG_SCOPE, "kitty graphics protocol error: {}", error_name(err));
                        self.state = State::Ignore;
                    }
                    return;
                }
            }
        }
    }

    /// Complete the current APC, returning its command.
    pub(crate) fn end(&mut self) -> Option<Command> {
        match core::mem::replace(&mut self.state, State::Inactive) {
            State::Inactive | State::Ignore | State::Identify(_) => None,
            State::Unknown(unknown) => Some(Command::Unknown(unknown.into_unknown())),
            State::Kitty(mut p) => match p.complete() {
                Ok(command) => Some(Command::Kitty(command)),
                Err(err) => {
                    tracing::warn!(target: LOG_SCOPE, "kitty graphics protocol error: {}", error_name(err));
                    None
                }
            },
        }
    }
}

/// The Zig error's name, as `{}` prints it.
fn error_name(err: graphics_command::Error) -> &'static str {
    match err {
        graphics_command::Error::InvalidFormat => "error.InvalidFormat",
        graphics_command::Error::Overflow => "error.Overflow",
        graphics_command::Error::OutOfMemory => "error.OutOfMemory",
        graphics_command::Error::InvalidData => "error.InvalidData",
    }
}

/// The handler's state.
pub(crate) enum State {
    /// We're not in the middle of an APC command yet.
    Inactive,

    /// We got an unrecognized APC sequence or the APC sequence we recognized became invalid. We're just dropping
    /// bytes.
    Ignore,

    /// We're waiting to identify the APC sequence. The way this is done is pretty fluid depending on supported APC
    /// protocols, but for now our rule is:
    ///
    ///  * 'G' - immediate transition to Kitty graphics protocol
    ///  * Buffer up to `;` and the bytes before dictate the protocol. If we overflow then we're immediately invalid
    ///    because we don't support anything longer than this.
    Identify(Identify),

    /// Kitty graphics protocol
    Kitty(Box<graphics_command::Parser>),

    /// An unsupported APC retained for the optional unknown callback.
    Unknown(UnknownBuilder),
}

/// The identify-phase buffer that accumulates the protocol prefix up to `;`.
#[derive(Clone, Copy, Default)]
pub(crate) struct Identify {
    len: u8,
    buf: [u8; GLYPH_IDENTIFIER.len()],
}

impl Identify {
    /// The bytes buffered so far.
    fn prefix(&self) -> &[u8] {
        &self.buf[..usize::from(self.len)]
    }
}

/// `UnknownBuilder` is responsible for accumulating bytes for an unidentified APC command if unknown capture is
/// enabled.
pub(crate) struct UnknownBuilder {
    data: Vec<u8>,
    max_bytes: usize,
    truncated: bool,
}

impl UnknownBuilder {
    /// An empty capture of at most `max_bytes`.
    fn new(max_bytes: usize) -> Self {
        Self {
            data: Vec::new(),
            max_bytes,
            truncated: false,
        }
    }

    /// Append some bytes to the unknown capture. This flags as truncated if allocation fails or we reach our byte
    /// limit, therefore it can't fail.
    fn append(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let current = self.data.len();

        // Determine how many bytes we can store in this append and if it is less than our input, then we have to note
        // we're truncating.
        let retained = bytes.len().min(self.max_bytes.saturating_sub(current));
        if retained < bytes.len() {
            self.truncated = true;
        }

        // If we require more bytes than our capacity allows then we need to grow.
        let required = current + retained;
        if required > self.data.capacity() {
            let capacity = self
                .max_bytes
                .min(required.max(self.data.capacity().saturating_mul(2).max(1)));
            if self.data.try_ensure_total_capacity(capacity).is_err() {
                self.truncated = true;
                return;
            }
        }

        self.data.extend_from_slice(&bytes[..retained]);
    }

    /// The capture as an [`Unknown`], which owns its bytes.
    fn into_unknown(self) -> Unknown {
        Unknown {
            content: self.data,
            truncated: self.truncated,
        }
    }
}

/// An unsupported APC returned by `Handler::end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unknown {
    pub(crate) content: Vec<u8>,
    pub(crate) truncated: bool,
}

/// Possible APC command types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Protocol {
    Kitty = 0,
}

impl Protocol {
    /// Number of protocols; sizes the per-protocol tables.
    pub(crate) const COUNT: usize = 1;

    /// Returns the default maximum bytes for the given protocol.
    pub(crate) const fn default_max_bytes(self) -> usize {
        match self {
            // Kitty graphics payloads can be very large (e.g. full images encoded as base64), so the default is set
            // to 65 MiB.
            Self::Kitty => 65 * 1024 * 1024,
        }
    }
}

/// A recognized or unsupported APC command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    Kitty(graphics_command::Command),
    Unknown(Unknown),
}

#[cfg(test)]
mod tests;
