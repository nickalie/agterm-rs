//! OSC (Operating System Command) related functions and types.
//!
//! OSC is another set of control sequences for terminal programs that start with "ESC ]". Unlike CSI or standard ESC
//! sequences, they may contain strings and other irregular formatting so a dedicated parser is created to handle it.
//!
//! Ghostty: `src/terminal/osc.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/mod.rs` at `RUSTTY_REV` (MIT).
//! Changes: a [`Command`] borrows its text from the parser that produced it, as Zig's slices into the parser's
//! storage do: "only valid until the next call to the parser" is the borrow. Its text is `&[u8]`, not Zig's
//! NUL-terminated `[:0]const u8`: the sentinel served the C API, which is not here. Zig's `parser.command` is not a
//! field (it would make the parser self-referential); [`Parser::end`] builds the command, and the two lists it owns in
//! Zig (`color_operation.requests`, `kitty_color_protocol.list`) are the parser's own until [`Parser::reset`]. The
//! optional allocator is [`Parser::alloc`], whether the parser may allocate: the OSCs that need one are refused
//! without it, as in Zig, and allocation itself is the heap's, failing as an error. The allocating capture grows a
//! `Vec` within its bound. Zig's `LibEnum`s and the C forms (`C`, `cval`, `ProgressReport.C`) are not here.

use crate::terminal::kitty::color as kitty_color;

pub(crate) mod encoding;
pub(crate) mod kitty_metadata;
pub(crate) mod parsers;

pub(crate) use parsers::color;

/// The log target of the OSC parser (Ghostty's `.osc` scope).
const LOG_SCOPE: &str = "osc";

/// A parsed OSC command. Its text and lists are borrowed from the parser that produced it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Command<'p> {
    /// This generally shouldn't ever be set except as an initial zero value. Ignore it.
    Invalid,

    /// Set the window title of the terminal
    ///
    /// If title mode 0 is set text is expect to be hex encoded (i.e. utf-8 with each code unit further encoded with
    /// two hex digits).
    ///
    /// If title mode 2 is set or the terminal is setup for unconditional utf-8 titles text is interpreted as utf-8.
    /// Else text is interpreted as latin1.
    ChangeWindowTitle(&'p [u8]),

    /// Set the icon of the terminal window. The name of the icon is not well defined, so this is currently ignored by
    /// Ghostty at the time of writing this. We just parse it so that we don't get parse errors in the log.
    ChangeWindowIcon(&'p [u8]),

    /// Semantic prompt command:
    /// <https://gitlab.freedesktop.org/Per_Bothner/specifications/blob/master/proposals/semantic-prompts.md>
    SemanticPrompt(SemanticPrompt<'p>),

    /// Set or get clipboard contents. If data is "?", then the current clipboard contents are sent to the pty.
    /// Otherwise, the contents are set on the clipboard.
    ClipboardContents {
        kind: u8,
        data: &'p [u8],
        /// The terminator, which the OSC 52 read response echoes.
        terminator: Terminator,
    },

    /// OSC 7. Reports the current working directory of the shell. This is a moderately flawed escape sequence but one
    /// that many major terminals support so we also support it. To understand the flaws, read through this
    /// terminal-wg issue: <https://gitlab.freedesktop.org/terminal-wg/specifications/-/issues/20>
    ReportPwd {
        /// The reported pwd value. This is not checked for validity. It should be a file URL but it is up to the
        /// caller to utilize this value.
        value: &'p [u8],
    },

    /// OSC 22. Set the mouse shape. There doesn't seem to be a standard naming scheme for cursors but it looks like
    /// terminals such as Foot are moving towards using the W3C CSS cursor names. For OSC parsing, we just parse
    /// whatever string is given.
    MouseShape { value: &'p [u8] },

    /// OSC color operations to set, reset, or report color settings. Some OSCs allow multiple operations to be
    /// specified in a single OSC so we need a list-like datastructure to manage them.
    ///
    /// Currently, these OSCs are handled by `color_operation`:
    ///
    /// 4, 5, 10-19, 104, 105, 110-119
    ColorOperation {
        op: color::Operation,
        /// The parser's list of the requests.
        requests: &'p color::List,
        terminator: Terminator,
    },

    /// Kitty color protocol, OSC 21 <https://sw.kovidgoyal.net/kitty/color-stack/#id1>
    KittyColorProtocol(&'p kitty_color::Osc),

    /// Show a desktop notification (OSC 9 or OSC 777)
    ShowDesktopNotification { title: &'p [u8], body: &'p [u8] },

    /// Start a hyperlink (OSC 8)
    HyperlinkStart { id: Option<&'p [u8]>, uri: &'p [u8] },

    /// End a hyperlink (OSC 8)
    HyperlinkEnd,

    /// `ConEmu` sleep (OSC 9;1)
    ConemuSleep { duration_ms: u16 },

    /// `ConEmu` show GUI message box (OSC 9;2)
    ConemuShowMessageBox(&'p [u8]),

    /// `ConEmu` change tab title (OSC 9;3)
    ConemuChangeTabTitle(ConemuChangeTabTitle<'p>),

    /// `ConEmu` progress report (OSC 9;4)
    ConemuProgressReport(ProgressReport),

    /// `ConEmu` wait input (OSC 9;5)
    ConemuWaitInput,

    /// `ConEmu` GUI macro (OSC 9;6)
    ConemuGuimacro(&'p [u8]),

    /// `ConEmu` run process (OSC 9;7)
    ConemuRunProcess(&'p [u8]),

    /// `ConEmu` output environment variable (OSC 9;8)
    ConemuOutputEnvironmentVariable(&'p [u8]),

    /// `ConEmu` `XTerm` keyboard and output emulation (OSC 9;10) <https://conemu.github.io/en/TerminalModes.html>
    ConemuXtermEmulation {
        /// null => do not change, false => turn off, true => turn on
        keyboard: Option<bool>,
        /// null => do not change, false => turn off, true => turn on
        output: Option<bool>,
    },

    /// `ConEmu` comment (OSC 9;11)
    ConemuComment(&'p [u8]),

    /// Kitty text sizing protocol (OSC 66)
    KittyTextSizing(parsers::kitty_text_sizing::Osc<'p>),

    /// Kitty clipboard protocol (OSC 5522)
    KittyClipboardProtocol(KittyClipboardProtocol<'p>),

    /// Kitty drag and drop protocol (OSC 72) <https://sw.kovidgoyal.net/kitty/drag-and-drop-protocol/>
    KittyDndProtocol(KittyDndProtocol<'p>),

    /// OSC 3008. Hierarchical context signalling (UAPI spec).
    /// <https://uapi-group.org/specifications/specs/osc_context/>
    ContextSignal(parsers::context_signal::Command<'p>),

    /// Kitty desktop notifications (OSC 99)
    KittyDesktopNotification(KittyDesktopNotification<'p>),

    /// An OSC sequence whose number this parser does not implement. Only produced when
    /// [`Parser::unknown_max_bytes`] is nonzero.
    Unknown(Unknown<'p>),

    /// Program status protocol (OSC 7501). A program reports what it is doing, such as working or waiting on the user,
    /// or asks whether the terminal supports the protocol. See [`ProgramStatus`].
    ProgramStatus(ProgramStatus<'p>),
}

/// An OSC sequence whose number this parser does not implement. For the sequence `ESC ] 7400;status=busy BEL`,
/// `content` is "7400;status=busy" and `terminator` is `.bel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unknown<'p> {
    /// Every byte fed to the parser for this sequence, including the number at the start. Owned by the parser and
    /// only valid until the next parser call.
    ///
    /// When the bytes come from the VT stream, this never contains control characters (bytes below 0x20). Those end
    /// the sequence, cancel it, or are dropped before they reach the OSC parser.
    pub(crate) content: &'p [u8],

    /// True if the sequence was longer than [`Parser::unknown_max_bytes`], or memory ran out while reading it. In that
    /// case `content` holds only the beginning of the sequence.
    pub(crate) truncated: bool,

    /// How the program ended the sequence. If you send a reply, end it the same way.
    pub(crate) terminator: Terminator,
}

/// The payload of a `ConEmu` tab title change (OSC 9;3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConemuChangeTabTitle<'p> {
    Reset,
    Value(&'p [u8]),
}

/// `Command.Key`: the command's tag, in Zig's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandKey {
    Invalid,
    ChangeWindowTitle,
    ChangeWindowIcon,
    SemanticPrompt,
    ClipboardContents,
    ReportPwd,
    MouseShape,
    ColorOperation,
    KittyColorProtocol,
    ShowDesktopNotification,
    HyperlinkStart,
    HyperlinkEnd,
    ConemuSleep,
    ConemuShowMessageBox,
    ConemuChangeTabTitle,
    ConemuProgressReport,
    ConemuWaitInput,
    ConemuGuimacro,
    ConemuRunProcess,
    ConemuOutputEnvironmentVariable,
    ConemuXtermEmulation,
    ConemuComment,
    KittyTextSizing,
    KittyClipboardProtocol,
    KittyDndProtocol,
    ContextSignal,
    KittyDesktopNotification,
    Unknown,
    ProgramStatus,
}

impl Command<'_> {
    /// The command's tag (Zig's coercion of the union to its tag).
    pub(crate) fn key(&self) -> CommandKey {
        match self {
            Self::Invalid => CommandKey::Invalid,
            Self::ChangeWindowTitle(_) => CommandKey::ChangeWindowTitle,
            Self::ChangeWindowIcon(_) => CommandKey::ChangeWindowIcon,
            Self::SemanticPrompt(_) => CommandKey::SemanticPrompt,
            Self::ClipboardContents { .. } => CommandKey::ClipboardContents,
            Self::ReportPwd { .. } => CommandKey::ReportPwd,
            Self::MouseShape { .. } => CommandKey::MouseShape,
            Self::ColorOperation { .. } => CommandKey::ColorOperation,
            Self::KittyColorProtocol(_) => CommandKey::KittyColorProtocol,
            Self::ShowDesktopNotification { .. } => CommandKey::ShowDesktopNotification,
            Self::HyperlinkStart { .. } => CommandKey::HyperlinkStart,
            Self::HyperlinkEnd => CommandKey::HyperlinkEnd,
            Self::ConemuSleep { .. } => CommandKey::ConemuSleep,
            Self::ConemuShowMessageBox(_) => CommandKey::ConemuShowMessageBox,
            Self::ConemuChangeTabTitle(_) => CommandKey::ConemuChangeTabTitle,
            Self::ConemuProgressReport(_) => CommandKey::ConemuProgressReport,
            Self::ConemuWaitInput => CommandKey::ConemuWaitInput,
            Self::ConemuGuimacro(_) => CommandKey::ConemuGuimacro,
            Self::ConemuRunProcess(_) => CommandKey::ConemuRunProcess,
            Self::ConemuOutputEnvironmentVariable(_) => CommandKey::ConemuOutputEnvironmentVariable,
            Self::ConemuXtermEmulation { .. } => CommandKey::ConemuXtermEmulation,
            Self::ConemuComment(_) => CommandKey::ConemuComment,
            Self::KittyTextSizing(_) => CommandKey::KittyTextSizing,
            Self::KittyClipboardProtocol(_) => CommandKey::KittyClipboardProtocol,
            Self::KittyDndProtocol(_) => CommandKey::KittyDndProtocol,
            Self::ContextSignal(_) => CommandKey::ContextSignal,
            Self::KittyDesktopNotification(_) => CommandKey::KittyDesktopNotification,
            Self::Unknown(_) => CommandKey::Unknown,
            Self::ProgramStatus(_) => CommandKey::ProgramStatus,
        }
    }
}

/// `Command.SemanticPrompt`.
pub(crate) type SemanticPrompt<'p> = parsers::semantic_prompt::Command<'p>;

/// `Command.KittyClipboardProtocol`.
pub(crate) type KittyClipboardProtocol<'p> = parsers::kitty_clipboard_protocol::Osc<'p>;

/// `Command.KittyDndProtocol`.
pub(crate) type KittyDndProtocol<'p> = parsers::kitty_dnd_protocol::Osc<'p>;

/// `Command.KittyDesktopNotification`.
pub(crate) type KittyDesktopNotification<'p> = parsers::kitty_desktop_notification::Osc<'p>;

/// `Command.ProgramStatus`.
pub(crate) type ProgramStatus<'p> = parsers::program_status::Command<'p>;

/// `Command.ProgressReport`: the `ConEmu` progress report payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgressReport {
    pub(crate) state: ProgressReportState,
    pub(crate) progress: Option<u8>,
}

/// The state of a `ConEmu` progress report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProgressReportState {
    Remove,
    Set,
    Error,
    Indeterminate,
    Pause,
}

/// The terminator used to end an OSC command. For OSC commands that demand a response, we try to match the terminator
/// used in the request since that is most likely to be accepted by the calling program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Terminator {
    /// The preferred string terminator is ESC followed by \
    St,

    /// Some applications and terminals use BELL (0x07) as the string terminator.
    Bel,
}

impl Terminator {
    /// Initialize the terminator based on the last byte seen. If the last byte is a BEL then we use BEL, otherwise we
    /// just assume ST.
    pub(crate) fn init(ch: Option<u8>) -> Self {
        match ch {
            Some(0x07) => Self::Bel,
            None | Some(_) => Self::St,
        }
    }

    /// The terminator as a string. This is static memory so it doesn't need to be freed.
    pub(crate) fn string(self) -> &'static [u8] {
        match self {
            Self::St => b"\x1b\\",
            Self::Bel => b"\x07",
        }
    }
}

impl core::fmt::Display for Terminator {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::St => "\x1b\\",
            Self::Bel => "\x07",
        })
    }
}

/// Maximum size of a "normal" OSC.
pub(crate) const MAX_BUF: usize = 2048;

/// Maximum size of an OSC that requires dynamically allocated storage. OSC input is untrusted, so these captures must
/// have a finite bound.
pub(crate) const MAX_ALLOCATING_BUF: usize = 8 * 1024 * 1024;

/// The OSC parser.
pub(crate) struct Parser {
    /// Whether the parser has an allocator (Zig's optional `alloc`), used to accept data longer than [`MAX_BUF`].
    /// This only applies to some commands (e.g. OSC 52) that can reasonably exceed [`MAX_BUF`], and to the OSCs that
    /// build lists (colors), which are refused without one.
    pub(crate) alloc: bool,

    /// Maximum number of bytes retained by an allocating capture. This is configurable primarily so callers and tests
    /// can choose a smaller policy than the default.
    pub(crate) max_allocating_bytes: usize,

    /// The most bytes to keep from each OSC sequence whose number this parser does not implement.
    ///
    /// Zero, the default, discards these sequences. The parser stops reading one as soon as it sees a number it
    /// doesn't know, and `end` returns null. Any other value makes `end` return [`Command::Unknown`] with the
    /// sequence's bytes, so you can implement the sequence yourself. A sequence longer than the limit is still
    /// returned, with the first bytes up to the limit and `truncated` set to true.
    ///
    /// Limits up to [`MAX_BUF`] use a buffer inside the parser and never allocate. Larger limits allocate for each
    /// unknown sequence. Without an allocator, a larger limit behaves like [`MAX_BUF`]. If memory runs out partway
    /// through a sequence, it is returned truncated.
    ///
    /// Sequences with a number the parser does implement are not affected, even when their contents are malformed.
    pub(crate) unknown_max_bytes: usize,

    /// Current state of the parser.
    pub(crate) state: State,

    /// Buffer for temporary storage of OSC data
    pub(crate) buffer: [u8; MAX_BUF],

    /// Capture state. If this is set then we're actively capturing the bytes coming into the parser.
    pub(crate) capture: Option<Capture>,

    /// The request list of the last color operation, which its command borrows until the next reset.
    pub(crate) color_requests: Option<color::List>,

    /// The request list of the last Kitty color protocol command, which its command borrows until the next reset.
    pub(crate) kitty_color: Option<kitty_color::Osc>,
}

/// The parser's state: the OSC number read so far, or what it is doing with the rest of the sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Start,
    Invalid,

    /// Collecting the bytes of a sequence whose number is not implemented, to return as [`Command::Unknown`]. Only
    /// entered when `unknown_max_bytes` is nonzero.
    Unknown,

    /// Like `Unknown`, but the byte limit has been reached. Later bytes are dropped and the command is returned with
    /// `truncated` set.
    UnknownTruncated,

    // OSC command prefixes. Not all of these are valid OSCs, but may be needed to "bridge" to a valid OSC (e.g. to
    // support OSC 777 we need to have a state "77" even though there is no OSC 77). The bytes consumed to reach each
    // is `State::prefix`.
    S0,
    S1,
    S2,
    S3,
    S4,
    S5,
    S6,
    S7,
    S8,
    S9,
    S30,
    S300,
    S3008,
    S10,
    S11,
    S12,
    S13,
    S14,
    S15,
    S16,
    S17,
    S18,
    S19,
    S21,
    S22,
    S52,
    S55,
    S66,
    S72,
    S75,
    S750,
    S7501,
    S77,
    S99,
    S104,
    S105,
    S110,
    S111,
    S112,
    S113,
    S114,
    S115,
    S116,
    S117,
    S118,
    S119,
    S133,
    S552,
    S777,
    S1337,
    S5522,
}

impl State {
    /// `@tagName(state)` as `{t}` prints it: `start`, `invalid`, `unknown`, `unknown_truncated`, or the OSC number
    /// (`@"52"` is `52`).
    pub(crate) fn tag_name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Invalid => "invalid",
            Self::Unknown => "unknown",
            Self::UnknownTruncated => "unknown_truncated",
            prefix => prefix.number(),
        }
    }

    /// The bytes consumed to reach this prefix state (`.@"13"` after reading "13"), and "" for `start`. `invalid`,
    /// `unknown` and `unknown_truncated` are not prefix states and have none, as in Zig, where they are unreachable.
    pub(crate) fn prefix(self) -> &'static [u8] {
        match self {
            Self::Start | Self::Invalid | Self::Unknown | Self::UnknownTruncated => b"",
            prefix => prefix.number().as_bytes(),
        }
    }

    /// The OSC number of a prefix state.
    fn number(self) -> &'static str {
        match self {
            Self::Start | Self::Invalid | Self::Unknown | Self::UnknownTruncated => "",
            Self::S0 => "0",
            Self::S1 => "1",
            Self::S2 => "2",
            Self::S3 => "3",
            Self::S4 => "4",
            Self::S5 => "5",
            Self::S6 => "6",
            Self::S7 => "7",
            Self::S8 => "8",
            Self::S9 => "9",
            Self::S30 => "30",
            Self::S300 => "300",
            Self::S3008 => "3008",
            Self::S10 => "10",
            Self::S11 => "11",
            Self::S12 => "12",
            Self::S13 => "13",
            Self::S14 => "14",
            Self::S15 => "15",
            Self::S16 => "16",
            Self::S17 => "17",
            Self::S18 => "18",
            Self::S19 => "19",
            Self::S21 => "21",
            Self::S22 => "22",
            Self::S52 => "52",
            Self::S55 => "55",
            Self::S66 => "66",
            Self::S72 => "72",
            Self::S75 => "75",
            Self::S750 => "750",
            Self::S7501 => "7501",
            Self::S77 => "77",
            Self::S99 => "99",
            Self::S104 => "104",
            Self::S105 => "105",
            Self::S110 => "110",
            Self::S111 => "111",
            Self::S112 => "112",
            Self::S113 => "113",
            Self::S114 => "114",
            Self::S115 => "115",
            Self::S116 => "116",
            Self::S117 => "117",
            Self::S118 => "118",
            Self::S119 => "119",
            Self::S133 => "133",
            Self::S552 => "552",
            Self::S777 => "777",
            Self::S1337 => "1337",
            Self::S5522 => "5522",
        }
    }
}

/// An active capture of the rest of a sequence: the bytes go to the parser's fixed buffer or to a growing one.
pub(crate) struct Capture {
    pub(crate) backing: CaptureBacking,
    /// The byte bound this capture may not grow beyond: for the fixed backing the buffer length (`MAX_BUF`, or the
    /// shorter `min(unknown_max_bytes, MAX_BUF)` slice of an unknown capture); for the allocating backing the
    /// parser's `max_allocating_bytes` policy, or `unknown_max_bytes`. `capture_failed` pins it to the retained
    /// length once an unknown capture is truncated.
    pub(crate) max_bytes: usize,
}

/// Where a capture's bytes go.
pub(crate) enum CaptureBacking {
    /// The count of bytes captured so far into [`Parser::buffer`] (Zig's fixed writer's `end` over the buffer).
    Fixed(usize),
    /// A growing buffer (`std.Io.Writer.Allocating`).
    Allocating(Vec<u8>),
}

/// The backing a capture starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureMode {
    Fixed,
    Allocating,
}

/// Grow `w`'s allocation, within `max_bytes`, so it holds at least `needed` bytes: to twice its size, or exactly
/// `needed` when that is more, never past `max_bytes` (`Allocating.Writer` normally grows super-linearly, so it is
/// grown explicitly to keep the allocation itself bounded too). Returns false when the allocation failed.
fn grow_bounded(w: &mut Vec<u8>, needed: usize, max_bytes: usize) -> bool {
    if needed <= w.capacity() {
        return true;
    }
    let new_capacity = max_bytes.min(w.capacity().saturating_mul(2).max(needed));
    w.try_reserve_exact(new_capacity - w.len()).is_ok()
}

impl Capture {
    /// Return the captured trailing data. This is the data from the point that trailing data capture was requested.
    /// The fixed backing's bytes are the front of the parser's buffer, which is passed in.
    #[inline]
    pub(crate) fn trailing<'b>(&'b self, buffer: &'b [u8; MAX_BUF]) -> &'b [u8] {
        match &self.backing {
            CaptureBacking::Fixed(len) => &buffer[..*len],
            CaptureBacking::Allocating(w) => w,
        }
    }

    /// The captured trailing data, for the parsers that overwrite a separator byte with a sentinel in place (e.g. OSC
    /// 777 rxvt).
    #[inline]
    pub(crate) fn trailing_mut<'b>(&'b mut self, buffer: &'b mut [u8; MAX_BUF]) -> &'b mut [u8] {
        match &mut self.backing {
            CaptureBacking::Fixed(len) => &mut buffer[..*len],
            CaptureBacking::Allocating(w) => w,
        }
    }

    /// Append one byte without permitting the backing allocation to grow beyond `max_bytes`. Returns false on Zig's
    /// `error.WriteFailed`: the capture is full or the allocation failed.
    #[inline]
    pub(crate) fn write_byte(&mut self, buffer: &mut [u8; MAX_BUF], c: u8) -> bool {
        match &mut self.backing {
            CaptureBacking::Fixed(len) => {
                if *len >= self.max_bytes || *len >= MAX_BUF {
                    return false;
                }
                buffer[*len] = c;
                *len += 1;
                true
            }
            CaptureBacking::Allocating(w) => {
                if w.len() >= self.max_bytes || !grow_bounded(w, w.len() + 1, self.max_bytes) {
                    return false;
                }
                w.push(c);
                true
            }
        }
    }

    /// Append a slice without permitting the backing allocation to grow beyond `max_bytes`. This matches the
    /// byte-at-a-time semantics of `write_byte`: bytes are retained up to exactly `max_bytes` and the first byte that
    /// doesn't fit fails the write (false).
    pub(crate) fn write_slice(&mut self, buffer: &mut [u8; MAX_BUF], bytes: &[u8]) -> bool {
        let buffered_len = match &self.backing {
            CaptureBacking::Fixed(len) => *len,
            CaptureBacking::Allocating(w) => w.len(),
        };
        let n = bytes.len().min(self.max_bytes.saturating_sub(buffered_len));
        match &mut self.backing {
            // The fixed writer's capacity is `max_bytes`, so `n` always fits.
            CaptureBacking::Fixed(len) => {
                buffer[*len..*len + n].copy_from_slice(&bytes[..n]);
                *len += n;
            }
            CaptureBacking::Allocating(w) => {
                if !grow_bounded(w, w.len() + n, self.max_bytes) {
                    return false;
                }
                // The growth above made room for all `n` bytes, so this does not reallocate.
                w.extend_from_slice(&bytes[..n]);
            }
        }
        n == bytes.len()
    }
}

impl Parser {
    /// A parser, with an allocator or without one (`alloc`).
    pub(crate) const fn new(alloc: bool) -> Self {
        Self {
            alloc,
            max_allocating_bytes: MAX_ALLOCATING_BUF,
            unknown_max_bytes: 0,
            state: State::Start,
            buffer: [0; MAX_BUF],
            capture: None,
            color_requests: None,
            kitty_color: None,
        }
    }

    /// Reset the parser state.
    pub(crate) fn reset(&mut self) {
        // Handle any cleanup that individual OSCs require: the lists the last command owned. The borrow checker has
        // ended the command that borrowed them before `reset` can run.
        self.kitty_color = None;
        self.color_requests = None;

        // If we're capturing, then stop it.
        self.state = State::Start;
        self.capture = None;
    }

    /// Append the NUL Zig writes after a captured body (a `[:0]` string's sentinel), so the body's text is everything
    /// before the last byte of [`Capture::trailing`]. The sentinel takes a byte of the capture, so a full capture
    /// refuses it: then, or without a capture, this returns false and invalidates the parser, as Zig's failed
    /// `writeByte(0)` does.
    pub(crate) fn write_sentinel(&mut self) -> bool {
        let written = self
            .capture
            .as_mut()
            .is_some_and(|cap| cap.write_byte(&mut self.buffer, 0));
        if !written {
            self.state = State::Invalid;
        }
        written
    }

    /// The captured body after [`Parser::write_sentinel`]: its text, without the sentinel. `None` without a capture.
    pub(crate) fn terminated_trailing(&mut self) -> Option<&[u8]> {
        if !self.write_sentinel() {
            return None;
        }
        let data = self.capture.as_ref()?.trailing(&self.buffer);
        data.split_last().map(|(_, text)| text)
    }

    /// Make sure that we have an allocator. If we don't, set the state to invalid so that any additional OSC data is
    /// discarded.
    #[inline]
    fn ensure_allocator(&mut self) -> bool {
        if self.alloc {
            return true;
        }
        tracing::warn!(
            target: LOG_SCOPE,
            "An allocator is required to process OSC {} but none was provided.",
            self.state.tag_name()
        );
        self.state = State::Invalid;
        false
    }

    /// Begin capturing trailing data. All inputs to next from this point forward will be captured into the capture's
    /// buffer, which may be backed by either a fixed size or allocating buffer depending on mode.
    ///
    /// Get the trailing data using `capture.trailing()`.
    #[inline]
    fn capture_trailing(&mut self, mode: CaptureMode) {
        debug_assert!(self.capture.is_none());
        let fixed = Capture {
            backing: CaptureBacking::Fixed(0),
            max_bytes: MAX_BUF,
        };
        self.capture = Some(match mode {
            CaptureMode::Fixed => fixed,
            // We don't have an allocator - fall back to a fixed buffer and hope that it's big enough.
            CaptureMode::Allocating if !self.alloc => fixed,
            CaptureMode::Allocating => {
                let mut w = Vec::new();
                if w.try_reserve_exact(MAX_BUF.min(self.max_allocating_bytes)).is_err() {
                    // The allocator failed for some reason, fall back to a fixed buffer and hope that it's big
                    // enough.
                    fixed
                } else {
                    Capture {
                        backing: CaptureBacking::Allocating(w),
                        max_bytes: self.max_allocating_bytes,
                    }
                }
            }
        });
    }

    /// Called when the sequence's number turns out not to be one this parser implements, either at byte c or at the
    /// end of the sequence (c is null). Supported OSCs never reach this.
    ///
    /// With `unknown_max_bytes` at zero, this marks the sequence invalid, exactly as the parser did before unknown
    /// sequences were supported. Otherwise it starts collecting the sequence's bytes. The digits read so far are not
    /// stored anywhere, but the current state's name is exactly those digits (state `.@"13"` after reading "13"). So
    /// the state name is written first, then c, and the collected bytes hold the whole sequence from its first byte.
    ///
    /// This is deliberately not inline. It is referenced from every prefix state in `next`, and inlining it there
    /// grows the state machine enough to measurably slow per-byte parsing of supported OSCs.
    #[inline(never)]
    fn unknown_or_invalid(&mut self, c: Option<u8>) {
        let max_bytes = self.unknown_max_bytes;
        if max_bytes == 0 {
            self.state = State::Invalid;
            return;
        }

        debug_assert!(self.capture.is_none());
        let prefix = self.state.prefix();
        self.state = State::Unknown;

        // Limits past the fixed buffer need an allocation. Without an allocator, or if allocating fails, the fixed
        // buffer is used and longer sequences are reported as truncated.
        let fixed = |max_bytes: usize| Capture {
            backing: CaptureBacking::Fixed(0),
            max_bytes,
        };
        let mut cap = if max_bytes > MAX_BUF && self.alloc {
            let mut w = Vec::new();
            if w.try_reserve_exact(MAX_BUF.min(max_bytes)).is_ok() {
                Capture {
                    backing: CaptureBacking::Allocating(w),
                    max_bytes,
                }
            } else {
                fixed(MAX_BUF)
            }
        } else {
            fixed(max_bytes.min(MAX_BUF))
        };

        let written =
            cap.write_slice(&mut self.buffer, prefix) && c.is_none_or(|b| cap.write_byte(&mut self.buffer, b));
        self.capture = Some(cap);
        if !written {
            self.capture_failed();
        }
    }

    /// Handle a failed capture write, either from reaching the capture's limit or from an allocation failure.
    /// Supported OSCs become invalid. Unknown OSCs keep what was captured and are reported as truncated.
    ///
    /// This is inline so the supported-OSC overflow path stays a single store.
    #[inline]
    fn capture_failed(&mut self) {
        match self.state {
            State::Unknown => {
                // Pin the limit to what was retained so every later write fails at the first bounds check. Without
                // this, a later allocating write could succeed after an allocation failure and leave a gap in the
                // content.
                if let Some(cap) = self.capture.as_mut() {
                    cap.max_bytes = cap.trailing(&self.buffer).len();
                }
                self.state = State::UnknownTruncated;
            }
            // Already truncated: the write failed at the first bounds check and there is nothing more to do.
            State::UnknownTruncated => {}
            _ => self.state = State::Invalid,
        }
    }

    /// Consume a slice of bytes, advancing the parser state. This is equivalent to calling `next` for each byte in
    /// order, but is much faster once a data capture is active because the remaining bytes are appended to the
    /// capture in bulk.
    pub(crate) fn next_slice(&mut self, input: &[u8]) {
        if self.state == State::Invalid {
            return;
        }

        // Run the state machine byte-at-a-time until a capture begins. The command prefix before a capture starts is
        // only a handful of bytes so this loop is short in practice.
        let mut offset: usize = 0;
        while self.capture.is_none() {
            let Some(&c) = input.get(offset) else {
                return;
            };
            self.next(c);
            offset += 1;
            if self.state == State::Invalid {
                return;
            }
        }

        let rem = &input[offset..];
        if rem.is_empty() {
            return;
        }
        let written = self
            .capture
            .as_mut()
            .is_some_and(|cap| cap.write_slice(&mut self.buffer, rem));
        if !written {
            // We have overflowed our buffer or had some other error. Discard any further input.
            self.capture_failed();
        }
    }

    /// Consume the next character c and advance the parser state.
    #[inline]
    pub(crate) fn next(&mut self, c: u8) {
        // If the state becomes invalid for any reason, just discard any further input.
        if self.state == State::Invalid {
            return;
        }

        // If a writer has been initialized, we just accumulate the rest of the OSC sequence in the writer's buffer and
        // skip the state machine.
        if let Some(cap) = self.capture.as_mut() {
            let written = match &mut cap.backing {
                // The fixed unknown capture is a shorter slice of the buffer, so the bound is the capture's
                // `max_bytes`, which is never more than `MAX_BUF`.
                CaptureBacking::Fixed(len) if *len < cap.max_bytes => {
                    self.buffer[*len] = c;
                    *len += 1;
                    true
                }
                CaptureBacking::Fixed(_) => false,
                CaptureBacking::Allocating(_) => cap.write_byte(&mut self.buffer, c),
            };
            if !written {
                // We have overflowed our buffer or had some other error. Discard any further input.
                self.capture_failed();
            }
            return;
        }

        self.next_prefix(c);
    }

    /// The state machine over the OSC number, before a capture begins.
    fn next_prefix(&mut self, c: u8) {
        match (self.state, c) {
            // invalid is handled by `next`; unknown states always have an active capture.
            (State::Invalid | State::Unknown | State::UnknownTruncated, _) => {}

            (State::Start, b'0') => self.state = State::S0,
            (State::Start, b'1') => self.state = State::S1,
            (State::Start, b'2') => self.state = State::S2,
            (State::Start, b'3') => self.state = State::S3,
            (State::Start, b'4') => self.state = State::S4,
            (State::Start, b'5') => self.state = State::S5,
            (State::Start, b'6') => self.state = State::S6,
            (State::Start, b'7') => self.state = State::S7,
            (State::Start, b'8') => self.state = State::S8,
            (State::Start, b'9') => self.state = State::S9,

            (State::S3, b'0') => self.state = State::S30,
            (State::S30, b'0') => self.state = State::S300,
            (State::S300, b'8') => self.state = State::S3008,

            (State::S1, b'0') => self.state = State::S10,
            (State::S1, b'1') => self.state = State::S11,
            (State::S1, b'2') => self.state = State::S12,
            (State::S1, b'3') => self.state = State::S13,
            (State::S1, b'4') => self.state = State::S14,
            (State::S1, b'5') => self.state = State::S15,
            (State::S1, b'6') => self.state = State::S16,
            (State::S1, b'7') => self.state = State::S17,
            (State::S1, b'8') => self.state = State::S18,
            (State::S1, b'9') => self.state = State::S19,

            (State::S10, b'4') => self.state = State::S104,
            (State::S10, b'5') => self.state = State::S105,

            (State::S11, b'0') => self.state = State::S110,
            (State::S11, b'1') => self.state = State::S111,
            (State::S11, b'2') => self.state = State::S112,
            (State::S11, b'3') => self.state = State::S113,
            (State::S11, b'4') => self.state = State::S114,
            (State::S11, b'5') => self.state = State::S115,
            (State::S11, b'6') => self.state = State::S116,
            (State::S11, b'7') => self.state = State::S117,
            (State::S11, b'8') => self.state = State::S118,
            (State::S11, b'9') => self.state = State::S119,

            (State::S13, b'3') => self.state = State::S133,
            (State::S133, b'7') => self.state = State::S1337,

            (State::S2, b'1') => self.state = State::S21,
            (State::S2, b'2') => self.state = State::S22,

            (State::S5, b'2') => self.state = State::S52,
            (State::S5, b'5') => self.state = State::S55,
            (State::S55, b'2') => self.state = State::S552,
            (State::S552, b'2') => self.state = State::S5522,

            (State::S6, b'6') => self.state = State::S66,

            (State::S7, b'2') => self.state = State::S72,
            (State::S7, b'5') => self.state = State::S75,
            (State::S7, b'7') => self.state = State::S77,
            (State::S75, b'0') => self.state = State::S750,
            (State::S750, b'1') => self.state = State::S7501,
            (State::S77, b'7') => self.state = State::S777,

            (State::S9, b'9') => self.state = State::S99,

            (state, b';') => self.begin_body(state),
            (_, _) => self.unknown_or_invalid(Some(c)),
        }
    }

    /// The `;` after the OSC number `state`: start capturing its body, with an allocator check for the color OSCs and
    /// an allocating capture for the ones whose payloads can exceed the fixed buffer.
    fn begin_body(&mut self, state: State) {
        match state {
            State::S1
            | State::S3008
            | State::S2
            | State::S7
            | State::S133
            | State::S1337
            | State::S9
            | State::S0
            | State::S22
            | State::S777
            | State::S8 => self.capture_trailing(CaptureMode::Fixed),

            State::S4
            | State::S5
            | State::S10
            | State::S11
            | State::S12
            | State::S13
            | State::S14
            | State::S15
            | State::S16
            | State::S17
            | State::S18
            | State::S19
            | State::S21
            | State::S104
            | State::S105
            | State::S110
            | State::S111
            | State::S112
            | State::S113
            | State::S114
            | State::S115
            | State::S116
            | State::S117
            | State::S118
            | State::S119 => {
                if self.ensure_allocator() {
                    self.capture_trailing(CaptureMode::Fixed);
                }
            }

            // The protocols allow payloads up to 4096 bytes per chunk plus metadata (OSC 72), report bodies up to 4087
            // bytes (OSC 7501), and encoded payloads (OSC 99) that exceed the fixed buffer.
            State::S52 | State::S66 | State::S72 | State::S7501 | State::S99 | State::S5522 => {
                self.capture_trailing(CaptureMode::Allocating);
            }

            // These only lead to longer numbers (3 leads to 3008), and the others already ended or have a capture.
            State::Start
            | State::Invalid
            | State::Unknown
            | State::UnknownTruncated
            | State::S3
            | State::S30
            | State::S300
            | State::S55
            | State::S552
            | State::S6
            | State::S75
            | State::S750
            | State::S77 => self.unknown_or_invalid(Some(b';')),
        }
    }

    /// End the sequence and return the command it contains, or null if it isn't a valid command.
    ///
    /// `terminator_ch` is the byte that ended the sequence. Commands that reply to the program end their reply the
    /// same way: BEL (0x07) gets a BEL reply, and any other byte, or null, gets an ST reply.
    ///
    /// A program can also cancel a sequence partway through by sending CAN or SUB instead of a terminator. Pass that
    /// byte as `terminator_ch`. The sequence is then discarded and this returns null, whatever command it contained.
    /// This matches xterm.
    ///
    /// The returned command borrows the parser: it is only valid until the next call to the parser. Copy out any data
    /// you need to keep.
    pub(crate) fn end(&mut self, terminator_ch: Option<u8>) -> Option<Command<'_>> {
        // std.ascii.control_code.can, std.ascii.control_code.sub
        if matches!(terminator_ch, Some(0x18 | 0x1A)) {
            return None;
        }

        match self.state {
            State::Start | State::Invalid => None,

            State::Unknown | State::UnknownTruncated => parsers::unknown::parse(self, terminator_ch),

            // These states only lead to longer numbers (3 leads to 3008) and are not OSCs themselves, so ending on one
            // is unknown.
            State::S3
            | State::S30
            | State::S300
            | State::S55
            | State::S552
            | State::S6
            | State::S75
            | State::S750
            | State::S77 => {
                self.unknown_or_invalid(None);
                if self.state == State::Invalid {
                    return None;
                }
                parsers::unknown::parse(self, terminator_ch)
            }

            State::S0 | State::S2 => parsers::change_window_title::parse(self, terminator_ch),

            State::S1 => parsers::change_window_icon::parse(self, terminator_ch),

            State::S4
            | State::S5
            | State::S10
            | State::S11
            | State::S12
            | State::S13
            | State::S14
            | State::S15
            | State::S16
            | State::S17
            | State::S18
            | State::S19
            | State::S104
            | State::S105
            | State::S110
            | State::S111
            | State::S112
            | State::S113
            | State::S114
            | State::S115
            | State::S116
            | State::S117
            | State::S118
            | State::S119 => parsers::color::parse(self, terminator_ch),

            State::S7 => parsers::report_pwd::parse(self, terminator_ch),
            State::S8 => parsers::hyperlink::parse(self, terminator_ch),
            State::S9 => parsers::osc9::parse(self, terminator_ch),
            State::S21 => parsers::kitty_color::parse(self, terminator_ch),
            State::S22 => parsers::mouse_shape::parse(self, terminator_ch),
            State::S52 => parsers::clipboard_operation::parse(self, terminator_ch),
            State::S3008 => parsers::context_signal::parse(self, terminator_ch),
            State::S66 => parsers::kitty_text_sizing::parse(self, terminator_ch),
            State::S72 => parsers::kitty_dnd_protocol::parse(self, terminator_ch),
            State::S99 => parsers::kitty_desktop_notification::parse(self, terminator_ch),
            State::S7501 => parsers::program_status::parse(self, terminator_ch),
            State::S133 => parsers::semantic_prompt::parse(self, terminator_ch),
            State::S777 => parsers::rxvt_extension::parse(self, terminator_ch),
            State::S1337 => parsers::iterm2::parse(self, terminator_ch),
            State::S5522 => parsers::kitty_clipboard_protocol::parse(self, terminator_ch),
        }
    }
}

#[cfg(test)]
mod tests;
