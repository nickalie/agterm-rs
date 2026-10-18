//! The stream's tests (Ghostty's `stream.zig` tests, and rustty's beside them), split by topic under `tests/`. The
//! shared CSI corpus, the recording handler and the drivers are here.

use super::*;
use crate::terminal::{ansi, csi, modes};

/// The CSI shapes the fast paths must handle as the state machine does: plain params, sub-params (colon), mixed
/// separators, empty leading params, `>`/`?`/`<`/`=` private markers, intermediates (which must fall back to the
/// state machine), a colon directly after "ESC [" (the `csi_entry` edge case), parameterless finals, `MAX_PARAMS`
/// overflow (25 params: the dispatch is dropped entirely), param value saturation, C0 controls embedded
/// mid-sequence, 0x7F, and CAN/SUB.
pub(super) const CSI_DIFFERENTIAL_CORPUS: &[&[u8]] = &[
    b"\x1b[m",
    b"\x1b[0m",
    b"\x1b[38;2;10;20;30m",
    b"\x1b[38:2:10:20:30m",
    b"\x1b[38;2:10;20:30m",
    b"\x1b[;5H",
    b"\x1b[;;m",
    b"\x1b[?2004h",
    b"\x1b[?1000;1002;1006l",
    b"\x1b[>4;2m",
    b"\x1b[<0c",
    b"\x1b[=0c",
    b"\x1b[!p",
    b"\x1b[ q",
    b"\x1b[2 q",
    b"\x1b[$}",
    b"\x1b[:3m",
    // Colon separators with a non-'m' final: the whole dispatch is dropped ("we only allow colon or mixed separators
    // for the 'm' command"). These make a lost `params_sep.set()` observable; every other colon here is on an 'm',
    // whose SGR parse would hide it.
    b"\x1b[1:2H",
    b"\x1b[1;2:3H",
    b"\x1b[?1:2h",
    b"\x1b[1:2J",
    b"\x1b[H",
    b"\x1b[A\x1b[B\x1b[C\x1b[D",
    b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20;21;22;23;24;25m",
    b"\x1b[99999999m",
    b"\x1b[1\x07;2m",
    b"\x1b[1\x0d;2H",
    b"\x1b[1\x7f2m",
    b"\x1b[1\x18;2m",
    b"\x1b[1\x1a;2m",
    b"\x1b[1;2\x1b[3;4m",
    b"hello\x1b[31mworld\x1b[0m\r\n",
    b"\x1b[3;J\x1b[2K\x1b[10X",
    b"\x1b[?25l\x1b[?25h",
];

/// A handler that records the exact sequence of dispatch callbacks it receives: the fast paths change how an action
/// is produced, never what the handler is told.
#[derive(Default)]
pub(super) struct RecordingHandler {
    pub(super) log: Vec<String>,
}

impl Handler for RecordingHandler {
    fn print(&mut self, cp: u32) {
        self.log.push(format!("print {cp}"));
    }
    fn cursor_up(&mut self, v: CursorMovement) {
        self.log.push(format!("cursor_up {}", v.value));
    }
    fn cursor_down(&mut self, v: CursorMovement) {
        self.log.push(format!("cursor_down {}", v.value));
    }
    fn cursor_left(&mut self, v: CursorMovement) {
        self.log.push(format!("cursor_left {}", v.value));
    }
    fn cursor_right(&mut self, v: CursorMovement) {
        self.log.push(format!("cursor_right {}", v.value));
    }
    fn cursor_pos(&mut self, v: CursorPos) {
        self.log.push(format!("cursor_pos {} {}", v.row, v.col));
    }
    fn cursor_style(&mut self, v: ansi::CursorStyle) {
        self.log.push(format!("cursor_style {v:?}"));
    }
    fn set_attribute(&mut self, attr: sgr::Attribute<'_>) {
        self.log.push(format!("sgr {attr:?}"));
    }
    fn set_mode(&mut self, v: Mode) {
        self.log.push(format!("set_mode {:?}", v.mode));
    }
    fn reset_mode(&mut self, v: Mode) {
        self.log.push(format!("reset_mode {:?}", v.mode));
    }
    fn erase_display_below(&mut self, p: bool) {
        self.log.push(format!("ed_below {p}"));
    }
    fn erase_display_scrollback(&mut self, p: bool) {
        self.log.push(format!("ed_scrollback {p}"));
    }
    fn erase_line_complete(&mut self, p: bool) {
        self.log.push(format!("el_complete {p}"));
    }
    fn erase_chars(&mut self, n: usize) {
        self.log.push(format!("erase_chars {n}"));
    }
    fn linefeed(&mut self) {
        self.log.push("linefeed".to_string());
    }
    fn carriage_return(&mut self) {
        self.log.push("carriage_return".to_string());
    }
    fn bell(&mut self) {
        self.log.push("bell".to_string());
    }
    fn modify_key_format(&mut self, v: ansi::ModifyKeyFormat) {
        self.log.push(format!("modify_key_format {v:?}"));
    }
    fn full_reset(&mut self) {
        self.log.push("full_reset".to_string());
    }
    fn device_attributes(&mut self, req: device_attributes::Req) {
        self.log.push(format!("device_attributes {req:?}"));
    }
    fn soft_reset(&mut self) {
        self.log.push("soft_reset".to_string());
    }
    fn active_status_display(&mut self, v: ansi::StatusDisplay) {
        self.log.push(format!("active_status_display {v:?}"));
    }
}

/// A stream over `H`'s default.
pub(super) fn stream<H: Handler + Default>() -> Stream<H> {
    Stream::new(Options::with_handler(H::default()))
}

/// Drive the input through the bulk path, `consume_until_ground`, where the CSI entry and parameter fast paths and
/// the APC and OSC payload scans are, with the escape scan of `next_slice_capped` around it but without its bulk
/// UTF-8 decode.
pub(super) fn drive_bulk<H: Handler + Default>(input: &[u8]) -> H {
    let mut s = stream::<H>();
    drive_bulk_into(&mut s, input);
    s.handler
}

/// `drive_bulk` for a stream that already exists, so a test can feed several chunks through the bulk path with the
/// parser state carried across calls (Ghostty's tests do this with repeated `nextSlice`).
pub(super) fn drive_bulk_into<H: Handler>(s: &mut Stream<H>, input: &[u8]) {
    let mut i = 0usize;
    while i < input.len() {
        if s.parser.state != parser::State::Ground {
            // `consume_until_ground` always advances (it consumes at least one byte per iteration, or returns
            // `input.len()`).
            i += s.consume_until_ground(&input[i..]);
            continue;
        }
        let c = input[i];
        if c == 0x1B {
            s.parser.state = parser::State::Escape;
            s.parser.clear();
        } else {
            s.next_utf8(c);
        }
        i += 1;
    }
}

/// The public `next_slice` entry point.
pub(super) fn drive_slice<H: Handler + Default>(input: &[u8]) -> H {
    let mut s = stream::<H>();
    s.next_slice(input);
    s.handler
}

/// One `next` per byte.
pub(super) fn drive_per_byte<H: Handler + Default>(input: &[u8]) -> H {
    let mut s = stream::<H>();
    for &b in input {
        s.next(b);
    }
    s.handler
}

/// The oracle of the fast paths: the parser's state machine alone, every action of `Parser::next` dispatched, with
/// none of the stream's fast paths.
pub(super) fn drive_state_machine<H: Handler + Default>(input: &[u8]) -> H {
    let mut s = stream::<H>();
    for &c in input {
        if s.parser.state == parser::State::Ground {
            s.next_utf8(c);
            continue;
        }
        let [exit, transition, entry] = s.parser.next(c);
        for action in [exit, transition, entry].into_iter().flatten() {
            dispatch_action(&mut s.handler, action, c);
        }
    }
    s.handler
}

mod csi_dispatch;
mod parser_utf8_simd;
mod slice_continuation;
