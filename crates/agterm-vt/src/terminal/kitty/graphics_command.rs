//! The Kitty graphics protocol's commands: the parser of an APC `G` sequence's control data and payload, the command
//! tree it builds, and the response a command gets.
//!
//! Ghostty: `src/terminal/kitty/graphics_command.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/kitty/graphics_command.rs` at `RUSTTY_REV` (MIT).
//! Changes: the payload is a `Vec` grown fallibly, the command owns it, and there is no allocator argument (the heap
//! is the allocator). Integer values are read with Zig's `std.fmt.parseInt`, as Ghostty's are, so a `_` digit
//! separator is accepted (rustty refuses it; libghostty-vt does not). Zig's `LibEnum` descriptors are plain enums.
//! The packed `Usage` and `Background` convert from their `u32` with `from_bits` and `from_int`.

use crate::fmt::{ParseIntError, parse_int};
use crate::mem::alloc::TryVecExt;
use crate::simd::base64;

/// The log target of this module (Ghostty's `.kitty_gfx` scope).
const LOG_SCOPE: &str = "kitty_gfx";

/// A command that could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    InvalidFormat,
    Overflow,
    OutOfMemory,
    InvalidData,
}

impl From<ParseIntError> for Error {
    /// `error.Overflow` stays `Overflow`; `error.InvalidCharacter` is `InvalidFormat`.
    fn from(err: ParseIntError) -> Self {
        match err {
            ParseIntError::Overflow => Self::Overflow,
            ParseIntError::InvalidCharacter => Self::InvalidFormat,
        }
    }
}

/// The key-value pairs for the control information for a command. The recognized keys are always single ASCII letters
/// and the values are either single characters or 32-bit unsigned integers. Unknown non-letter keys are ignored,
/// matching how unknown letter keys are ignored when building a command.
///
/// This is deliberately a dense value table plus a presence bitmap. There are only 52 possible keys, so a lookup is
/// one machine-word bit test followed by an indexed load. The values are undefined until their presence bit is set.
///
/// For the value itself: if it is a single printable ASCII character it is the ASCII code. Otherwise, it is parsed as
/// a 32-bit unsigned integer.
#[derive(Debug, Clone, Copy)]
struct Kv {
    values: [u32; 52],
    present: u64,
}

impl Default for Kv {
    fn default() -> Self {
        Self {
            values: [0; 52],
            present: 0,
        }
    }
}

impl Kv {
    /// The table index of a key: `a`-`z` then `A`-`Z`.
    fn index(key: u8) -> Option<u8> {
        match key {
            b'a'..=b'z' => Some(key - b'a'),
            b'A'..=b'Z' => Some(26 + key - b'A'),
            _ => None,
        }
    }

    /// The value of `key`, if it was given.
    fn get(&self, key: u8) -> Option<u32> {
        let idx = Self::index(key)?;
        (self.present & (1_u64 << idx) != 0).then(|| self.values[usize::from(idx)])
    }

    /// The value of `key`, or 0 when it was not given.
    fn get_or_zero(&self, key: u8) -> u32 {
        self.get(key).unwrap_or(0)
    }

    /// The value of a key that holds an `i32` (`z`, `H`, `V`): the parser stores its bit pattern.
    fn get_signed(&self, key: u8) -> Option<i32> {
        self.get(key).map(|v| i32::from_ne_bytes(v.to_ne_bytes()))
    }

    /// Store `value` under `key`; a key that is not a letter is ignored.
    fn put(&mut self, key: u8, value: u32) {
        if let Some(idx) = Self::index(key) {
            self.values[usize::from(idx)] = value;
            self.present |= 1_u64 << idx;
        }
    }
}

/// Command parser parses the Kitty graphics protocol escape sequence.
#[derive(Debug)]
pub(crate) struct Parser {
    /// This is the list of KV pairs that we're building up.
    kv: Kv,

    /// This is used as a buffer to store the key/value of a KV pair. The value of a KV pair is at most a 32-bit
    /// integer which at most is 10 characters (4294967295), plus one character for the sign bit on signed ints.
    kv_temp: [u8; 11],
    kv_temp_len: u8,
    /// Current kv key
    kv_current: u8,

    /// This is the list we use to collect the bytes from the data payload. The Kitty Graphics protocol specification
    /// seems to imply that the payload content of a single command should never exceed 4096 bytes, but Kitty itself
    /// supports larger payloads, so we use a growable list here instead of a fixed buffer so that we can too.
    data: Vec<u8>,

    /// Maximum bytes the data payload can take. This is to prevent malicious input from causing us to allocate too
    /// much memory.
    max_bytes: usize,

    /// Internal state for parsing.
    state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Parsing k/v pairs. The "ignore" variants are in that state but ignore any data because we know they're invalid.
    ControlKey,
    ControlKeyIgnore,
    ControlValue,
    ControlValueIgnore,

    /// Collecting the data payload blob.
    Data,
}

impl Parser {
    /// Initialize the parser, its payload bounded by `max_bytes`.
    pub(crate) const fn new(max_bytes: usize) -> Self {
        Self {
            kv: Kv {
                values: [0; 52],
                present: 0,
            },
            kv_temp: [0; 11],
            kv_temp_len: 0,
            kv_current: 0,
            data: Vec::new(),
            max_bytes,
            state: State::ControlKey,
        }
    }

    /// Parse a complete command string.
    pub(crate) fn parse_string(data: &[u8]) -> Result<Command, Error> {
        let mut parser = Self::new(1024 * 1024);
        for &c in data {
            parser.feed(c)?;
        }
        parser.complete()
    }

    /// Feed a single byte to the parser.
    ///
    /// The first byte to start parsing should be the byte immediately following the "G" in the APC sequence, i.e.
    /// `\x1b_G123` the first byte should be "1".
    pub(crate) fn feed(&mut self, c: u8) -> Result<(), Error> {
        match self.state {
            State::ControlKey => match c {
                // '=' means the key is complete and we're moving to the value.
                b'=' => {
                    if self.kv_temp_len == 1 {
                        self.kv_current = self.kv_temp[0];
                        self.state = State::ControlValue;
                    } else {
                        // All control keys are a single character right now so if we're not a single character just
                        // ignore follow-up data.
                        self.state = State::ControlValueIgnore;
                    }
                    self.kv_temp_len = 0;
                }

                // This can be encountered if we have a sequence with no control data, only payload data (i.e.
                // "\x1b_G;<data>").
                //
                // Kitty treats this as valid so we do as well.
                b';' => self.state = State::Data,

                _ => self.accumulate_value(c, State::ControlKeyIgnore),
            },

            State::ControlKeyIgnore => {
                if c == b'=' {
                    self.state = State::ControlValueIgnore;
                }
            }

            State::ControlValue => match c {
                // move to next key
                b',' => self.finish_value(State::ControlKey)?,
                // move to data
                b';' => self.finish_value(State::Data)?,
                _ => self.accumulate_value(c, State::ControlValueIgnore),
            },

            State::ControlValueIgnore => match c {
                b',' => self.state = State::ControlKeyIgnore,
                b';' => self.state = State::Data,
                _ => {}
            },

            State::Data => {
                if self.data.len() >= self.max_bytes {
                    return Err(Error::OutOfMemory);
                }
                self.data.try_push(c).map_err(|_| Error::OutOfMemory)?;
            }
        }

        Ok(())
    }

    /// Feed a slice of bytes to the parser. This is equivalent to calling feed for each byte in order, but once we're
    /// in the data state the remainder of the slice is appended in bulk, avoiding per-byte overhead for large payloads.
    pub(crate) fn feed_slice(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut rem = bytes;
        while let Some((&c, rest)) = rem.split_first() {
            if self.state == State::Data {
                if self.data.len() + rem.len() > self.max_bytes {
                    return Err(Error::OutOfMemory);
                }
                return self.data.try_extend_copy(rem).map_err(|_| Error::OutOfMemory);
            }
            self.feed(c)?;
            rem = rest;
        }
        Ok(())
    }

    /// Complete the parsing. This must be called after all the bytes have been fed to the parser.
    pub(crate) fn complete(&mut self) -> Result<Command, Error> {
        match self.state {
            // We can't ever end in the control key state and be valid. This means the command looked something like
            // "a=1,b"
            State::ControlKey | State::ControlKeyIgnore => return Err(Error::InvalidFormat),

            // Some commands (i.e. placements) end without extra data so we end in the value state. i.e. "a=1,b=2"
            State::ControlValue => self.finish_value(State::Data)?,

            // Most commands end in data, i.e. "a=1,b=2;1234"
            State::ControlValueIgnore | State::Data => {}
        }

        // Determine our action, which is always a single character.
        let action = u8::try_from(self.kv.get(b'a').unwrap_or(u32::from(b't'))).map_err(|_| Error::InvalidFormat)?;
        let kv = &self.kv;
        let control = match action {
            b'q' => Control::Query(Transmission::parse(kv)?),
            b't' => Control::Transmit(Transmission::parse(kv)?),
            b'T' => Control::TransmitAndDisplay {
                transmission: Transmission::parse(kv)?,
                display: Display::parse(kv),
            },
            b'p' => Control::Display(Display::parse(kv)),
            b'd' => Control::Delete(Delete::parse(kv)?),
            b'f' => Control::TransmitAnimationFrame(AnimationFrameLoading::parse(kv)?),
            b'a' => Control::ControlAnimation(AnimationControl::parse(kv)),
            b'c' => Control::ComposeAnimation(AnimationFrameComposition::parse(kv)),
            _ => return Err(Error::InvalidFormat),
        };

        // Determine our quiet value. The spec specifies 0 and 1 but Kitty allows anything greater than 1 to suppress.
        let quiet = match self.kv.get(b'q') {
            None | Some(0) => Quiet::No,
            Some(1) => Quiet::Ok,
            Some(_) => Quiet::Failures,
        };

        Ok(Command {
            control,
            quiet,
            data: self.decode_data()?,
        })
    }

    /// Decodes the payload data from base64 and returns it. This takes the contents of self.data, it should only be
    /// used once we are done collecting payload bytes.
    fn decode_data(&mut self) -> Result<Vec<u8>, Error> {
        if self.data.is_empty() {
            return Ok(Vec::new());
        }

        let max_len = base64::max_len(&self.data);
        debug_assert!(max_len <= self.data.len());

        // This is kinda cursed, but we can decode the base64 on top of itself, since it's guaranteed that the encoded
        // size is larger, and any bytes in areas that are written to will have already been used (assuming scalar
        // decoding).
        let decoded_len = match base64::decode_in_place(&mut self.data, max_len) {
            Ok(decoded_len) => decoded_len,
            Err(err) => {
                tracing::warn!(target: LOG_SCOPE, "failed to decode base64 payload data: {err:?}");
                return Err(Error::InvalidData);
            }
        };
        debug_assert!(decoded_len <= max_len);

        // Remove the extra bytes.
        self.data.truncate(decoded_len);
        Ok(core::mem::take(&mut self.data))
    }

    /// Add one byte to the key or value being read; past the buffer, go to `overflow_state`.
    fn accumulate_value(&mut self, c: u8, overflow_state: State) {
        let idx = usize::from(self.kv_temp_len);
        if idx >= self.kv_temp.len() {
            self.state = overflow_state;
            self.kv_temp_len = 0;
            return;
        }
        self.kv_temp[idx] = c;
        self.kv_temp_len += 1;
    }

    /// Store the value read for the current key and go to `next_state`.
    fn finish_value(&mut self, next_state: State) -> Result<(), Error> {
        // We can move states right away, we don't use it.
        self.state = next_state;
        let temp = &self.kv_temp[..usize::from(self.kv_temp_len)];

        // Check for ASCII chars first
        if let [c] = *temp
            && !c.is_ascii_digit()
        {
            self.kv.put(self.kv_current, u32::from(c));
            self.kv_temp_len = 0;
            return Ok(());
        }

        // Handle integer fields, parsing signed fields accordingly. We still store the fields as u32 as they can be
        // bitcast back later during building of the higher-level command tree.
        let v = match self.kv_current {
            b'z' | b'H' | b'V' => u32::from_ne_bytes(parse_int::<i32>(temp, 10)?.to_ne_bytes()),
            _ => parse_int::<u32>(temp, 10)?,
        };
        self.kv.put(self.kv_current, v);

        // Clear our temp buffer
        self.kv_temp_len = 0;
        Ok(())
    }
}

/// Represents a possible response to a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Response {
    pub(crate) id: u32,
    pub(crate) image_number: u32,
    pub(crate) placement_id: u32,
    /// The 1-based animation frame number, echoed as "r=" for the animation frame actions. This is how clients learn
    /// the frame number assigned to a newly created frame.
    pub(crate) frame: u32,
    pub(crate) message: &'static [u8],
}

impl Default for Response {
    fn default() -> Self {
        Self {
            id: 0,
            image_number: 0,
            placement_id: 0,
            frame: 0,
            message: b"OK",
        }
    }
}

impl Response {
    /// Encode the response, `ESC _ G <keys> ; <message> ESC \`, or nothing without an id or image number.
    pub(crate) fn encode<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        // We only encode a result if we have either an id or an image number.
        if self.empty() {
            return Ok(());
        }

        writer.write_all(b"\x1b_G")?;
        let mut separator: &[u8] = b"";
        for (key, value) in [
            ('i', self.id),
            ('I', self.image_number),
            ('p', self.placement_id),
            ('r', self.frame),
        ] {
            if value > 0 {
                writer.write_all(separator)?;
                write!(writer, "{key}={value}")?;
                separator = b",";
            }
        }
        writer.write_all(b";")?;
        writer.write_all(self.message)?;
        writer.write_all(b"\x1b\\")
    }

    /// Returns true if this response is not an error.
    pub(crate) fn ok(&self) -> bool {
        self.message == b"OK"
    }

    /// Empty response
    pub(crate) fn empty(&self) -> bool {
        self.id == 0 && self.image_number == 0
    }
}

/// A parsed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    pub(crate) control: Control,
    pub(crate) quiet: Quiet,
    /// The decoded payload.
    pub(crate) data: Vec<u8>,
}

/// A command's action key (`a`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// q
    Query,
    /// t
    Transmit,
    /// T
    TransmitAndDisplay,
    /// p
    Display,
    /// d
    Delete,
    /// f
    TransmitAnimationFrame,
    /// a
    ControlAnimation,
    /// c
    ComposeAnimation,
}

/// Which responses a command suppresses (`q`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Quiet {
    /// 0
    No,
    /// 1
    Ok,
    /// 2
    Failures,
}

/// The command's action with its keys.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::enum_variant_names,
    reason = "the Kitty actions' names: `a=a` is control animation"
)]
pub(crate) enum Control {
    Query(Transmission),
    Transmit(Transmission),
    TransmitAndDisplay {
        transmission: Transmission,
        display: Display,
    },
    Display(Display),
    Delete(Delete),
    TransmitAnimationFrame(AnimationFrameLoading),
    ControlAnimation(AnimationControl),
    ComposeAnimation(AnimationFrameComposition),
}

/// The image and placement a command names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Identifiers {
    pub(crate) image_id: u32,
    pub(crate) image_number: u32,
    pub(crate) placement_id: u32,
}

impl Control {
    /// The action of this control.
    pub(crate) fn action(&self) -> Action {
        match self {
            Self::Query(_) => Action::Query,
            Self::Transmit(_) => Action::Transmit,
            Self::TransmitAndDisplay { .. } => Action::TransmitAndDisplay,
            Self::Display(_) => Action::Display,
            Self::Delete(_) => Action::Delete,
            Self::TransmitAnimationFrame(_) => Action::TransmitAnimationFrame,
            Self::ControlAnimation(_) => Action::ControlAnimation,
            Self::ComposeAnimation(_) => Action::ComposeAnimation,
        }
    }

    /// Returns the image and placement identifiers for any action.
    pub(crate) fn identifiers(&self) -> Identifiers {
        let (image_id, image_number, placement_id) = match self {
            Self::Query(t) | Self::Transmit(t) | Self::TransmitAndDisplay { transmission: t, .. } => {
                (t.image_id, t.image_number, t.placement_id)
            }
            Self::Display(d) => (d.image_id, d.image_number, d.placement_id),
            Self::Delete(d) => (d.image_id, d.image_number, d.placement_id),
            Self::TransmitAnimationFrame(f) => (
                f.transmission.image_id,
                f.transmission.image_number,
                f.transmission.placement_id,
            ),
            Self::ControlAnimation(a) => (a.image_id, a.image_number, a.placement_id),
            Self::ComposeAnimation(c) => (c.image_id, c.image_number, c.placement_id),
        };
        Identifiers {
            image_id,
            image_number,
            placement_id,
        }
    }
}

impl Command {
    /// Take ownership over the data in this command. If the returned value has a length of zero, then the data was
    /// empty.
    pub(crate) fn take_data(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.data)
    }

    /// Returns the transmission data if it has any.
    pub(crate) fn transmission(&self) -> Option<Transmission> {
        match &self.control {
            Control::Query(t) | Control::Transmit(t) => Some(*t),
            Control::TransmitAndDisplay { transmission, .. } => Some(*transmission),
            Control::TransmitAnimationFrame(f) => Some(f.transmission),
            _ => None,
        }
    }

    /// Returns the display data if it has any.
    pub(crate) fn display(&self) -> Option<Display> {
        match &self.control {
            Control::Display(d) | Control::TransmitAndDisplay { display: d, .. } => Some(*d),
            _ => None,
        }
    }
}

/// The transmission keys of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Transmission {
    /// f
    pub(crate) format: Format,
    /// Set when "f" carried a value we don't recognize. Kitty rejects these while handling the command so the EINVAL
    /// response can carry the image id, so we defer to `LoadingImage.init`. `format` is meaningless when this is set.
    pub(crate) format_unknown: bool,
    /// t
    pub(crate) medium: Medium,
    /// s
    pub(crate) width: u32,
    /// v
    pub(crate) height: u32,
    /// S
    pub(crate) size: u32,
    /// O
    pub(crate) offset: u32,
    /// i
    pub(crate) image_id: u32,
    /// I
    pub(crate) image_number: u32,
    /// p
    pub(crate) placement_id: u32,
    /// o
    pub(crate) compression: Compression,
    /// m
    pub(crate) more_chunks: bool,
    /// N
    pub(crate) usage: Usage,
}

impl Default for Transmission {
    fn default() -> Self {
        Self {
            format: Format::Rgba,
            format_unknown: false,
            medium: Medium::Direct,
            width: 0,
            height: 0,
            size: 0,
            offset: 0,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            compression: Compression::None,
            more_chunks: false,
            usage: Usage::DEFAULT,
        }
    }
}

/// Usage hints allow for optimising resource consumption strategies.
///
/// <https://sw.kovidgoyal.net/kitty/graphics-protocol/#usage-hints>
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Usage {
    /// Image with this usage hint is assumed to be used for only a short time, so may be evicted before other images
    /// if memory pressure is encountered.
    pub(crate) transient: bool,
}

impl Usage {
    /// No hints.
    pub(crate) const DEFAULT: Self = Self { transient: false };

    /// `@bitCast(v)` where `v` is the `u32` value of the `N` key.
    pub(crate) const fn from_bits(v: u32) -> Self {
        Self {
            transient: v & 0b1 != 0,
        }
    }
}

/// The pixel format of a transmission (`f`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    /// 24
    Rgb,
    /// 32
    Rgba,
    /// 100
    Png,
    // The following are not supported directly via the protocol but they are formats that a png may decode to that
    // we support.
    GrayAlpha,
    Gray,
}

/// The transmission medium (`t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Medium {
    /// d
    Direct,
    /// f
    File,
    /// t
    TemporaryFile,
    /// s
    SharedMemory,
}

/// The payload compression (`o`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Compression {
    None,
    /// z
    ZlibDeflate,
}

impl Transmission {
    /// The bytes per pixel of a raw format. A PNG must be decoded (validated) before this is asked.
    pub(crate) const fn format_bpp(format: Format) -> u8 {
        match format {
            Format::Gray => 1,
            Format::GrayAlpha => 2,
            Format::Rgb => 3,
            Format::Rgba => 4,
            Format::Png => unreachable!(),
        }
    }

    /// The transmission keys of a command.
    fn parse(kv: &Kv) -> Result<Self, Error> {
        let mut result = Self::default();
        if let Some(v) = kv.get(b'f') {
            result.format = match v {
                24 => Format::Rgb,
                0 | 32 => Format::Rgba,
                100 => Format::Png,
                _ => {
                    // Defer returning an error until the image is processed.
                    result.format_unknown = true;
                    Format::Rgba
                }
            };
        }

        if let Some(v) = kv.get(b't') {
            result.medium = match u8::try_from(v) {
                Ok(b'd') => Medium::Direct,
                Ok(b'f') => Medium::File,
                Ok(b't') => Medium::TemporaryFile,
                Ok(b's') => Medium::SharedMemory,
                _ => return Err(Error::InvalidFormat),
            };
        }

        result.width = kv.get_or_zero(b's');
        result.height = kv.get_or_zero(b'v');
        result.size = kv.get_or_zero(b'S');
        result.offset = kv.get_or_zero(b'O');
        result.image_id = kv.get_or_zero(b'i');
        result.image_number = kv.get_or_zero(b'I');
        result.placement_id = kv.get_or_zero(b'p');

        if let Some(v) = kv.get(b'o') {
            result.compression = match u8::try_from(v) {
                Ok(b'z') => Compression::ZlibDeflate,
                _ => return Err(Error::InvalidFormat),
            };
        }

        // If the transmission medium is a local-only medium, ignore the "m" key. The Kitty graphics protocol
        // specification does not explicitly call out this behavior (although the "m" key is only mentioned in
        // connection with remote clients) but that's how it's implemented in Kitty and at least one client (mpv)
        // relies on this behavior when using the shared memory transmission medium.
        //
        // https://sw.kovidgoyal.net/kitty/graphics-protocol/#the-transmission-medium
        // https://github.com/kovidgoyal/kitty/blob/ccc3bee9af794f332b4e9adcd714a649f639c397/kitty/graphics.c#L547-L592
        if result.medium == Medium::Direct
            && let Some(v) = kv.get(b'm')
        {
            result.more_chunks = v > 0;
        }

        if let Some(v) = kv.get(b'N') {
            result.usage = Usage::from_bits(v);
        }

        Ok(result)
    }
}

/// The display (placement) keys of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Display {
    /// i
    pub(crate) image_id: u32,
    /// I
    pub(crate) image_number: u32,
    /// p
    pub(crate) placement_id: u32,
    /// x
    pub(crate) x: u32,
    /// y
    pub(crate) y: u32,
    /// w
    pub(crate) width: u32,
    /// h
    pub(crate) height: u32,
    /// X
    pub(crate) x_offset: u32,
    /// Y
    pub(crate) y_offset: u32,
    /// c
    pub(crate) columns: u32,
    /// r
    pub(crate) rows: u32,
    /// C
    pub(crate) cursor_movement: CursorMovement,
    /// U
    pub(crate) virtual_placement: bool,
    /// P
    pub(crate) parent_id: u32,
    /// Q
    pub(crate) parent_placement_id: u32,
    /// H
    pub(crate) horizontal_offset: i32,
    /// V
    pub(crate) vertical_offset: i32,
    /// z
    pub(crate) z: i32,
}

/// Whether a placement moves the cursor (`C`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CursorMovement {
    /// 0
    #[default]
    After,
    /// 1
    None,
}

impl Display {
    /// The display keys of a command.
    fn parse(kv: &Kv) -> Self {
        Self {
            image_id: kv.get_or_zero(b'i'),
            image_number: kv.get_or_zero(b'I'),
            placement_id: kv.get_or_zero(b'p'),
            x: kv.get_or_zero(b'x'),
            y: kv.get_or_zero(b'y'),
            width: kv.get_or_zero(b'w'),
            height: kv.get_or_zero(b'h'),
            x_offset: kv.get_or_zero(b'X'),
            y_offset: kv.get_or_zero(b'Y'),
            columns: kv.get_or_zero(b'c'),
            rows: kv.get_or_zero(b'r'),
            // Kitty only tests this against 1 so any other value moves the cursor.
            cursor_movement: if kv.get(b'C') == Some(1) {
                CursorMovement::None
            } else {
                CursorMovement::After
            },
            // Kitty stores this in a bool, so any non-zero value is virtual.
            virtual_placement: kv.get(b'U').is_some_and(|v| v != 0),
            // We can bitcast these because of how we parse them earlier.
            z: kv.get_signed(b'z').unwrap_or(0),
            parent_id: kv.get_or_zero(b'P'),
            parent_placement_id: kv.get_or_zero(b'Q'),
            horizontal_offset: kv.get_signed(b'H').unwrap_or(0),
            vertical_offset: kv.get_signed(b'V').unwrap_or(0),
        }
    }
}

/// The keys of an animation frame transmission (`a=f`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AnimationFrameLoading {
    /// Frame data is transmitted exactly like image data, so an a=f command carries a full set of transmission keys:
    /// medium (t), format (f), the frame rectangle size (s/v), chunking (m), etc. The image id/number here identify
    /// the (existing) image the frame belongs to.
    pub(crate) transmission: Transmission,

    /// The left edge (pixels) of the rectangle within the frame that the transmitted data updates (x).
    pub(crate) x: u32,
    /// The top edge (y).
    pub(crate) y: u32,

    /// The 1-based frame number whose pixels serve as the base canvas for a newly created frame. Zero means no base
    /// frame: the canvas is filled with `background` instead (c).
    pub(crate) create_frame: u32,

    /// The 1-based frame number of an existing frame to edit. Zero (or any value past the next frame number) creates a
    /// new frame (r).
    pub(crate) edit_frame: u32,

    /// The gap in milliseconds before the next frame is shown. For new frames, zero selects the default gap and a
    /// negative value creates a gapless (never displayed) frame. For edits, zero leaves the gap unchanged (z).
    pub(crate) gap_ms: i32,

    /// X
    pub(crate) composition_mode: CompositionMode,
    /// Y
    pub(crate) background: Background,
}

/// The canvas background color as a 32-bit RGBA value where R is the most significant byte (0xRRGGBBAA), e.g.
/// Y=4278190335 (0xff0000ff) is opaque red. Bit-cast from the little-endian u32 value, so the field order is reversed
/// from the byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Background {
    pub(crate) a: u8,
    pub(crate) b: u8,
    pub(crate) g: u8,
    pub(crate) r: u8,
}

impl Background {
    /// `@bitCast(v)`: the background of the `Y` key's value.
    pub(crate) const fn from_int(v: u32) -> Self {
        let [alpha, blue, green, red] = v.to_le_bytes();
        Self {
            a: alpha,
            b: blue,
            g: green,
            r: red,
        }
    }
}

impl AnimationFrameLoading {
    /// The keys of an animation frame transmission.
    fn parse(kv: &Kv) -> Result<Self, Error> {
        Ok(Self {
            transmission: Transmission::parse(kv)?,
            x: kv.get_or_zero(b'x'),
            y: kv.get_or_zero(b'y'),
            create_frame: kv.get_or_zero(b'c'),
            edit_frame: kv.get_or_zero(b'r'),
            // 'z' is one of the keys Parser.finishValue parses as an i32, so the u32 in the KV table is the bit
            // pattern of that i32 and this reconstructs it exactly.
            gap_ms: kv.get_signed(b'z').unwrap_or(0),
            // Tested only against 1 like Kitty.
            composition_mode: match kv.get(b'X') {
                Some(1) => CompositionMode::Overwrite,
                _ => CompositionMode::AlphaBlend,
            },
            background: kv.get(b'Y').map(Background::from_int).unwrap_or_default(),
        })
    }
}

/// The keys of an animation frame composition (`a=c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AnimationFrameComposition {
    /// i
    pub(crate) image_id: u32,
    /// I
    pub(crate) image_number: u32,
    /// p
    pub(crate) placement_id: u32,

    /// The 1-based frame number being edited (the destination) (c).
    pub(crate) dest_frame: u32,

    /// The 1-based frame number whose pixels are composed onto the destination (the source) (r).
    pub(crate) source_frame: u32,

    /// The left edge (pixels) of the destination rectangle (x).
    pub(crate) x: u32,
    /// The top edge (y).
    pub(crate) y: u32,

    /// The width of the rectangle to compose. Zero selects the full image width (w).
    pub(crate) width: u32,
    /// The height; zero selects the full image height (h).
    pub(crate) height: u32,

    /// The left edge (pixels) of the source rectangle (X).
    pub(crate) left_edge: u32,
    /// The top edge (Y).
    pub(crate) top_edge: u32,

    /// C
    pub(crate) composition_mode: CompositionMode,
}

impl AnimationFrameComposition {
    /// Cannot fail: every key is stored as parsed, with no range validation (Kitty range-checks these while handling
    /// the command).
    fn parse(kv: &Kv) -> Self {
        Self {
            image_id: kv.get_or_zero(b'i'),
            image_number: kv.get_or_zero(b'I'),
            placement_id: kv.get_or_zero(b'p'),
            dest_frame: kv.get_or_zero(b'c'),
            source_frame: kv.get_or_zero(b'r'),
            x: kv.get_or_zero(b'x'),
            y: kv.get_or_zero(b'y'),
            width: kv.get_or_zero(b'w'),
            height: kv.get_or_zero(b'h'),
            left_edge: kv.get_or_zero(b'X'),
            top_edge: kv.get_or_zero(b'Y'),
            // Kitty tests this against zero for compose so any non-zero overwrites
            composition_mode: match kv.get(b'C') {
                None | Some(0) => CompositionMode::AlphaBlend,
                Some(_) => CompositionMode::Overwrite,
            },
        }
    }
}

/// The keys of an animation control (`a=a`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AnimationControl {
    /// i
    pub(crate) image_id: u32,
    /// I
    pub(crate) image_number: u32,
    /// p
    pub(crate) placement_id: u32,
    /// s
    pub(crate) action: AnimationAction,

    /// The 1-based frame number whose gap is set from `gap_ms` (r).
    pub(crate) frame: u32,

    /// The new gap for `frame` in milliseconds. Zero is ignored and a negative value makes the frame gapless (z).
    pub(crate) gap_ms: i32,

    /// The 1-based frame number to make current (client-driven animation). Zero is ignored (c).
    pub(crate) current_frame: u32,

    /// The loop count: zero is ignored, 1 loops forever, and any larger value plays that many minus one loops (v).
    pub(crate) loops: u32,
}

/// The animation state an animation control sets (`s`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AnimationAction {
    /// 0
    #[default]
    Invalid,
    /// 1
    Stop,
    /// 2
    RunWait,
    /// 3
    Run,
}

impl AnimationControl {
    /// Cannot fail: every key is stored as parsed. Out-of-range values (frame numbers, states) are silently ignored
    /// during execution, matching Kitty.
    fn parse(kv: &Kv) -> Self {
        Self {
            image_id: kv.get_or_zero(b'i'),
            image_number: kv.get_or_zero(b'I'),
            placement_id: kv.get_or_zero(b'p'),
            // Kitty ignores values it doesn't know, leaving the animation state untouched, which is what `.invalid`
            // means here.
            action: match kv.get(b's') {
                Some(1) => AnimationAction::Stop,
                Some(2) => AnimationAction::RunWait,
                Some(3) => AnimationAction::Run,
                _ => AnimationAction::Invalid,
            },
            frame: kv.get_or_zero(b'r'),
            // 'z' is one of the keys Parser.finishValue parses as an i32, so the u32 in the KV table is the bit
            // pattern of that i32 and this reconstructs it exactly.
            gap_ms: kv.get_signed(b'z').unwrap_or(0),
            current_frame: kv.get_or_zero(b'c'),
            loops: kv.get_or_zero(b'v'),
        }
    }
}

/// The keys of a delete command (`a=d`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Delete {
    /// i
    pub(crate) image_id: u32,
    /// I
    pub(crate) image_number: u32,
    /// p
    pub(crate) placement_id: u32,
    pub(crate) action: DeleteAction,
}

impl Delete {
    /// The keys of a delete command.
    fn parse(kv: &Kv) -> Result<Self, Error> {
        Ok(Self {
            image_id: kv.get_or_zero(b'i'),
            image_number: kv.get_or_zero(b'I'),
            placement_id: kv.get_or_zero(b'p'),
            action: DeleteAction::parse(kv)?,
        })
    }
}

/// What a delete command deletes (`d`); an uppercase key also frees the image data (`delete`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteAction {
    /// a/A
    All(bool),
    /// i/I
    Id {
        delete: bool,
        image_id: u32,
        placement_id: u32,
    },
    /// n/N
    Newest {
        delete: bool,
        image_number: u32,
        placement_id: u32,
    },
    /// c/C
    IntersectCursor(bool),
    /// f/F
    AnimationFrames(DeleteAnimationFrames),
    /// p/P
    IntersectCell { delete: bool, x: u32, y: u32 },
    /// q/Q
    IntersectCellZ { delete: bool, x: u32, y: u32, z: i32 },
    /// r/R
    Range { delete: bool, first: u32, last: u32 },
    /// x/X
    Column { delete: bool, x: u32 },
    /// y/Y
    Row { delete: bool, y: u32 },
    /// z/Z
    Z { delete: bool, z: i32 },
}

/// The keys of a delete of animation frames (`d=f`, `d=F`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DeleteAnimationFrames {
    /// uppercase
    pub(crate) delete: bool,
    /// i
    pub(crate) image_id: u32,
    /// I
    pub(crate) image_number: u32,
    /// r
    pub(crate) frame: u32,
}

impl DeleteAction {
    /// What a delete command deletes.
    fn parse(kv: &Kv) -> Result<Self, Error> {
        let what = u8::try_from(kv.get(b'd').unwrap_or(u32::from(b'a'))).map_err(|_| Error::InvalidFormat)?;
        let delete = what.is_ascii_uppercase();
        let x = kv.get_or_zero(b'x');
        let y = kv.get_or_zero(b'y');
        // We can bitcast here because of how we parse it earlier.
        let z = kv.get_signed(b'z').unwrap_or(0);
        Ok(match what.to_ascii_lowercase() {
            b'a' => Self::All(delete),
            b'i' => Self::Id {
                delete,
                image_id: kv.get_or_zero(b'i'),
                placement_id: kv.get_or_zero(b'p'),
            },
            b'n' => Self::Newest {
                delete,
                image_number: kv.get_or_zero(b'I'),
                placement_id: kv.get_or_zero(b'p'),
            },
            b'c' => Self::IntersectCursor(delete),
            b'f' => Self::AnimationFrames(DeleteAnimationFrames {
                delete,
                image_id: kv.get_or_zero(b'i'),
                image_number: kv.get_or_zero(b'I'),
                frame: kv.get_or_zero(b'r'),
            }),
            b'p' => Self::IntersectCell { delete, x, y },
            b'q' => Self::IntersectCellZ { delete, x, y, z },
            // Both bounds default to zero when omitted and no validation is performed; an inverted or zero range
            // simply matches no images.
            b'r' => Self::Range {
                delete,
                first: x,
                last: y,
            },
            b'x' => Self::Column { delete, x },
            b'y' => Self::Row { delete, y },
            b'z' => Self::Z { delete, z },
            _ => return Err(Error::InvalidFormat),
        })
    }
}

/// How an animation frame is composed onto its canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CompositionMode {
    /// 0
    #[default]
    AlphaBlend,
    /// 1
    Overwrite,
}

#[cfg(test)]
mod tests;
