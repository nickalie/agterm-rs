//! The VT stream: bytes from the pty decoded (UTF-8), parsed (the [`Parser`] state machine) and dispatched to a
//! [`Handler`] as terminal actions.
//!
//! Ghostty: `src/terminal/stream.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/stream.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's `handler.vt(comptime action, value)` is a [`Handler`] trait with one method per action, each a
//! no-op by default; Zig's `Action` union and its C forms are not here, its payload structs are. The stream owns its
//! handler, and dropping the stream drops it (Zig's `deinit`). The allocator option is [`Options::alloc`], whether
//! the OSC parser and the continuation tracker may allocate. Cut, with the branches that reached them: the
//! inspector's raw-action hook (`vtRaw`, which only the cut inspector declares, so every fast path is taken),
//! DECRQCRA and XTCHECKSUM (`xt_checksum`, which libghostty-vt answers only on an option agterm never sets; the two
//! CSIs fall through as unimplemented) and the per-byte debug mode. The fast paths are the SIMD ones libghostty-vt
//! builds by default, over the scalar decoder of [`crate::simd::vt`]; their vector scans are portable loops LLVM
//! vectorizes. The CSI, OSC and ESC dispatch are in [`csi_dispatch`] and [`dispatch`]. Drift absorbed: `6104c78f1`
//! (CSI 19 t), `d0c0d4008` (TBC with no parameter); `fbda61b87` (DECXCPR) reaches the stream through
//! [`device_status::req_from_int`].

use std::io;

use crate::terminal::mouse::Shape as MouseShape;
use crate::terminal::parser::{self, Parser};
use crate::terminal::stream_continuation::{Pending, Tracker};
use crate::terminal::utf8_decoder::Utf8Decoder;
use crate::terminal::{ansi, charsets, csi, device_attributes, device_status, kitty, modes, osc, sgr};

mod csi_dispatch;
mod dispatch;

/// The log target of the stream (Ghostty's `.stream` scope).
const LOG_SCOPE: &str = "stream";

/// `std.simd.suggestVectorLength(u8)`: the bytes the APC and OSC payload scans test at a time, or `None` where the
/// target has no usable vectors and only the scalar loop runs.
const PAYLOAD_SCAN_LANES: Option<usize> = match crate::simd::native_vector_bits() {
    Some(bits) => Some(bits / 8),
    None => None,
};

/// `simd.lanes(u32)`: the code points the printable-run scan tests at a time.
const PRINT_RUN_LANES: Option<usize> = crate::simd::lanes(core::mem::size_of::<u32>());

/// The most code points one bulk decode produces.
///
/// This is somewhat arbitrary so if someone can demonstrate a better number then we can switch.
const CP_BUF_LEN: usize = 4096;

/// Whether some byte of one APC scan group is not an `apc_put` byte (CAN 0x18, SUB 0x1A, ESC 0x1B, or `>= 0x80`).
///
/// The non-short-circuit `|` over the group and the reduction as an unsigned max of 0/0xFF lane masks are what LLVM
/// lowers to vector compares, as Zig's `@Vector` expression is.
#[inline]
fn apc_group_invalid(bytes: &[u8]) -> bool {
    let mut any: u8 = 0;
    for &b in bytes {
        let invalid = (b == 0x18) || (b == 0x1A) || (b == 0x1B) || (b >= 0x80);
        any = any.max(u8::from(invalid).wrapping_neg());
    }
    any != 0
}

/// Whether some byte of one OSC scan group is not an `osc_put` byte (a C0 control).
#[inline]
fn osc_group_invalid(bytes: &[u8]) -> bool {
    let mut stop = false;
    for &b in bytes {
        stop |= b < 0x20;
    }
    stop
}

/// Whether a code point is a C0 control or a C1 control decoded from UTF-8: exactly the code points with no bit set
/// outside 0x9F (bits 0-4 and bit 7 cover 0x00-0x1F and 0x80-0x9F and nothing else), so one AND-test classifies both
/// ranges.
#[inline]
const fn is_control(cp: u32) -> bool {
    cp & !0x9F == 0
}

/// `Action.CursorMovement`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorMovement {
    /// The value of the cursor movement. Depending on the action this may be an absolute value or it may be a
    /// relative value. For example, `cursor_up` is relative, but `cursor_row` is absolute.
    pub(crate) value: u16,
}

/// `Action.CursorPos`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorPos {
    pub(crate) row: u16,
    pub(crate) col: u16,
}

/// `Action.InvokeCharset`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvokeCharset {
    pub(crate) bank: charsets::ActiveSlot,
    pub(crate) charset: charsets::Slots,
    pub(crate) locking: bool,
}

/// `Action.ConfigureCharset`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConfigureCharset {
    pub(crate) slot: charsets::Slots,
    pub(crate) charset: charsets::Charset,
}

/// `Action.Mode`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mode {
    pub(crate) mode: modes::Mode,
}

/// `Action.RawMode`: a mode the stream does not know, for DECRQM's report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawMode {
    pub(crate) mode: u16,
    pub(crate) ansi: bool,
}

/// `Action.Margin`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Margin {
    pub(crate) top_left: u16,
    pub(crate) bottom_right: u16,
}

/// `Action.ResizeWindow`
///
/// A request to resize the window's text area (CSI 8 t). A value of zero means the parameter was omitted or zero, and
/// the current size for that dimension should be kept. xterm treats an explicit zero as the screen size, but we
/// can't distinguish it from omitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResizeWindow {
    pub(crate) rows: u16,
    pub(crate) columns: u16,
}

/// `Action.DeviceStatus`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeviceStatus {
    pub(crate) request: device_status::Request,
}

/// `Action.KittyKeyboardFlags`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KittyKeyboardFlags {
    pub(crate) flags: kitty::key::Flags,
}

/// `Action.WindowTitle`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WindowTitle<'a> {
    pub(crate) title: &'a [u8],
}

/// `Action.ReportPwd`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReportPwd<'a> {
    pub(crate) url: &'a [u8],
}

/// `Action.ShowDesktopNotification`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShowDesktopNotification<'a> {
    pub(crate) title: &'a [u8],
    pub(crate) body: &'a [u8],
}

/// `Action.StartHyperlink`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StartHyperlink<'a> {
    pub(crate) uri: &'a [u8],
    pub(crate) id: Option<&'a [u8]>,
}

/// `Action.ClipboardContents`: the terminator rides along so an OSC 52 read response can echo it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClipboardContents<'a> {
    pub(crate) kind: u8,
    pub(crate) data: &'a [u8],
    pub(crate) terminator: osc::Terminator,
}

/// `Action.ApcEnd`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ApcEnd {
    /// False when CAN, SUB, or another aborting transition ended the APC.
    pub(crate) terminated: bool,
}

/// `Action.ColorOperation`
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ColorOperation<'a> {
    pub(crate) op: osc::color::Operation,
    pub(crate) requests: &'a osc::color::List,
    pub(crate) terminator: osc::Terminator,
}

/// The receiver of a stream's actions. Every action is a no-op unless the handler implements it, as a Zig handler's
/// `vt` ignores the actions it does not switch on.
///
/// Printable text is delivered as `print_slice` (runs of code points) whenever the stream can decode several code
/// points at once, and as `print` otherwise. Handlers that care about text must handle both.
pub(crate) trait Handler {
    /// Print one code point.
    fn print(&mut self, _cp: u32) {}

    /// Print a run of printable code points; by default one `print` each.
    fn print_slice(&mut self, cps: &[u32]) {
        for &cp in cps {
            self.print(cp);
        }
    }

    /// REP: repeat the previous character.
    fn print_repeat(&mut self, _count: usize) {}

    /// BEL
    fn bell(&mut self) {}

    /// BS
    fn backspace(&mut self) {}

    /// HT and CHT
    fn horizontal_tab(&mut self, _count: u16) {}

    /// CBT
    fn horizontal_tab_back(&mut self, _count: u16) {}

    /// LF, VT and FF
    fn linefeed(&mut self) {}

    /// CR
    fn carriage_return(&mut self) {}

    /// ENQ
    fn enquiry(&mut self) {}

    /// SO, SI and the single and locking shifts
    fn invoke_charset(&mut self, _v: InvokeCharset) {}

    /// CUU
    fn cursor_up(&mut self, _v: CursorMovement) {}

    /// CUD
    fn cursor_down(&mut self, _v: CursorMovement) {}

    /// CUB
    fn cursor_left(&mut self, _v: CursorMovement) {}

    /// CUF
    fn cursor_right(&mut self, _v: CursorMovement) {}

    /// HPA
    fn cursor_col(&mut self, _v: CursorMovement) {}

    /// VPA
    fn cursor_row(&mut self, _v: CursorMovement) {}

    /// HPR
    fn cursor_col_relative(&mut self, _v: CursorMovement) {}

    /// VPR
    fn cursor_row_relative(&mut self, _v: CursorMovement) {}

    /// CUP
    fn cursor_pos(&mut self, _v: CursorPos) {}

    /// DECSCUSR
    fn cursor_style(&mut self, _v: ansi::CursorStyle) {}

    /// ED 0
    fn erase_display_below(&mut self, _protected: bool) {}

    /// ED 1
    fn erase_display_above(&mut self, _protected: bool) {}

    /// ED 2
    fn erase_display_complete(&mut self, _protected: bool) {}

    /// ED 3
    fn erase_display_scrollback(&mut self, _protected: bool) {}

    /// ED 22 (Kitty)
    fn erase_display_scroll_complete(&mut self, _protected: bool) {}

    /// EL 0
    fn erase_line_right(&mut self, _protected: bool) {}

    /// EL 1
    fn erase_line_left(&mut self, _protected: bool) {}

    /// EL 2
    fn erase_line_complete(&mut self, _protected: bool) {}

    /// EL 4, which the stream never decodes from the wire
    fn erase_line_right_unless_pending_wrap(&mut self, _protected: bool) {}

    /// DCH
    fn delete_chars(&mut self, _count: usize) {}

    /// ECH
    fn erase_chars(&mut self, _count: usize) {}

    /// IL
    fn insert_lines(&mut self, _count: usize) {}

    /// ICH
    fn insert_blanks(&mut self, _count: usize) {}

    /// DL
    fn delete_lines(&mut self, _count: usize) {}

    /// SU
    fn scroll_up(&mut self, _count: usize) {}

    /// SD
    fn scroll_down(&mut self, _count: usize) {}

    /// TBC 0, CTC 2
    fn tab_clear_current(&mut self) {}

    /// TBC 3, CTC 5
    fn tab_clear_all(&mut self) {}

    /// HTS, CTC 0
    fn tab_set(&mut self) {}

    /// DECST8C
    fn tab_reset(&mut self) {}

    /// IND
    fn index(&mut self) {}

    /// NEL
    fn next_line(&mut self) {}

    /// RI
    fn reverse_index(&mut self) {}

    /// RIS
    fn full_reset(&mut self) {}

    /// SM, DECSET, DECKPAM
    fn set_mode(&mut self, _v: Mode) {}

    /// RM, DECRST, DECKPNM
    fn reset_mode(&mut self, _v: Mode) {}

    /// XTSAVE
    fn save_mode(&mut self, _v: Mode) {}

    /// XTRESTORE
    fn restore_mode(&mut self, _v: Mode) {}

    /// DECRQM of a known mode
    fn request_mode(&mut self, _v: Mode) {}

    /// DECRQM of a mode the stream does not know
    fn request_mode_unknown(&mut self, _v: RawMode) {}

    /// DECSTBM
    fn top_and_bottom_margin(&mut self, _v: Margin) {}

    /// DECSLRM
    fn left_and_right_margin(&mut self, _v: Margin) {}

    /// `CSI s` with no parameters: DECSLRM when mode 69 is set, SCOSC otherwise
    fn left_and_right_margin_ambiguous(&mut self) {}

    /// DECSC
    fn save_cursor(&mut self) {}

    /// DECRC, SCORC
    fn restore_cursor(&mut self) {}

    /// XTMODKEYS
    fn modify_key_format(&mut self, _v: ansi::ModifyKeyFormat) {}

    /// XTSHIFTESCAPE
    fn mouse_shift_capture(&mut self, _capture: bool) {}

    /// DECSCA 0, EPA
    fn protected_mode_off(&mut self) {}

    /// SPA
    fn protected_mode_iso(&mut self) {}

    /// DECSCA 1
    fn protected_mode_dec(&mut self) {}

    /// XTWINOPS 14, 16, 18, 19 and 21
    fn size_report(&mut self, _v: csi::SizeReportStyle) {}

    /// XTWINOPS 22
    fn title_push(&mut self, _index: u16) {}

    /// XTWINOPS 23
    fn title_pop(&mut self, _index: u16) {}

    /// XTVERSION
    fn xtversion(&mut self) {}

    /// DA1, DA2, DA3, DECID
    fn device_attributes(&mut self, _req: device_attributes::Req) {}

    /// DSR
    fn device_status(&mut self, _v: DeviceStatus) {}

    /// `CSI ? u`
    fn kitty_keyboard_query(&mut self) {}

    /// `CSI > flags u`
    fn kitty_keyboard_push(&mut self, _v: KittyKeyboardFlags) {}

    /// `CSI < n u`
    fn kitty_keyboard_pop(&mut self, _n: u16) {}

    /// `CSI = flags ; 1 u`
    fn kitty_keyboard_set(&mut self, _v: KittyKeyboardFlags) {}

    /// `CSI = flags ; 2 u`
    fn kitty_keyboard_set_or(&mut self, _v: KittyKeyboardFlags) {}

    /// `CSI = flags ; 3 u`
    fn kitty_keyboard_set_not(&mut self, _v: KittyKeyboardFlags) {}

    /// The start of a DCS string.
    fn dcs_hook(&mut self, _dcs: &parser::Dcs) {}

    /// One byte of a DCS string.
    fn dcs_put(&mut self, _code: u8) {}

    /// The end of a DCS string.
    fn dcs_unhook(&mut self) {}

    /// The start of an APC string.
    fn apc_start(&mut self) {}

    /// One byte of an APC string.
    fn apc_put(&mut self, _code: u8) {}

    /// A run of APC string bytes; by default one `apc_put` each.
    fn apc_put_slice(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.apc_put(b);
        }
    }

    /// The end of an APC string.
    fn apc_end(&mut self, _v: ApcEnd) {}

    /// OSC 8 with a URI
    fn start_hyperlink(&mut self, _v: StartHyperlink<'_>) {}

    /// OSC 8 without one
    fn end_hyperlink(&mut self) {}

    /// DECSASD
    fn active_status_display(&mut self, _v: ansi::StatusDisplay) {}

    /// DECALN
    fn decaln(&mut self) {}

    /// OSC 0 and 2
    fn window_title(&mut self, _v: WindowTitle<'_>) {}

    /// OSC 7
    fn report_pwd(&mut self, _v: ReportPwd<'_>) {}

    /// OSC 9 and 777
    fn show_desktop_notification(&mut self, _v: ShowDesktopNotification<'_>) {}

    /// OSC 9;4 (`ConEmu`)
    fn progress_report(&mut self, _v: osc::ProgressReport) {}

    /// OSC 52
    fn clipboard_contents(&mut self, _v: ClipboardContents<'_>) {}

    /// OSC 22
    fn mouse_shape(&mut self, _v: MouseShape) {}

    /// An empty OSC 22
    fn mouse_shape_reset(&mut self) {}

    /// SCS
    fn configure_charset(&mut self, _v: ConfigureCharset) {}

    /// SGR
    fn set_attribute(&mut self, _attr: sgr::Attribute<'_>) {}

    /// OSC 21 (Kitty)
    fn kitty_color_report(&mut self, _v: &kitty::color::Osc) {}

    /// OSC 4, 5, 10-19, 104, 105, 110-119
    fn color_operation(&mut self, _v: ColorOperation<'_>) {}

    /// OSC 133
    fn semantic_prompt(&mut self, _v: osc::SemanticPrompt<'_>) {}

    /// OSC 5522 (Kitty)
    fn kitty_clipboard(&mut self, _v: osc::KittyClipboardProtocol<'_>) {}

    /// OSC 72 (Kitty)
    fn kitty_dnd(&mut self, _v: osc::KittyDndProtocol<'_>) {}

    /// XTWINOPS 8
    fn resize_window(&mut self, _v: ResizeWindow) {}

    /// An OSC whose number the parser does not implement.
    fn osc_unknown(&mut self, _v: osc::Unknown<'_>) {}

    /// OSC 7501
    fn program_status(&mut self, _v: osc::ProgramStatus<'_>) {}

    /// DECSTR
    fn soft_reset(&mut self) {}
}

/// A stream of tty control characters dispatched to `H`.
pub(crate) struct Stream<H: Handler> {
    pub(crate) handler: H,
    pub(crate) parser: Parser,
    pub(crate) utf8decoder: Utf8Decoder,
    pub(crate) continuation: Option<Tracker>,
}

/// How a stream is set up.
pub(crate) struct Options<H> {
    /// The handler.
    pub(crate) handler: H,

    /// Whether the stream may allocate (Zig's optional allocator). Without it the stream is fully allocation free
    /// and some operations are dropped, such as OSC 52 clipboard operations.
    pub(crate) alloc: bool,

    /// Maximum size in bytes of the continuation suffix. If this is `None` or zero then continuation tracking is
    /// disabled; it is also disabled without [`Self::alloc`]. Feeding this continuation suffix into an equivalent
    /// stream at ground reconstructs the unfinished state without repeating committed terminal effects. Continuation
    /// tracking is only supported by the terminal's own stream handler.
    pub(crate) continuation_max_bytes: Option<usize>,

    /// The most bytes to keep from each OSC sequence whose number the OSC parser does not implement. Zero, the
    /// default, discards these sequences. Any other value sends them to the handler as `osc_unknown` actions. See
    /// [`osc::Parser::unknown_max_bytes`] for how the limit behaves.
    ///
    /// This only affects OSC. Other kinds of unknown sequences, such as APC, are collected by the handler and have
    /// their own limits there.
    pub(crate) osc_unknown_max_bytes: usize,
}

impl<H> Options<H> {
    /// The options of `handler`, with Zig's field defaults for the rest: no allocation, no continuation tracking,
    /// unknown OSCs discarded.
    pub(crate) const fn with_handler(handler: H) -> Self {
        Self {
            handler,
            alloc: false,
            continuation_max_bytes: None,
            osc_unknown_max_bytes: 0,
        }
    }
}

/// Why [`Stream::write_continuation`] wrote nothing.
#[derive(Debug)]
pub(crate) enum WriteContinuationError {
    /// The writer failed (`error.WriteFailed`).
    WriteFailed(io::Error),
    /// The stream does not track its continuation.
    ContinuationDisabled,
    /// The suffix was lost: it outgrew its limit or its growth failed.
    ContinuationUnavailable,
}

impl<H: Handler> Stream<H> {
    /// A stream over `options.handler`. Without allocation, operations that require heap allocation are dropped: OSC
    /// 52 (clipboard operations), for example, can be arbitrarily large.
    pub(crate) fn new(options: Options<H>) -> Self {
        let mut parser = Parser::new();
        parser.osc.alloc = options.alloc;
        parser.osc.unknown_max_bytes = options.osc_unknown_max_bytes;

        let continuation = match options.continuation_max_bytes {
            Some(max_bytes) if options.alloc && max_bytes > 0 => Some(Tracker::new(max_bytes)),
            _ => None,
        };

        Self {
            handler: options.handler,
            parser,
            utf8decoder: Utf8Decoder::default(),
            continuation,
        }
    }

    /// Write the current continuation suffix directly to a caller-owned writer. The caller must pause and serialize
    /// access to this stream.
    pub(crate) fn write_continuation(&self, writer: &mut dyn io::Write) -> Result<(), WriteContinuationError> {
        let tracker = self
            .continuation
            .as_ref()
            .ok_or(WriteContinuationError::ContinuationDisabled)?;
        if tracker.broken {
            return Err(WriteContinuationError::ContinuationUnavailable);
        }
        tracker.write(writer).map_err(WriteContinuationError::WriteFailed)
    }

    /// True when no continuation suffix is needed to reproduce the stream's current parsing state.
    #[inline]
    pub(crate) fn ground(&self) -> bool {
        // Parser ground alone is not sufficient because the UTF-8 decoder may have some state.
        self.parser.state == parser::State::Ground && self.utf8decoder.state == 0
    }

    /// Update the continuation suffix after one complete feed call.
    fn track_continuation(&mut self, input: &[u8]) {
        let ground = self.ground();
        let pending = if self.parser.state == parser::State::Ground {
            Pending::Utf8
        } else {
            Pending::Vt
        };
        let Some(tracker) = self.continuation.as_mut() else {
            return;
        };

        // If we're in a ground state, we have no continuation suffix to track by definition.
        if ground {
            tracker.reset();
            return;
        }

        // Retain the part of this feed needed to replay the unfinished state. When the parser is grounded here, the
        // feed must have ended inside a UTF-8 codepoint instead, because the ground check above covers both state
        // machines.
        tracker.append(pending, input);
    }

    /// Process a string of characters.
    pub(crate) fn next_slice(&mut self, input: &[u8]) {
        self.next_slice_untracked(input);

        // Continuation tracking is opt-in and this branch predicts perfectly, so disabled streams pay nothing else
        // here.
        if self.continuation.is_some() {
            self.track_continuation(input);
        }
    }

    /// Process a string of characters, but only the shortest prefix needed to reach the ground state.
    ///
    /// The ground state is when the stream isn't in the middle of any type of sequence: UTF-8, ESC, CSI, OSC, etc. It
    /// is the stateless point of the stream.
    ///
    /// If the stream is already at ground then this consumes nothing and returns zero. `Some` is the number of bytes
    /// consumed before reaching ground, including the byte that reaches it. `None` means the full slice was consumed
    /// without reaching ground.
    ///
    /// This is anywhere from 1% to 5% slower than `next_slice`, depending on the types of inputs provided (e.g. OSC vs
    /// APC, whether it ever reaches ground, etc.).
    pub(crate) fn next_slice_until_ground(&mut self, input: &[u8]) -> Option<usize> {
        let consumed = self.next_slice_until_ground_untracked(input);
        let consumed_len = consumed.unwrap_or(input.len());
        if self.continuation.is_some() && consumed_len > 0 {
            self.track_continuation(&input[..consumed_len]);
        }
        consumed
    }

    /// [`Self::next_slice_until_ground`] without the continuation tracking.
    fn next_slice_until_ground_untracked(&mut self, input: &[u8]) -> Option<usize> {
        if self.ground() {
            return Some(0);
        }

        // Process UTF-8 if we're within that state.
        let mut offset: usize = 0;
        while self.utf8decoder.state != 0 {
            let &c = input.get(offset)?;
            self.next_utf8(c);
            offset += 1;
        }

        // Process non-UTF-8
        if self.parser.state != parser::State::Ground {
            offset += self.consume_until_ground(&input[offset..]);
        }

        self.ground().then_some(offset)
    }

    /// [`Self::next_slice`] without the continuation tracking.
    fn next_slice_untracked(&mut self, input: &[u8]) {
        // The batched path decodes UTF-8 in bulk and hands printable runs to the handler as print_slice actions.
        let mut cp_buf = [0_u32; CP_BUF_LEN];

        // Split the input into chunks that fit into cp_buf.
        for chunk in input.chunks(CP_BUF_LEN) {
            self.next_slice_capped(chunk, &mut cp_buf);
        }
    }

    /// One chunk of [`Self::next_slice`], at most `cp_buf.len()` bytes.
    fn next_slice_capped(&mut self, input: &[u8], cp_buf: &mut [u32]) {
        debug_assert!(input.len() <= cp_buf.len());

        let mut offset: usize = 0;

        // If the scalar UTF-8 decoder was in the middle of processing a code sequence, we continue until it's not.
        while self.utf8decoder.state != 0 {
            let Some(&c) = input.get(offset) else {
                return;
            };
            self.next_utf8(c);
            offset += 1;
        }
        if offset >= input.len() {
            return;
        }

        // If we're not in the ground state then we process until we are. This can happen if the last chunk of input
        // put us in the middle of a control sequence.
        offset += self.consume_until_ground(&input[offset..]);
        if offset >= input.len() {
            return;
        }
        offset += self.consume_all_escapes(&input[offset..]);

        // If we're in the ground state then we can decode in bulk until we see an ESC (0x1B), since all other
        // characters up to that point are just UTF-8.
        while self.parser.state == parser::State::Ground && offset < input.len() {
            let res = crate::simd::vt::utf8_decode_until_control_seq(&input[offset..], cp_buf);
            print_decoded(&mut self.handler, &cp_buf[..res.decoded]);

            // Consume the bytes we just processed.
            offset += res.consumed;
            if offset >= input.len() {
                return;
            }

            // If our offset is NOT an escape then we must have a partial UTF-8 sequence. In that case, we pass it off
            // to the scalar parser.
            if input[offset] != 0x1B {
                for &c in &input[offset..] {
                    self.next_utf8(c);
                }
                return;
            }

            // Process control sequences until we run out.
            offset += self.consume_all_escapes(&input[offset..]);
        }
    }

    /// Parses back-to-back escape sequences until none are left. Returns the number of bytes consumed from the
    /// provided input.
    ///
    /// Expects input to start with 0x1B, use `consume_until_ground` first if the stream may be in the middle of an
    /// escape sequence.
    fn consume_all_escapes(&mut self, input: &[u8]) -> usize {
        let mut offset: usize = 0;
        while input.get(offset) == Some(&0x1B) {
            self.parser.state = parser::State::Escape;
            self.parser.clear();
            offset += 1;
            offset += self.consume_until_ground(&input[offset..]);
            if offset >= input.len() {
                return input.len();
            }
        }
        offset
    }

    /// Parses escape sequences until the parser reaches the ground state. Returns the number of bytes consumed from
    /// the provided input.
    fn consume_until_ground(&mut self, input: &[u8]) -> usize {
        let mut offset: usize = 0;
        while self.parser.state != parser::State::Ground {
            let Some(&c) = input.get(offset) else {
                return input.len();
            };
            match self.fast_path(&input[offset..]) {
                FastPath::Consumed(n) => offset += n,
                FastPath::Exhausted => return input.len(),
                FastPath::None => {
                    self.next_non_utf8(c);
                    offset += 1;
                }
            }
        }
        offset
    }

    /// The bulk fast paths of [`Self::consume_until_ground`] for the states that dominate escape sequences: CSI entry
    /// and parameters, and the APC and OSC payloads. `input` is not empty.
    #[inline]
    fn fast_path(&mut self, input: &[u8]) -> FastPath {
        let c = input[0];
        let state = self.parser.state;
        match state {
            // "ESC [" is by far the most common escape sequence prefix, so handle the '[' and the byte that follows
            // it here rather than paying a next_non_utf8 call for each.
            parser::State::Escape if c == b'[' => {
                self.parser.state = parser::State::CsiEntry;
                FastPath::Consumed(1)
            }
            parser::State::CsiEntry if self.csi_entry_byte(c) => FastPath::Consumed(1),
            // Bulk-consume CSI parameter bytes. If we're still in csi_param after them, the next byte isn't a
            // parameter byte and next_non_utf8 handles it.
            parser::State::CsiParam => match self.consume_csi_params(input) {
                n if n >= input.len() => FastPath::Exhausted,
                0 => FastPath::None,
                n => FastPath::Consumed(n),
            },
            // Bulk-consume APC string bytes into a single slice. APC payloads (e.g. Kitty graphics) can be megabytes
            // of base64 data, so per-byte dispatch is far too slow.
            parser::State::SosPmApcString => {
                let n = self.consume_apc_string(input);
                self.end_string(input, n, Self::end_apc)
            }
            // Bulk-consume OSC string bytes into the OSC parser. OSC payloads (e.g. OSC 52 clipboard operations and
            // the kitty clipboard protocol) can be megabytes of base64 data, so per-byte dispatch is far too slow.
            parser::State::OscString => {
                let n = self.consume_osc_string(input);
                self.end_string(input, n, Self::end_osc)
            }
            _ => FastPath::None,
        }
    }

    /// After `n` bulk-consumed payload bytes, the fast path of the string's normal termination by the byte that
    /// follows them (`end`, which returns whether it handled that byte).
    #[inline]
    fn end_string(&mut self, input: &[u8], n: usize, end: fn(&mut Self, u8) -> bool) -> FastPath {
        let Some(&next) = input.get(n) else {
            return FastPath::Exhausted;
        };
        if end(self, next) {
            FastPath::Consumed(n + 1)
        } else if n > 0 {
            FastPath::Consumed(n)
        } else {
            // Aborting transitions and ignored bytes go through the state machine.
            FastPath::None
        }
    }

    /// Fast-path normal APC termination. This matches `Parser::next`'s exit and entry actions while avoiding the
    /// generic action loop for every completed APC. Aborting transitions need the scalar path so the handler can
    /// distinguish them from terminators.
    fn end_apc(&mut self, c: u8) -> bool {
        match c {
            0x1B => {
                self.parser.clear();
                self.parser.state = parser::State::Escape;
            }
            0x9C => self.parser.state = parser::State::Ground,
            _ => return false,
        }
        self.handler.apc_end(ApcEnd { terminated: true });
        true
    }

    /// Fast-path normal OSC termination. This matches `Parser::next`'s exit and entry actions while avoiding the
    /// generic action loop for every completed OSC. CAN/SUB abort and ignored C0 bytes go through the state machine.
    fn end_osc(&mut self, c: u8) -> bool {
        let state = match c {
            // The sequence should be terminated by a full ST ("ESC \"); the "\" that should follow is dispatched as a
            // normal escape sequence.
            0x1B => parser::State::Escape,
            // BEL terminates the string directly.
            0x07 => parser::State::Ground,
            _ => return false,
        };
        if let Some(cmd) = self.parser.osc.end(Some(c)) {
            dispatch::osc_dispatch(&mut self.handler, cmd);
        }
        if state == parser::State::Escape {
            self.parser.clear();
        }
        self.parser.state = state;
        true
    }

    /// Bulk-consume APC string bytes and dispatch them as a single `apc_put_slice` action. Returns the number of
    /// bytes consumed. Stops at the first byte that is not an `apc_put` byte in the parse table, leaving it for the
    /// caller to process through the state machine. CAN, SUB, ESC, and most C1 bytes exit or abort the string state;
    /// 0xA0-0xFF are ignored by the table (not payload), so they can't be bulk-consumed either.
    fn consume_apc_string(&mut self, input: &[u8]) -> usize {
        debug_assert_eq!(self.parser.state, parser::State::SosPmApcString);
        let mut end: usize = 0;

        // Scan ordinary APC bytes a vector at a time, keeping the scalar loop below as the tail and as the whole scan
        // on targets with no recommended vector width.
        if let Some(lanes) = PAYLOAD_SCAN_LANES {
            while let Some(group) = input.get(end..end + lanes) {
                if apc_group_invalid(group) {
                    break;
                }
                end += lanes;
            }
        }
        // Not apc_put bytes: CAN/SUB/ESC and most C1 exit or abort the state; 0xA0-0xFF are ignored by it.
        end += input[end..]
            .iter()
            .position(|&b| matches!(b, 0x18 | 0x1A | 0x1B | 0x80..=0xFF))
            .unwrap_or(input.len() - end);

        if end > 0 {
            self.handler.apc_put_slice(&input[..end]);
        }
        end
    }

    /// Bulk-consume OSC string bytes into the OSC parser. Returns the number of bytes consumed. Stops at the first
    /// byte that is not an `osc_put` byte in the parse table, leaving it for the caller to process through the state
    /// machine. Every byte >= 0x20 is an `osc_put` byte; bytes below either terminate or abort the string (BEL, CAN,
    /// SUB, ESC) or are ignored.
    fn consume_osc_string(&mut self, input: &[u8]) -> usize {
        debug_assert_eq!(self.parser.state, parser::State::OscString);
        let mut end: usize = 0;

        // Scan ordinary OSC bytes a vector at a time, keeping the scalar loop below as the tail and as the whole scan
        // on targets with no recommended vector width (mirroring the APC path).
        if let Some(lanes) = PAYLOAD_SCAN_LANES {
            while let Some(group) = input.get(end..end + lanes) {
                if osc_group_invalid(group) {
                    break;
                }
                end += lanes;
            }
        }
        // Not osc_put bytes: BEL/CAN/SUB/ESC terminate or abort the state; other C0 bytes are ignored by it.
        end += input[end..].iter().position(|&b| b < 0x20).unwrap_or(input.len() - end);

        if end > 0 {
            self.parser.osc.next_slice(&input[..end]);
        }
        end
    }

    /// Fast path for a byte in the `csi_entry` state, the state right after "ESC [". Virtually every CSI sequence
    /// spends exactly one byte in this state, on either a digit, a private marker, or a final byte. Returns true if
    /// the byte was fully handled; false means the caller must process it through the general state machine.
    #[inline]
    fn csi_entry_byte(&mut self, c: u8) -> bool {
        debug_assert_eq!(self.parser.state, parser::State::CsiEntry);
        let p = &mut self.parser;
        match c {
            // First parameter digit.
            b'0'..=b'9' => {
                p.state = parser::State::CsiParam;
                // param_acc is zero (cleared on escape entry) so accumulating is just the digit value.
                p.param_acc = u16::from(c - b'0');
                p.param_acc_idx = 1;
            }
            // An empty first parameter.
            b';' => {
                p.state = parser::State::CsiParam;
                p.params[0] = 0;
                p.params_idx = 1;
            }
            // Private marker (e.g. '?' in "ESC [ ? 2004 h").
            0x3C..=0x3F => {
                p.state = parser::State::CsiParam;
                p.collect(c);
            }
            // A final byte: a parameterless CSI.
            0x40..=0x7E => self.csi_dispatch_final(c),
            // Defer to the state machine for anything else (C0 controls, intermediates, colon).
            _ => return false,
        }
        true
    }

    /// Bulk-consume CSI parameter bytes (digits and separators) and, if reached, the final byte (dispatching the
    /// CSI). Returns the number of bytes consumed. Stops at the first byte that isn't handled here, leaving the
    /// parser in the `csi_param` state so the caller can process that byte.
    fn consume_csi_params(&mut self, input: &[u8]) -> usize {
        debug_assert_eq!(self.parser.state, parser::State::CsiParam);
        for (offset, &c) in input.iter().enumerate() {
            match c {
                b'0'..=b'9' | b':' | b';' => csi_param_byte(&mut self.parser, c),
                // A final byte: dispatch the CSI.
                0x40..=0x7E => {
                    self.csi_dispatch_final(c);
                    return offset + 1;
                }
                // Anything else (C0 controls, intermediates, etc.) is handled by the caller.
                _ => return offset,
            }
        }
        input.len()
    }

    /// Finalize and dispatch a CSI directly from parser state for the fast paths, without going through
    /// `Parser::next`. This must match the behavior of the parser's `csi_dispatch` action.
    fn csi_dispatch_final(&mut self, c: u8) {
        let p = &mut self.parser;
        p.state = parser::State::Ground;

        // Ignore sequences with too many parameters, matching the parser's behavior of dropping the dispatch
        // entirely.
        if usize::from(p.params_idx) >= parser::MAX_PARAMS {
            return;
        }

        // Finalize the last parameter if we have one.
        if p.param_acc_idx > 0 {
            p.params[usize::from(p.params_idx)] = p.param_acc;
            p.params_idx += 1;
        }

        let action = parser::Csi {
            intermediates: parser::InlineVec::from_buf(p.intermediates, p.intermediates_idx),
            params: parser::InlineVec::from_buf(p.params, p.params_idx),
            params_sep: p.params_sep,
            final_: c,
        };

        // We only allow colon or mixed separators for the 'm' command.
        if c != b'm' && p.params_sep.count() > 0 {
            tracing::warn!(
                target: LOG_SCOPE,
                "CSI colon or mixed separators only allowed for 'm' command, got: {action}"
            );
            return;
        }

        csi_dispatch::csi_dispatch(&mut self.handler, &action);
    }

    /// Like `next_slice` but takes one byte and is necessarily a scalar operation that can't use the bulk paths.
    /// Prefer `next_slice` if you can and try to get multiple bytes at once.
    pub(crate) fn next(&mut self, c: u8) {
        self.next_untracked(c);
        if self.continuation.is_some() {
            self.track_continuation(&[c]);
        }
    }

    /// [`Self::next`] without the continuation tracking.
    fn next_untracked(&mut self, c: u8) {
        // The scalar path can be responsible for decoding UTF-8.
        if self.parser.state == parser::State::Ground {
            self.next_utf8(c);
        } else {
            self.next_non_utf8(c);
        }
    }

    /// Process the next byte and print as necessary.
    ///
    /// This assumes we're in the UTF-8 decoding state. If we may not be in the UTF-8 decoding state call
    /// `next_slice` or `next`.
    fn next_utf8(&mut self, c: u8) {
        debug_assert_eq!(self.parser.state, parser::State::Ground);

        let (codepoint, consumed) = self.utf8decoder.next(c);
        if let Some(codepoint) = codepoint {
            self.handle_codepoint(codepoint);
        }
        if !consumed {
            // We optimize for the scenario where the text being printed in the terminal ISN'T full of ill-formed
            // UTF-8 sequences.
            let (codepoint, consumed) = self.utf8decoder.next(c);
            // It should be impossible for the decoder to not consume the byte twice in a row.
            debug_assert!(consumed);
            if let Some(codepoint) = codepoint {
                self.handle_codepoint(codepoint);
            }
        }
    }

    /// To be called whenever the UTF-8 decoder produces a codepoint.
    ///
    /// This function is abstracted this way to handle the case where the decoder emits a 0x1B after rejecting an
    /// ill-formed sequence.
    fn handle_codepoint(&mut self, c: u32) {
        if !is_control(c) {
            self.handler.print(c);
            return;
        }
        match u8::try_from(c) {
            // ESC
            Ok(0x1B) => {
                self.parser.state = parser::State::Escape;
                self.parser.clear();
            }
            Ok(c @ 0x00..=0x1F) => dispatch::execute(&mut self.handler, c),
            // Ignore C1 that came via UTF-8 decoding, matching xterm.
            _ => {
                if log_unsupported_once(&SEEN_UTF8_DECODED_C1_CODEPOINT, 0x80) {
                    tracing::warn!(target: LOG_SCOPE, "ignoring UTF-8-decoded C1 controls, first: 0x{c:x}");
                }
            }
        }
    }

    /// Process the next character and call any callbacks if necessary.
    ///
    /// This assumes that we're not in the UTF-8 decoding state. If we may be in the UTF-8 decoding state call
    /// `next_slice` or `next`.
    fn next_non_utf8(&mut self, c: u8) {
        debug_assert_ne!(self.parser.state, parser::State::Ground);

        let state = self.parser.state;
        match state {
            // Fast path for CSI entry.
            parser::State::Escape if c == b'[' => {
                self.parser.state = parser::State::CsiEntry;
                return;
            }
            // Fast path for CSI params: csi_param is the most common parser state other than ground by a fairly wide
            // margin.
            parser::State::CsiParam if self.csi_param_fast(c) => return,
            // Fast path for CSI entry, the state right after "ESC [".
            parser::State::CsiEntry if self.csi_entry_byte(c) => return,
            _ => {}
        }

        let [exit, transition, entry] = self.parser.next(c);
        for action in [exit, transition, entry].into_iter().flatten() {
            dispatch_action(&mut self.handler, action, c);
        }
    }

    /// One byte in the `csi_param` state, if the fast path handles it.
    #[inline]
    fn csi_param_fast(&mut self, c: u8) -> bool {
        match c {
            // A C0 escape (yes, this is valid):
            0x00..=0x0F => dispatch::execute(&mut self.handler, c),
            // We ignore C0 escapes > 0xF since execute doesn't have processing for them anyway; 0x7F is explicitly
            // ignored.
            0x10..=0x17 | 0x19 | 0x1C..=0x1F | 0x7F => {}
            // We don't currently have any handling for 0x18 or 0x1A, but they should still move the parser state to
            // ground.
            0x18 | 0x1A => self.parser.state = parser::State::Ground,
            b'0'..=b'9' | b':' | b';' => csi_param_byte(&mut self.parser, c),
            // A final byte: dispatch the CSI directly.
            0x40..=0x7E => self.csi_dispatch_final(c),
            // Defer to the state machine to handle any other characters.
            _ => return false,
        }
        true
    }
}

/// What a fast path of [`Stream::consume_until_ground`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FastPath {
    /// It consumed this many bytes, at least one.
    Consumed(usize),
    /// It consumed the whole input.
    Exhausted,
    /// It did not apply: the byte goes through the state machine.
    None,
}

/// A parameter digit or separator in the `csi_param` state.
#[inline]
fn csi_param_byte(p: &mut Parser, c: u8) {
    if usize::from(p.params_idx) >= parser::MAX_PARAMS {
        return;
    }
    if c.is_ascii_digit() {
        p.param_acc = p.param_acc.saturating_mul(10).saturating_add(u16::from(c - b'0'));
        // The parser's CSI param action uses param_acc_idx to decide if there's a final param that needs to be
        // consumed or not, but it doesn't matter really what it is as long as it's not 0.
        p.param_acc_idx |= 1;
        return;
    }
    p.params[usize::from(p.params_idx)] = p.param_acc;
    if c == b':' {
        p.params_sep.set(usize::from(p.params_idx));
    }
    p.params_idx += 1;
    p.param_acc = 0;
    p.param_acc_idx = 0;
}

/// Hand runs of printable codepoints to the handler as `print_slice` actions so it can process them with per-run
/// rather than per-codepoint overhead; execute the C0 controls between them and drop the decoded C1 ones.
fn print_decoded<H: Handler>(handler: &mut H, cps: &[u32]) {
    let mut i: usize = 0;
    while let Some(&cp) = cps.get(i) {
        if is_control(cp) {
            match u8::try_from(cp) {
                // C0 controls execute rather than print.
                Ok(c @ 0x00..=0x1F) => dispatch::execute(handler, c),
                // C1 controls decoded from UTF-8 are ignored.
                _ => {
                    if log_unsupported_once(&SEEN_UTF8_DECODED_C1_SLICE, 0x80) {
                        tracing::warn!(target: LOG_SCOPE, "ignoring UTF-8-decoded C1 controls, first: 0x{cp:x}");
                    }
                }
            }
            i += 1;
            continue;
        }
        let end = printable_run_end(cps, i + 1);
        handler.print_slice(&cps[i..end]);
        i = end;
    }
}

/// The end of the printable run that continues at `start`: the index of the next control, or the length.
///
/// This is an early-exit search loop that LLVM won't auto-vectorize, and printable runs dominate real input, so it
/// scans several codepoints at a time manually, with one AND-test per lane.
#[inline]
fn printable_run_end(cps: &[u32], start: usize) -> usize {
    let mut end = start;
    if let Some(lanes) = PRINT_RUN_LANES {
        while let Some(window) = cps.get(end..end + lanes) {
            let min_masked = window.iter().fold(u32::MAX, |m, &v| m.min(v & !0x9F));
            if min_masked == 0 {
                break;
            }
            end += lanes;
        }
    }
    end + cps[end..]
        .iter()
        .position(|&v| is_control(v))
        .unwrap_or(cps.len() - end)
}

/// Dispatch one action of `Parser::next` to the handler; `c` is the byte that produced it.
fn dispatch_action<H: Handler>(handler: &mut H, action: parser::Action<'_>, c: u8) {
    match action {
        parser::Action::Print(p) => handler.print(p),
        parser::Action::Execute(code) => dispatch::execute(handler, code),
        parser::Action::CsiDispatch(csi) => csi_dispatch::csi_dispatch(handler, &csi),
        parser::Action::EscDispatch(esc) => dispatch::esc_dispatch(handler, &esc),
        parser::Action::OscDispatch(cmd) => dispatch::osc_dispatch(handler, *cmd),
        parser::Action::DcsHook(dcs) => handler.dcs_hook(&dcs),
        parser::Action::DcsPut(code) => handler.dcs_put(code),
        parser::Action::DcsUnhook => handler.dcs_unhook(),
        parser::Action::ApcStart => handler.apc_start(),
        parser::Action::ApcPut(code) => handler.apc_put(code),
        parser::Action::ApcEnd => handler.apc_end(ApcEnd {
            terminated: c == 0x1B || c == 0x9C,
        }),
    }
}

/// The table of keys [`log_unsupported_once`] has logged for one call site.
struct UnsupportedOnceTable {
    /// `u32` slots so every `u16` key is representable alongside an empty sentinel and so 32-bit targets have native
    /// atomics.
    seen: [core::sync::atomic::AtomicU32; 16],
}

impl UnsupportedOnceTable {
    /// An unclaimed slot.
    const EMPTY: u32 = u32::MAX;

    /// A table with no key logged.
    const fn new() -> Self {
        Self {
            seen: [const { core::sync::atomic::AtomicU32::new(Self::EMPTY) }; 16],
        }
    }

    /// The number of claimed slots.
    #[cfg(test)]
    fn claimed(&self) -> usize {
        self.seen
            .iter()
            .filter(|s| s.load(core::sync::atomic::Ordering::Acquire) != Self::EMPTY)
            .count()
    }
}

/// Whether to log an unsupported-input message: true at most once per distinct key per process.
///
/// These messages are emitted in response to input that the terminal application controls, so a misbehaving (or
/// merely chatty) program can trigger the same message millions of times, e.g. by toggling an unimplemented mode on
/// every frame. Each log call has a real throughput cost (formatting plus a blocking write per message) while adding
/// no diagnostic value beyond the first occurrence.
///
/// The keys seen so far are tracked in a small fixed table (64 bytes) per call site. Real streams only ever produce a
/// handful of distinct unsupported values per site, so if the table ever fills, messages for further new values are
/// suppressed as well: by that point the log already shows this class of problem and unbounded distinct values would
/// flood it anyway.
///
/// Zig's `logUnsupportedOnce` takes the format and logs itself; here the caller logs when this returns true, so the
/// message is only formatted when it is emitted.
fn log_unsupported_once(table: &UnsupportedOnceTable, key: u16) -> bool {
    use core::sync::atomic::Ordering;

    let key = u32::from(key);

    // The atomics make concurrent streams safe: slots are only ever claimed, never changed, so the scan can stop at
    // the first empty slot. The worst case race is a benign duplicate message.
    for slot in &table.seen {
        let cur = slot.load(Ordering::Acquire);
        if cur == key {
            return false; // already logged
        }
        if cur != UnsupportedOnceTable::EMPTY {
            continue; // other key, keep scanning
        }

        // Empty slot: claim it for this key and log. Losing the race suppresses if it was to the same key, and keeps
        // scanning otherwise.
        match slot.compare_exchange(UnsupportedOnceTable::EMPTY, key, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(actual) if actual == key => return false,
            Err(_) => {}
        }
    }

    // Table full: suppress new values too.
    false
}

// The tables behind the call sites, in the order they appear in `stream.zig`. `SEEN_UNIMPLEMENTED_MODE` and
// `SEEN_UNIMPLEMENTED_ESC_ACTION` are each shared by two call sites exactly as Zig shares them. The two
// UTF-8-decoded C1 sites are two Zig instantiations (`u32` and `u21` arguments), so each has its own table.
static SEEN_UTF8_DECODED_C1_SLICE: UnsupportedOnceTable = UnsupportedOnceTable::new();
static SEEN_UTF8_DECODED_C1_CODEPOINT: UnsupportedOnceTable = UnsupportedOnceTable::new();
static SEEN_INVALID_C0: UnsupportedOnceTable = UnsupportedOnceTable::new();
static SEEN_INVALID_DEVICE_ATTRIBUTES: UnsupportedOnceTable = UnsupportedOnceTable::new();
static SEEN_UNIMPLEMENTED_MODE: UnsupportedOnceTable = UnsupportedOnceTable::new();
static SEEN_UNKNOWN_CSI_M_INTERMEDIATE: UnsupportedOnceTable = UnsupportedOnceTable::new();
static SEEN_CSI_22_23_T_EXTRA_PARAMS: UnsupportedOnceTable = UnsupportedOnceTable::new();
static SEEN_CSI_T_UNIMPLEMENTED_PARAM: UnsupportedOnceTable = UnsupportedOnceTable::new();
static SEEN_OSC1_CHANGE_ICON: UnsupportedOnceTable = UnsupportedOnceTable::new();
static SEEN_UNIMPLEMENTED_ESC_ACTION: UnsupportedOnceTable = UnsupportedOnceTable::new();

#[cfg(test)]
mod tests;
