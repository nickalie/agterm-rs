//! The wire between the session server and its clients: frames, the stable control layer every version reads, and the
//! version-specific messages of each connection role.
//!
//! herdr: `src/protocol/wire.rs` at `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified.
//! Kept: the length-prefixed framing with its guards (the length checked against the limit before anything is
//! allocated, partial reads reassembled, trailing bytes refused), `FramingError`, the exact-match version check and
//! its older/newer refusals, and the hello/welcome handshake. Changed: postcard in place of bincode (whose last
//! release is a `compile_error!` tombstone), a fixed header with a magic and the protocol version on every frame, a
//! stable JSON control layer (after the shape of herdr's stable endpoint contract, `src/protocol/endpoint.rs`)
//! carrying the handshake and the update handoff's request, and agterm's own messages: the api `SessionHost` needs
//! and the raw attach stream. herdr's TUI messages (frames, surfaces, notifications, the client shell) are not here.
//!
//! # Frames
//!
//! Every frame, in both directions and for the whole life of a connection, is
//!
//! ```text
//! magic "AGLS" | protocol u32 LE | kind u32 LE | length u32 LE | payload[length]
//! ```
//!
//! The header's layout, its byte order and the kind numbers are frozen: every version reads them.
//!
//! - [`FrameKind::Control`]: the payload is one JSON object, a [`Control`] operation, at most [`MAX_CONTROL_SIZE`]
//!   bytes. Its schemas are frozen too: an operation keeps its name and its fields' meaning forever, a new field is
//!   optional, unknown fields are ignored, and a changed shape takes a new operation name.
//! - [`FrameKind::Message`]: the payload is postcard of the connection role's message type ([`ApiCall`] and
//!   [`ApiReply`], [`AttachClientMessage`] and [`AttachServerMessage`]), at most [`MAX_FRAME_SIZE`] bytes. Those types
//!   belong to one [`PROTOCOL_VERSION`] and change with it, so a message frame is read only when its protocol is the
//!   one the connection negotiated.
//! - [`FrameKind::MessagePart`]: the first bytes of a message whose postcard is longer than one frame. A message of
//!   `n` bytes, `MAX_FRAME_SIZE < n <= MAX_MESSAGE_SIZE` ([`MAX_MESSAGE_SIZE`]), is sent as parts of exactly
//!   [`MAX_FRAME_SIZE`] bytes each, then one [`FrameKind::Message`] frame with the remaining 1 to [`MAX_FRAME_SIZE`]
//!   bytes; a message that fits one frame is never split. The frames of one message are consecutive, of one protocol,
//!   and written as one unit ([`write_message`]); [`read_frame`] joins them into one message frame. So every message
//!   of every role (a whole scrollback's screen, a long paste, a clipboard) travels whole, and each frame's
//!   allocation stays bounded by its own checked length.
//!
//! A frame with a bad magic, an unknown kind or an oversized length is a broken peer: the connection closes without
//! a reply. So is a message whose parts break the rules above or add up past [`MAX_MESSAGE_SIZE`]: the reader
//! refuses it as a framing error, never as an error of its role (a sender whose reply is too large finds out from
//! [`encode_message`] first, and answers with its role's error instead). A newer version adds kinds only behind a
//! hello that names them, so an older server never sees one from a well-behaved client.
//!
//! # Connections
//!
//! One listener per namespace; a client opens a connection per role and starts it with [`Control::Hello`]:
//!
//! - [`Role::Api`] and [`Role::Attach`] need the client's protocol to equal the server's ([`welcome`]). On a match
//!   the server answers with a [`Welcome`] without error and both sides exchange message frames; otherwise the
//!   welcome carries a [`ControlError`] (`protocol_mismatch`) and the server closes.
//! - [`Role::Control`] is accepted whatever the client's protocol, so a new build can ask a server of an older one to
//!   hand its sessions over: `hello(control) → welcome → handoff.request.v1 → handoff.result.v1`. A welcome always
//!   carries the server's build id and capabilities, a refusal's included; [`CAPABILITY_HANDOFF_REQUEST`] says the
//!   server takes the request.
//!
//! A [`HandoffResult`] that is `ok` means the handoff committed; a failure means it failed before its commit. A
//! connection that ends before the result leaves the outcome unknown: the client reconnects and reads the server's
//! build before it retries or offers the reset.

use std::fmt;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// The first four bytes of every frame.
pub const MAGIC: [u8; 4] = *b"AGLS";

/// The protocol of this build's message frames. Bumped whenever a message type's postcard layout changes (2: the reply
/// owner's [`AttachClientMessage::Reply`] and [`AttachServerMessage::ReplyOwner`], the colour scheme in
/// [`ReplyMetadata`], the list-only clipboard read).
pub const PROTOCOL_VERSION: u32 = 2;

/// The size of a frame's header: magic, protocol, kind, length.
pub const HEADER_LEN: usize = 16;

/// The largest payload of a message frame or a message part (herdr's limit). A longer length is refused before
/// anything is allocated.
pub const MAX_FRAME_SIZE: usize = 2 * 1024 * 1024;

/// The largest message: its parts' payloads together (module docs, [`FrameKind::MessagePart`]). An operational
/// ceiling on encoded bytes, checked while a message is encoded and before each part's payload is read; a screen past
/// it (an unlimited scrollback can hold more) is refused as [`FramingError::Oversized`], never cut.
pub const MAX_MESSAGE_SIZE: usize = 256 * 1024 * 1024;

/// The largest payload of a control frame, checked before anything is allocated.
pub const MAX_CONTROL_SIZE: usize = 64 * 1024;

/// The capability of a server that takes [`Control::HandoffRequest`].
pub const CAPABILITY_HANDOFF_REQUEST: &str = "handoff.request.v1";

/// What a frame's payload is. The numbers are frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// One JSON [`Control`] operation.
    Control,
    /// Postcard of the connection role's message type, or the last part of a longer one.
    Message,
    /// Exactly [`MAX_FRAME_SIZE`] bytes of a longer message, which a [`FrameKind::Message`] frame ends. [`read_frame`]
    /// joins the parts, so a [`Frame`] it returns never has this kind.
    MessagePart,
}

impl FrameKind {
    const CONTROL: u32 = 1;
    const MESSAGE: u32 = 2;
    const MESSAGE_PART: u32 = 3;

    fn number(self) -> u32 {
        match self {
            Self::Control => Self::CONTROL,
            Self::Message => Self::MESSAGE,
            Self::MessagePart => Self::MESSAGE_PART,
        }
    }

    fn from_number(number: u32) -> Option<Self> {
        match number {
            Self::CONTROL => Some(Self::Control),
            Self::MESSAGE => Some(Self::Message),
            Self::MESSAGE_PART => Some(Self::MessagePart),
            _ => None,
        }
    }

    fn limit(self) -> usize {
        match self {
            Self::Control => MAX_CONTROL_SIZE,
            Self::Message | Self::MessagePart => MAX_FRAME_SIZE,
        }
    }
}

/// Why a frame could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum FramingError {
    /// The peer closed the connection between two frames.
    #[error("connection closed")]
    Closed,
    /// The connection ended inside a frame.
    #[error("unexpected end of stream inside a frame")]
    UnexpectedEof,
    /// The frame does not start with [`MAGIC`].
    #[error("not an agterm-session frame (magic {0:02x?})")]
    BadMagic([u8; 4]),
    /// The header names a kind this version does not know.
    #[error("unknown frame kind {0}")]
    UnknownKind(u32),
    /// A payload exceeds its limit: a frame's that of its kind ([`MAX_CONTROL_SIZE`], [`MAX_FRAME_SIZE`]), a
    /// message's [`MAX_MESSAGE_SIZE`].
    #[error("frame size {claimed} exceeds maximum {max}")]
    Oversized { claimed: usize, max: usize },
    /// The frame is of the other kind than the reader expected.
    #[error("expected a {expected:?} frame, got a {got:?} frame")]
    WrongKind { expected: FrameKind, got: FrameKind },
    /// A message frame of another protocol than the connection negotiated.
    #[error("message frame of protocol {got} on a connection of protocol {negotiated}")]
    ProtocolChanged { negotiated: u32, got: u32 },
    /// The payload did not decode, or decoded to fewer bytes than its length; a message's parts broke their rules; or
    /// a value to send did not serialize.
    #[error("malformed payload: {0}")]
    Malformed(String),
    /// A control operation this side cannot send ([`Control::Unknown`]).
    #[error("{0}")]
    Unencodable(String),
    /// Reading or writing failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// One frame as read: its header's protocol and kind, and the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The sender's protocol.
    pub protocol: u32,
    pub kind: FrameKind,
    pub payload: Vec<u8>,
}

impl Frame {
    /// The control operation the frame carries.
    ///
    /// # Errors
    ///
    /// The frame is a message frame, or its payload is not one control operation (trailing whitespace aside).
    pub fn control(&self) -> Result<Control, FramingError> {
        self.expect_kind(FrameKind::Control)?;
        serde_json::from_slice(&self.payload).map_err(|error| FramingError::Malformed(error.to_string()))
    }

    /// The message the frame carries, on a connection that negotiated `negotiated`.
    ///
    /// # Errors
    ///
    /// The frame is a control frame or of another protocol, its payload does not decode as `M`, or bytes are left over.
    pub fn message<M: for<'de> Deserialize<'de>>(&self, negotiated: u32) -> Result<M, FramingError> {
        self.expect_kind(FrameKind::Message)?;
        if self.protocol != negotiated {
            return Err(FramingError::ProtocolChanged {
                negotiated,
                got: self.protocol,
            });
        }
        let (message, rest) =
            postcard::take_from_bytes(&self.payload).map_err(|error| FramingError::Malformed(error.to_string()))?;
        if !rest.is_empty() {
            return Err(FramingError::Malformed(format!(
                "decoded {} bytes but payload length was {}; trailing bytes are not allowed",
                self.payload.len() - rest.len(),
                self.payload.len()
            )));
        }
        Ok(message)
    }

    fn expect_kind(&self, expected: FrameKind) -> Result<(), FramingError> {
        if self.kind == expected {
            Ok(())
        } else {
            Err(FramingError::WrongKind {
                expected,
                got: self.kind,
            })
        }
    }
}

/// `control` as one frame of this build's protocol. A control operation is never split into parts.
///
/// # Errors
///
/// [`FramingError::Unencodable`] for [`Control::Unknown`], which only a reader produces; [`FramingError::Oversized`]
/// when its JSON exceeds [`MAX_CONTROL_SIZE`] (a long capability list, a long `import_exe`);
/// [`FramingError::Malformed`] when it does not serialize.
pub fn encode_control(control: &Control) -> Result<Vec<u8>, FramingError> {
    if matches!(control, Control::Unknown) {
        return Err(FramingError::Unencodable(
            "an unknown control operation cannot be sent".to_owned(),
        ));
    }
    let mut frame = header_placeholder();
    serde_json::to_writer(&mut frame, control).map_err(|error| FramingError::Malformed(error.to_string()))?;
    finish_frame(frame, FrameKind::Control)
}

/// `message` as the frames of this build's protocol that carry it: one message frame, or for a postcard longer than
/// [`MAX_FRAME_SIZE`] its parts and the closing message frame (module docs). The frames are laid out while the message
/// serializes, and nothing past [`MAX_MESSAGE_SIZE`] is stored.
///
/// # Errors
///
/// [`FramingError::Malformed`] when the message does not serialize; [`FramingError::Oversized`] when its postcard
/// exceeds [`MAX_MESSAGE_SIZE`], with its whole length as `claimed`.
pub fn encode_message<M: Serialize>(message: &M) -> Result<Vec<u8>, FramingError> {
    postcard::serialize_with_flavor(message, FrameSink::new())
        .map_err(|error| FramingError::Malformed(error.to_string()))?
        .finish()
}

/// Writes `control` as one frame and flushes.
///
/// # Errors
///
/// As [`encode_control`] (nothing is written then), or the write failed.
pub fn write_control<W: Write>(writer: &mut W, control: &Control) -> Result<(), FramingError> {
    write_frame_bytes(writer, &encode_control(control)?)
}

/// Writes `message`'s frames ([`encode_message`]) with one `write_all` and flushes. Writers that share a stream
/// serialize their calls, so a message's parts are never interleaved with another frame.
///
/// # Errors
///
/// As [`encode_message`] (nothing is written then), or the write failed.
pub fn write_message<W: Write, M: Serialize>(writer: &mut W, message: &M) -> Result<(), FramingError> {
    write_frame_bytes(writer, &encode_message(message)?)
}

/// Reads one frame, reassembling partial reads, and joins a message's parts into one message frame. Every header is
/// checked (magic, kind, the kind's limit, a part's exact length, the message's running total against
/// [`MAX_MESSAGE_SIZE`]) before its payload is allocated.
///
/// # Errors
///
/// [`FramingError::Closed`] at a clean end between frames ([`FramingError::UnexpectedEof`] between a message's
/// parts); [`FramingError::WrongKind`] for a control frame among a message's parts, [`FramingError::Malformed`] for a
/// part of another length or protocol; otherwise any other [`FramingError`] but the encoding ones.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Frame, FramingError> {
    let mut header = [0_u8; HEADER_LEN];
    if !read_header(reader, &mut header)? {
        return Err(FramingError::Closed);
    }
    let (protocol, kind, length) = parse_header(&header)?;
    if kind == FrameKind::MessagePart {
        check_part_length(length)?;
    }
    let mut payload = Vec::new();
    read_payload(reader, &mut payload, length)?;
    let kind = if kind == FrameKind::MessagePart {
        read_message_parts(reader, protocol, &mut payload)?;
        FrameKind::Message
    } else {
        kind
    };
    Ok(Frame {
        protocol,
        kind,
        payload,
    })
}

/// Reads the frames after a message's first part through the closing message frame, appending their payloads.
fn read_message_parts<R: Read>(reader: &mut R, protocol: u32, payload: &mut Vec<u8>) -> Result<(), FramingError> {
    loop {
        let mut header = [0_u8; HEADER_LEN];
        if !read_header(reader, &mut header)? {
            return Err(FramingError::UnexpectedEof);
        }
        let (part_protocol, kind, length) = parse_header(&header)?;
        check_next_part(protocol, payload.len(), part_protocol, kind, length)?;
        read_payload(reader, payload, length)?;
        if kind == FrameKind::Message {
            return Ok(());
        }
    }
}

/// Whether a frame of `protocol`, `kind` and `length` may follow `joined` bytes of a message of `message_protocol`.
fn check_next_part(
    message_protocol: u32,
    joined: usize,
    protocol: u32,
    kind: FrameKind,
    length: usize,
) -> Result<(), FramingError> {
    if kind == FrameKind::Control {
        return Err(FramingError::WrongKind {
            expected: FrameKind::Message,
            got: kind,
        });
    }
    if protocol != message_protocol {
        return Err(FramingError::Malformed(format!(
            "a part of protocol {protocol} in a message of protocol {message_protocol}"
        )));
    }
    let claimed = joined.saturating_add(length);
    if claimed > MAX_MESSAGE_SIZE {
        return Err(FramingError::Oversized {
            claimed,
            max: MAX_MESSAGE_SIZE,
        });
    }
    match kind {
        FrameKind::MessagePart => check_part_length(length),
        _ if length == 0 => Err(FramingError::Malformed(
            "an empty message frame after a message's parts".to_owned(),
        )),
        _ => Ok(()),
    }
}

fn check_part_length(length: usize) -> Result<(), FramingError> {
    if length == MAX_FRAME_SIZE {
        return Ok(());
    }
    Err(FramingError::Malformed(format!(
        "a message part of {length} bytes; every part holds {MAX_FRAME_SIZE}"
    )))
}

/// Appends `length` bytes from `reader` to `payload`, read in place.
fn read_payload<R: Read>(reader: &mut R, payload: &mut Vec<u8>, length: usize) -> Result<(), FramingError> {
    let start = payload.len();
    payload.resize(start + length, 0);
    read_exact_or_eof(reader, &mut payload[start..])
}

fn header_placeholder() -> Vec<u8> {
    vec![0_u8; HEADER_LEN]
}

/// Fills in the header of `frame`, whose payload follows the placeholder.
fn finish_frame(mut frame: Vec<u8>, kind: FrameKind) -> Result<Vec<u8>, FramingError> {
    let length = frame.len() - HEADER_LEN;
    if length > kind.limit() {
        return Err(FramingError::Oversized {
            claimed: length,
            max: kind.limit(),
        });
    }
    fill_header(&mut frame, kind, length)?;
    Ok(frame)
}

/// Writes the header of a frame of this build's protocol at the start of `frame`.
fn fill_header(frame: &mut [u8], kind: FrameKind, length: usize) -> Result<(), FramingError> {
    let length = u32::try_from(length).map_err(|_| FramingError::Oversized {
        claimed: length,
        max: kind.limit(),
    })?;
    frame[..4].copy_from_slice(&MAGIC);
    frame[4..8].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    frame[8..12].copy_from_slice(&kind.number().to_le_bytes());
    frame[12..16].copy_from_slice(&length.to_le_bytes());
    Ok(())
}

/// A postcard sink that lays a message out as its frames while it serializes: a placeholder header before every
/// [`MAX_FRAME_SIZE`] bytes, filled in once the frame's kind is known (a part when more bytes follow it). Past
/// [`MAX_MESSAGE_SIZE`] it stores nothing more and only counts, so the refusal names the whole length.
struct FrameSink {
    frames: Vec<u8>,
    /// Where the current frame's header starts.
    current: usize,
    /// The message's postcard bytes so far, stored or not.
    length: usize,
}

impl FrameSink {
    fn new() -> Self {
        Self {
            frames: header_placeholder(),
            current: 0,
            length: 0,
        }
    }

    fn finish(mut self) -> Result<Vec<u8>, FramingError> {
        if self.length > MAX_MESSAGE_SIZE {
            return Err(FramingError::Oversized {
                claimed: self.length,
                max: MAX_MESSAGE_SIZE,
            });
        }
        let last = self.frames.len() - self.current - HEADER_LEN;
        fill_header(&mut self.frames[self.current..], FrameKind::Message, last)?;
        Ok(self.frames)
    }

    /// What the current frame still holds.
    fn room(&self) -> usize {
        MAX_FRAME_SIZE - (self.frames.len() - self.current - HEADER_LEN)
    }

    /// Closes the full current frame as a part and opens the next one.
    fn start_part(&mut self) -> postcard::Result<()> {
        fill_header(&mut self.frames[self.current..], FrameKind::MessagePart, MAX_FRAME_SIZE)
            .map_err(|_| postcard::Error::SerializeBufferFull)?;
        self.current = self.frames.len();
        self.frames.extend_from_slice(&[0; HEADER_LEN]);
        Ok(())
    }
}

impl postcard::ser_flavors::Flavor for FrameSink {
    type Output = Self;

    /// Postcard pushes a `Vec<u8>`'s bytes one at a time, so this is the hot path of an output stream.
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.length = self.length.saturating_add(1);
        if self.length > MAX_MESSAGE_SIZE {
            return Ok(());
        }
        if self.room() == 0 {
            self.start_part()?;
        }
        self.frames.push(byte);
        Ok(())
    }

    fn try_extend(&mut self, mut data: &[u8]) -> postcard::Result<()> {
        self.length = self.length.saturating_add(data.len());
        if self.length > MAX_MESSAGE_SIZE {
            return Ok(());
        }
        while !data.is_empty() {
            if self.room() == 0 {
                self.start_part()?;
            }
            let (now, rest) = data.split_at(self.room().min(data.len()));
            self.frames.extend_from_slice(now);
            data = rest;
        }
        Ok(())
    }

    fn finalize(self) -> postcard::Result<Self> {
        Ok(self)
    }
}

fn write_frame_bytes<W: Write>(writer: &mut W, frame: &[u8]) -> Result<(), FramingError> {
    writer.write_all(frame)?;
    writer.flush()?;
    Ok(())
}

/// Splits a header into its protocol, kind and length, refusing what no frame of any version has.
fn parse_header(header: &[u8; HEADER_LEN]) -> Result<(u32, FrameKind, usize), FramingError> {
    let word = |at: usize| u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
    let magic = [header[0], header[1], header[2], header[3]];
    if magic != MAGIC {
        return Err(FramingError::BadMagic(magic));
    }
    let kind = FrameKind::from_number(word(8)).ok_or(FramingError::UnknownKind(word(8)))?;
    let claimed = usize::try_from(word(12)).unwrap_or(usize::MAX);
    if claimed > kind.limit() {
        return Err(FramingError::Oversized {
            claimed,
            max: kind.limit(),
        });
    }
    Ok((word(4), kind, claimed))
}

/// Fills `header`; false when the stream ended before its first byte (a clean close between frames).
fn read_header<R: Read>(reader: &mut R, header: &mut [u8; HEADER_LEN]) -> Result<bool, FramingError> {
    let mut filled = 0;
    while filled < HEADER_LEN {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(FramingError::UnexpectedEof),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(FramingError::Io(error)),
        }
    }
    Ok(true)
}

/// `Read::read_exact`, with the end of the stream reported as [`FramingError::UnexpectedEof`].
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<(), FramingError> {
    reader.read_exact(buf).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            FramingError::UnexpectedEof
        } else {
            FramingError::Io(error)
        }
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// The stable control layer
// ---------------------------------------------------------------------------------------------------------------------

/// A control operation: the payload of a [`FrameKind::Control`] frame, read by every version. Each operation's name
/// and schema are frozen (module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Control {
    /// The first frame of every connection, from the client.
    #[serde(rename = "hello.v1")]
    Hello(Hello),
    /// The server's answer to a hello.
    #[serde(rename = "welcome.v1")]
    Welcome(Welcome),
    /// On a control connection: hand every session over to a new server.
    #[serde(rename = "handoff.request.v1")]
    HandoffRequest(HandoffRequest),
    /// The outcome of a handoff request.
    #[serde(rename = "handoff.result.v1")]
    HandoffResult(HandoffResult),
    /// An operation of a newer version. The server answers it with [`ErrorCode::UnknownOp`] and closes.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// What a connection is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Version-independent operations: the handoff request.
    Control,
    /// Request and reply by id ([`ApiCall`], [`ApiReply`]).
    Api,
    /// One session's stream ([`AttachClientMessage`], [`AttachServerMessage`]).
    Attach,
    /// A role of a newer version.
    #[serde(other)]
    Unknown,
}

/// A client's hello. Its protocol is the frame header's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub role: Role,
    /// The client's build id (a hash of its staged binary).
    pub build: String,
    pub capabilities: Vec<String>,
}

/// The server's answer to a hello. Its protocol is the frame header's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// The server's build id (a hash of its staged binary).
    pub build: String,
    /// What the server implements; a client ignores names it does not know.
    pub capabilities: Vec<String>,
    /// The server's process id.
    pub pid: u32,
    /// Set when the server refuses the connection; it closes after sending the welcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ControlError>,
}

/// Why the server refused a control operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlError {
    pub code: ErrorCode,
    pub message: String,
}

impl ControlError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ControlError {}

/// A refusal's permanent code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The hello's protocol differs from the server's, on a role that needs them equal.
    ProtocolMismatch,
    /// A control operation the server does not know.
    UnknownOp,
    /// A role the server does not know, or an operation the role does not take.
    UnsupportedRole,
    /// The handoff failed before its commit; the old server keeps serving.
    HandoffFailed,
    /// A code of a newer version.
    #[serde(other)]
    Other,
}

/// Hand every session over to the server started from `import_exe`. The replacement must report `expected_build`
/// and `expected_protocol`, or the handoff fails before its commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffRequest {
    /// The replacement server's executable.
    pub import_exe: String,
    pub expected_build: String,
    pub expected_protocol: u32,
}

/// The outcome of a handoff request: `Ok` once committed, an error when it failed before its commit. On the wire it
/// is `{"ok": true}` or `{"ok": false, "error": {...}}`; any other combination is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "HandoffResultWire", into = "HandoffResultWire")]
pub struct HandoffResult(pub Result<(), ControlError>);

#[derive(Serialize, Deserialize)]
struct HandoffResultWire {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<ControlError>,
}

impl TryFrom<HandoffResultWire> for HandoffResult {
    type Error = String;

    fn try_from(wire: HandoffResultWire) -> Result<Self, Self::Error> {
        match (wire.ok, wire.error) {
            (true, None) => Ok(Self(Ok(()))),
            (false, Some(error)) => Ok(Self(Err(error))),
            (true, Some(_)) => Err("a successful handoff result carries an error".to_owned()),
            (false, None) => Err("a failed handoff result carries no error".to_owned()),
        }
    }
}

impl From<HandoffResult> for HandoffResultWire {
    fn from(result: HandoffResult) -> Self {
        match result.0 {
            Ok(()) => Self { ok: true, error: None },
            Err(error) => Self {
                ok: false,
                error: Some(error),
            },
        }
    }
}

/// Whether a client of protocol `client` may use `role` on a server of [`PROTOCOL_VERSION`]: control always, api and
/// attach only on an exact match (herdr's `check_client_version`).
///
/// # Errors
///
/// The refusal the welcome carries.
pub fn check_hello(role: Role, client: u32) -> Result<(), ControlError> {
    match role {
        Role::Control => Ok(()),
        Role::Unknown => Err(ControlError::new(
            ErrorCode::UnsupportedRole,
            format!("this server (protocol {PROTOCOL_VERSION}) does not know the requested connection role"),
        )),
        Role::Api | Role::Attach => check_protocol(client),
    }
}

fn check_protocol(client: u32) -> Result<(), ControlError> {
    let server = PROTOCOL_VERSION;
    match client.cmp(&server) {
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Less => Err(ControlError::new(
            ErrorCode::ProtocolMismatch,
            format!(
                "client protocol {client} is older than server protocol {server}; use the agterm-session of the \
                 server's build"
            ),
        )),
        std::cmp::Ordering::Greater => Err(ControlError::new(
            ErrorCode::ProtocolMismatch,
            format!(
                "client protocol {client} is newer than server protocol {server}; the server predates this build and \
                 must hand its sessions over or be restarted"
            ),
        )),
    }
}

/// The server's welcome for `hello`, sent in a frame of protocol `client` (the hello's header).
#[must_use]
pub fn welcome(hello: &Hello, client: u32, build: &str, capabilities: &[String]) -> Welcome {
    Welcome {
        build: build.to_owned(),
        capabilities: capabilities.to_vec(),
        pid: std::process::id(),
        error: check_hello(hello.role, client).err(),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Messages of the api role
// ---------------------------------------------------------------------------------------------------------------------

/// A request on an api connection; the reply carries the same `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiCall {
    pub id: u64,
    pub request: ApiRequest,
}

/// The reply to the [`ApiCall`] with the same `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiReply {
    pub id: u64,
    pub result: Result<ApiResponse, ApiError>,
}

/// What `SessionHost` and `ngtermctl clipboard set` ask of the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiRequest {
    /// Every session of the namespace.
    List,
    /// Kill one session or a batch; `force` also ends a session whose shell ignores the hangup.
    Kill { names: Vec<String>, force: bool },
    /// The session's screen from the server's terminal, with scrollback when `all`.
    Screen { name: String, all: bool },
    /// Typed input (CR as Return, encoded for the keyboard mode), queued without taking the lead.
    Type { name: String, data: Vec<u8> },
    /// Bytes written into the session's output as if its program printed them (an OSC 52 clipboard write).
    Print { name: String, data: Vec<u8> },
    /// The foreground job of the terminal the shell `shell` leads.
    ForegroundJob { shell: i32 },
    /// Whether any process of `job` still runs.
    JobRunning { job: Vec<JobProcess> },
    /// Kill what is left of `job`.
    EndJob { job: Vec<JobProcess> },
}

/// A successful reply, one variant per request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiResponse {
    Sessions(Vec<SessionRecord>),
    Killed(Vec<KillResult>),
    Screen(Screen),
    /// `Type`, `Print` and `EndJob` succeeded.
    Done,
    /// The job; `None` when the process table cannot be read.
    ForegroundJob(Option<Vec<JobProcess>>),
    JobRunning(bool),
}

/// A failed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub kind: ApiErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiErrorKind {
    /// No session of that name.
    NoSuchSession,
    /// The server cannot do this here (an operation without a form on this OS).
    Unsupported,
    /// The operation failed.
    Failed,
}

/// One session as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub name: String,
    /// Attached clients.
    pub clients: u32,
    /// The session's shell; `None` once it has ended.
    pub leader_pid: Option<i32>,
    /// When the session was created, in whole seconds since the Unix epoch.
    pub created_at: u64,
}

/// What a kill did to one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillResult {
    pub name: String,
    pub outcome: KillOutcome,
}

/// [`agterm_host::KillOutcome`] on the wire: [`KillOutcome::Gone`] is the host's
/// [`agterm_host::KillOutcome::StaleSocket`], the outcome that killed nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KillOutcome {
    /// The session and its shell were ended, and the foreground job the shell left was hung up.
    Killed,
    /// No session of that name was left to kill: none was there, or its program had already ended.
    Gone,
    /// The kill failed, with the reason.
    Failed(String),
}

/// A session's screen as the server's terminal holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    /// Counts the screen's changes.
    pub revision: u64,
    pub columns: u32,
    pub rows: u32,
    /// Zero-based, in the active screen.
    pub cursor_column: u32,
    /// Zero-based, in the active screen.
    pub cursor_row: u32,
    /// The alternate screen is active.
    pub alternate: bool,
    /// Plain text read the way a terminal copies a selection (zmx's `serializeScreenText`): soft-wrapped rows joined
    /// into one line, trailing blanks of a line and trailing blank lines trimmed; empty for a blank screen.
    pub text: String,
}

/// One process of a foreground job, matched again later by pid and start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProcess {
    pub pid: i32,
    /// When it started, in the platform's own clock.
    pub started: u64,
    /// Its process group (Unix).
    pub group: i32,
    /// The foreground process group of its terminal; zero or negative without one.
    pub foreground: i32,
}

// What the api's replies are to `SessionHost` ([`agterm_host`]), and its job records back: the wire carries the host's
// types field for field, so a client converts without judging anything.

impl From<SessionRecord> for agterm_host::LiveSessionRecord {
    fn from(record: SessionRecord) -> Self {
        Self {
            name: record.name,
            clients: Some(record.clients),
            leader_pid: record.leader_pid,
            created_at: Some(record.created_at),
        }
    }
}

impl From<KillOutcome> for agterm_host::KillOutcome {
    fn from(outcome: KillOutcome) -> Self {
        match outcome {
            KillOutcome::Killed => Self::Killed,
            KillOutcome::Gone => Self::StaleSocket,
            KillOutcome::Failed(reason) => Self::Failed(reason),
        }
    }
}

impl From<Screen> for agterm_host::LiveScreen {
    fn from(screen: Screen) -> Self {
        Self {
            revision: screen.revision,
            columns: screen.columns,
            rows: screen.rows,
            cursor_column: screen.cursor_column,
            cursor_row: screen.cursor_row,
            alternate: screen.alternate,
            text: screen.text,
        }
    }
}

impl From<JobProcess> for agterm_host::ProcessRecord {
    fn from(process: JobProcess) -> Self {
        Self {
            pid: process.pid,
            started: process.started,
            group: process.group,
            foreground: process.foreground,
        }
    }
}

impl From<agterm_host::ProcessRecord> for JobProcess {
    fn from(record: agterm_host::ProcessRecord) -> Self {
        Self {
            pid: record.pid,
            started: record.started,
            group: record.group,
            foreground: record.foreground,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Messages of the attach role
// ---------------------------------------------------------------------------------------------------------------------

/// From an attached client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachClientMessage {
    /// The connection's first message: attach to a session, creating it when `create` is given.
    Attach(AttachRequest),
    /// Input for the session's program.
    Input { data: Vec<u8> },
    /// The client's grid changed; applied only while the client leads.
    Resize { size: GridSize },
    /// The client's facts for the replies the server gives on its behalf changed (they replace the earlier ones
    /// whole); the server answers with the reply owner's.
    Metadata(ReplyMetadata),
    /// The answer to the [`AttachServerMessage::DelegatedRequest`] `id` the client was sent in its epoch `epoch`;
    /// dropped unless that request still waits for this client's answer and `epoch` is still its current one.
    DelegatedReply { id: u64, epoch: u64, reply: DelegatedReply },
    /// What the client's terminal answered the program with to Kitty graphics commands (APC G: whole
    /// `ESC _ G … ESC \` answers, nothing else), sent only while the client owns the replies (the last
    /// [`AttachServerMessage::ReplyOwner`] it read said so) and never for a snapshot's bytes; `epoch` is its current
    /// epoch and `generation` that report's. Written to the program whatever the lead, after the replies queued before
    /// it; dropped unless the client still owns the replies in that generation and that epoch. A client's terminal
    /// answers nothing else: the server answers every other query itself.
    Reply { epoch: u64, generation: u32, data: Vec<u8> },
    /// Leave the session running and close the connection.
    Detach,
}

/// Attach to the session `name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachRequest {
    pub name: String,
    /// Create the session when it does not exist; without it, a missing session is refused as gone.
    pub create: Option<SessionSpec>,
    pub size: GridSize,
    /// Explicit leadership; `None` for an unmanaged client.
    pub managed: Option<Managed>,
    pub metadata: ReplyMetadata,
}

/// What a session is created with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSpec {
    /// The program and its arguments.
    pub argv: Vec<String>,
    /// The child's environment, in order.
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
    /// Names of variables the server records from the session's environment.
    pub tracked_env: Vec<String>,
}

/// An attach under explicit leadership: the client's token, echoed in its role reports, and whether it claims the
/// lead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Managed {
    pub nonce: String,
    pub claim: bool,
}

/// A terminal grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

/// A colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// What the server needs from the reply owner to answer frontend-dependent queries itself: the theme's colours (OSC
/// 4/10/11/12, Kitty's OSC 21), the cell's pixel size (XTWINOPS `14 t`/`16 t`, mode 2048, the cells a Kitty image
/// covers) and the desktop's appearance (`CSI ? 996 n`, mode 2031). A value left out takes the server's stable
/// default (`server::replies`), never another client's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyMetadata {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    /// `None`: the foreground's colour answers for it, as Ghostty answers OSC 12 without a cursor colour.
    pub cursor: Option<Rgb>,
    /// Empty for the library's palette, else exactly the 256 entries (any other length counts as empty).
    pub palette: Vec<Rgb>,
    /// Zero when unknown.
    pub cell_width_px: u32,
    /// Zero when unknown.
    pub cell_height_px: u32,
    /// Whether the client's terminal shows Kitty graphics.
    pub kitty_graphics: bool,
    /// The desktop's appearance; `None` when unknown.
    pub color_scheme: Option<ColorScheme>,
}

/// The desktop's light or dark appearance, as a colour scheme query (`CSI ? 996 n`) and report (mode 2031) tell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorScheme {
    Light,
    Dark,
}

/// To an attached client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachServerMessage {
    /// A snapshot of the session starts; output resumes at `offset` after it. A new epoch replaces any snapshot not
    /// yet ended and any output not yet sent.
    SnapshotBegin { epoch: u64, offset: u64, size: GridSize },
    /// Part of the snapshot's bytes, fed to the client's terminal as output.
    SnapshotChunk { epoch: u64, data: Vec<u8> },
    /// The snapshot is complete.
    SnapshotEnd { epoch: u64 },
    /// The session's output from `offset` on, contiguous with what came before.
    Output { offset: u64, data: Vec<u8> },
    /// The session's grid changed, in the output at `offset`.
    Resized { offset: u64, size: GridSize },
    /// The client's role changed; `generation` counts the session's leadership changes.
    Role { role: LeadRole, generation: u32 },
    /// Whether the client owns the session's replies from here on in the stream (its terminal answers Kitty graphics
    /// commands, [`AttachClientMessage::Reply`]; no other client's terminal answers anything). The owner is the leader,
    /// else the first managed client attached. Sent to every client before its first snapshot (after its role), then
    /// whenever its ownership changes; `generation` counts the session's ownership changes.
    ReplyOwner { owner: bool, generation: u32 },
    /// A request only the reply owner can answer, sent to it alone, in order with the output: the output up to the
    /// program's request came before it, the output after it waits for the answer, or for the request's end (denied
    /// when the owner changes, the client falls behind or leaves, or no answer comes within the server's deadline).
    DelegatedRequest {
        id: u64,
        epoch: u64,
        request: DelegatedRequest,
    },
    /// The attach was refused; the server closes.
    Refused { reason: AttachRefusal, message: String },
    /// The session's program ended; the server closes.
    Exit { status: ExitStatus },
    /// The server is shutting down.
    Shutdown { reason: Option<String> },
}

/// A client's role at its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeadRole {
    /// No client leads.
    Unowned,
    Leader,
    Follower,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DelegatedRequest {
    /// A program's clipboard read (OSC 52, or Kitty's OSC 5522) of `selection`: `c` the clipboard, `s` the selection
    /// clipboard, `p` the primary selection. `list_only`: it asks only which types the clipboard holds (Kitty's `.`
    /// target), which needs no consent; the program is never served the contents for it.
    ClipboardRead { selection: String, list_only: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DelegatedReply {
    /// The clipboard's text; `None` when it holds none (the program reads an empty clipboard).
    Clipboard { text: Option<String> },
    /// Refused by policy or by the user: the program reads an empty clipboard (OSC 52) or `EPERM` (OSC 5522).
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachRefusal {
    /// The session does not exist and the attach did not ask to create it.
    Gone,
    /// The session could not be created.
    CreateFailed,
    /// No consistent snapshot could be cut in time.
    Unavailable,
}

/// How a session's program ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExitStatus {
    /// Its exit code, and the signal that ended it.
    Exited { code: u32, signal: Option<String> },
    /// Unknown: the server is not the program's parent (after a Unix handoff).
    Unknown,
}
