//! The stream continuation: the input a stream needs to replay to reconstruct the parser state it was left in, when a
//! feed ends in the middle of a VT sequence or a UTF-8 codepoint.
//!
//! Ghostty: `src/terminal/stream_continuation.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/stream_continuation.rs` at `RUSTTY_REV` (MIT).
//! Changes: the retained bytes are a `Vec` grown fallibly, and the continuation is written to a `std::io::Write`. The
//! backward search for the last ESC is `rposition` (Zig's vectorized scan finds the same byte). `validate`, which
//! checks a snapshot's continuation, went with the snapshot codec, its only user.

use std::io;

use crate::mem::alloc::{OutOfMemory, TryVecExt as _};
use crate::terminal::parser::{self, Parser};
use crate::terminal::utf8_decoder::Utf8Decoder;

/// Retains the input needed to reconstruct unfinished Stream parser state.
///
/// A feed is one chunk of bytes given to a Stream. It can end in the middle of
/// a VT sequence or a UTF-8 codepoint. To continue in another Stream, that
/// Stream starts from ground (with no sequence in progress) and reads the
/// saved bytes again. The first saved byte is called the replay start.
///
/// Stream updates this tracker once per feed based on where that replay start
/// is:
///
///   - Ground (parser and UTF-8 decoder): `reset`, no suffix is needed.
///   - Anything unfinished: `append`, which replaces the suffix with the
///     bytes from the replay start onward when that start is inside this
///     feed, and otherwise extends the suffix with this whole feed because
///     the unfinished state began in an earlier one.
///
/// This keeps the retained bytes minimal: they always begin at the replay
/// start, so the feed path never needs to parse or trim old input. The suffix
/// may still contain a byte whose visible terminal effect already happened,
/// such as BEL inside an unfinished CSI sequence. `write` leaves out those
/// bytes so replay does not perform the same effect twice.
///
/// If the suffix exceeds `max_bytes`, or retaining it fails, `broken`
/// is set until a later feed ends at ground (`reset`) or contains a new
/// replay start (`replace`), both of which need nothing that was lost.
///
/// Replay semantics are guaranteed only for the terminal's own stream
/// handler; other handlers are unsupported.
pub(crate) struct Tracker {
    /// The longest suffix the tracker keeps; a longer one breaks it.
    pub(crate) max_bytes: usize,
    /// The retained suffix, from its replay start.
    pub(crate) bytes: Vec<u8>,
    /// Whether the suffix was lost (too long, or its growth failed).
    pub(crate) broken: bool,
}

impl Tracker {
    /// The room reserved up front for the suffix.
    const INITIAL_CAPACITY: usize = 4096;

    /// Initialize a tracker that keeps at most `max_bytes`.
    pub(crate) fn new(max_bytes: usize) -> Self {
        let mut bytes = Vec::new();
        // We ignore memory errors here. They'll mark the tracker as broken in a future append.
        let _ = bytes.try_ensure_total_capacity(max_bytes.min(Self::INITIAL_CAPACITY));
        Self {
            max_bytes,
            bytes,
            broken: false,
        }
    }

    /// Clear the current continuation suffix and broken state while keeping
    /// its allocation. Stream calls this after reaching ground because no
    /// earlier input is needed to reconstruct the next unfinished state.
    pub(crate) fn reset(&mut self) {
        self.broken = false;
        self.bytes.clear();
    }

    /// Retain the part of `input` needed to replay a feed that ended with
    /// `pending` state unfinished.
    ///
    /// When the input contains the replay start for that state, it replaces
    /// the retained suffix outright because nothing earlier is needed. The
    /// new suffix is complete on its own, so this also repairs a broken
    /// tracker. Otherwise the unfinished state began in an earlier feed and
    /// this whole input extends the current suffix.
    pub(crate) fn append(&mut self, pending: Pending, input: &[u8]) {
        let start = match pending {
            Pending::Vt => find_vt_replay_start(input),
            Pending::Utf8 => find_utf8_replay_start(input),
        };
        match start {
            Some(start) => self.replace(&input[start..]),
            None => self.extend(input),
        }
    }

    /// Replace the continuation suffix with one that has a new replay start.
    fn replace(&mut self, suffix: &[u8]) {
        self.bytes.clear();
        if suffix.len() > self.max_bytes || self.append_slice(suffix).is_err() {
            self.mark_broken();
            return;
        }
        self.broken = false;
    }

    /// Extend the continuation suffix with a fragment that contains no replay
    /// start of its own. The stream stayed unfinished for the whole fragment,
    /// so the existing suffix plus this fragment reproduces the current state.
    fn extend(&mut self, fragment: &[u8]) {
        if self.broken {
            return;
        }
        if fragment.len() > self.max_bytes.saturating_sub(self.bytes.len()) || self.append_slice(fragment).is_err() {
            self.mark_broken();
        }
    }

    /// Write the replay-safe continuation suffix to `writer`. The retained
    /// bytes already begin at the replay start; this omits only bytes that
    /// would repeat committed terminal effects without contributing to the
    /// unfinished parser state (e.g. a BEL inside an unfinished CSI). The
    /// tracker must not be broken.
    pub(crate) fn write(&self, writer: &mut dyn io::Write) -> io::Result<()> {
        let mut scanner = BoundaryScanner::new();
        for &c in &self.bytes {
            if scanner.next(c) != Effect::Omittable {
                writer.write_all(&[c])?;
            }
        }
        Ok(())
    }

    /// Lose the suffix until a later feed needs none of it.
    fn mark_broken(&mut self) {
        self.broken = true;
        self.bytes.clear();
    }

    /// `self.bytes.appendSlice(..)`, failing instead of aborting.
    fn append_slice(&mut self, bytes: &[u8]) -> Result<(), OutOfMemory> {
        self.bytes.try_extend_copy(bytes)
    }
}

/// Which state machine is unfinished at the end of a feed. When the VT
/// parser is outside ground the unfinished state is an escape sequence.
/// When it is grounded, only the UTF-8 decoder can be unfinished, with
/// an incomplete codepoint ending the feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pending {
    Vt,
    Utf8,
}

/// Find where replay must begin when a feed ends inside a VT sequence.
///
/// VT parsers treat ESC specially: it abandons the previous parser state and
/// starts a fresh escape state no matter what was being parsed. Because of
/// that rule, feeding the bytes from the last ESC onward into a grounded
/// Stream recreates the unfinished state at the end of `input`.
///
/// The caller must only use this when the VT parser ended outside ground.
/// The returned value is the index of the last ESC in `input`. A null result
/// means the sequence began in an earlier feed, so this entire input must be
/// appended to the continuation bytes already retained.
fn find_vt_replay_start(input: &[u8]) -> Option<usize> {
    // Escape sequences are short, so when the stream ends inside one the replay start is usually within the last few
    // bytes.
    input.iter().rposition(|&b| b == 0x1B)
}

/// Find where replay must begin when a feed ends inside a UTF-8 codepoint.
///
/// A UTF-8 codepoint starts with a lead byte and may have up to three
/// continuation bytes. If the lead byte is in this input, replay must begin
/// there so the decoder sees the complete partial codepoint again. Since an
/// incomplete codepoint can contain at most three bytes, only the final three
/// input bytes need to be searched.
///
/// The caller must only use this when the VT parser is at ground and the
/// UTF-8 decoder ended mid-codepoint. The returned value is the lead byte's
/// index. A null result means the lead byte was in an earlier feed, so this
/// entire input must be appended to the continuation bytes already retained.
fn find_utf8_replay_start(input: &[u8]) -> Option<usize> {
    const MAX_PENDING: usize = 3;
    let mut idx = input.len();
    while idx > 0 && input.len() - idx < MAX_PENDING {
        idx -= 1;
        if input[idx] >= 0xC0 {
            return Some(idx);
        }
    }
    None
}

/// Classifies bytes of a continuation suffix for export.
///
/// The retained suffix reconstructs unfinished state exactly, but it can
/// contain bytes whose handler-visible work already committed the first
/// time (e.g. a C0 control executed inside an unfinished CSI sequence).
/// This implements a minimal VT stream processor (to avoid circular imports
/// with stream.zig) so `Tracker.write` can omit those bytes and replay
/// never repeats a terminal effect. It only runs at export time, never on
/// the feed path.
///
/// This is allocation-free.
struct BoundaryScanner {
    parser: Parser,
    utf8decoder: Utf8Decoder,
}

/// How one byte affects committed work and the continuation suffix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    /// The byte commits no handler-visible work. It must remain because it
    /// may still build the unfinished VT, UTF-8, APC, or DCS state.
    Uncommitted,

    /// The byte commits handler-visible work but also changes a parser
    /// state tag or reaches ground. It cannot be omitted in isolation;
    /// boundary analysis decides whether the surrounding prefix is safe.
    Committed,

    /// The byte commits handler-visible work without changing either
    /// state-machine tag or reaching ground. It can be omitted without
    /// changing the unfinished state, avoiding a duplicate effect.
    Omittable,
}

impl BoundaryScanner {
    /// Both state machines at ground.
    fn new() -> Self {
        Self {
            parser: Parser::new(),
            utf8decoder: Utf8Decoder::default(),
        }
    }

    /// True only when neither state machine needs prior bytes to continue.
    fn ground(&self) -> bool {
        self.parser.state == parser::State::Ground && self.utf8decoder.state == 0
    }

    /// Consume one byte using the same scalar UTF-8 retry and VT transitions
    /// as Stream, then classify its effect on continuation construction.
    fn next(&mut self, c: u8) -> Effect {
        // Preserve the state tags so we can recognize committed controls that
        // do not contribute to the unfinished sequence.
        let parser_state = self.parser.state;
        let utf8_state = self.utf8decoder.state;

        // A byte is committed when replaying it would repeat handler-visible
        // work already captured outside the continuation suffix. Actions that
        // only build unfinished APC or DCS state are not committed because
        // replay must reconstruct that state.
        let committed = if self.parser.state == parser::State::Ground {
            self.ground_byte(c)
        } else {
            // These actions only build standard handler state. Their matching end/unhook actions are committed work.
            self.parser.next(c).into_iter().flatten().any(|action| {
                !matches!(
                    action,
                    parser::Action::DcsHook(_)
                        | parser::Action::DcsPut(_)
                        | parser::Action::ApcStart
                        | parser::Action::ApcPut(_)
                )
            })
        };

        if !committed {
            return Effect::Uncommitted;
        }

        // A committed byte is independently omittable only while the stream
        // remains unfinished and neither state-machine tag changes.
        let state_changed = parser_state != self.parser.state || utf8_state != self.utf8decoder.state;
        if !state_changed && !self.ground() {
            Effect::Omittable
        } else {
            Effect::Committed
        }
    }

    /// One byte at ground: Stream's scalar UTF-8 path, including retrying a byte that follows a malformed sequence.
    /// Returns whether it commits handler-visible work.
    fn ground_byte(&mut self, c: u8) -> bool {
        let mut committed = false;
        let (cp, consumed) = self.utf8decoder.next(c);
        if let Some(cp) = cp {
            committed = self.codepoint(cp) || committed;
        }
        if !consumed {
            let (cp, consumed) = self.utf8decoder.next(c);
            debug_assert!(consumed);
            if let Some(cp) = cp {
                committed = self.codepoint(cp) || committed;
            }
        }
        committed
    }

    /// Apply the Stream.handleCodepoint behavior relevant to replay. Returns
    /// whether the accepted codepoint would commit handler-visible work.
    fn codepoint(&mut self, cp: u32) -> bool {
        if cp == 0x1B {
            self.parser.state = parser::State::Escape;
            self.parser.clear();
            return false;
        }

        // Match Stream.handleCodepoint: all other accepted codepoints have
        // already caused a handler-visible action (including supported C0s).
        true
    }
}

/// `std.Io.Writer.fixed(&buf)`: an `io::Write` over a fixed-size buffer that fails (like `error.WriteFailed`) when
/// the buffer would overflow. Test fixture shared with the stream's continuation tests.
#[cfg(test)]
pub(crate) struct FixedIoWriter<'a> {
    buf: &'a mut [u8],
    /// Number of bytes written so far (Zig's `std.Io.Writer.end`).
    pub(crate) end: usize,
}

#[cfg(test)]
impl<'a> FixedIoWriter<'a> {
    pub(crate) fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, end: 0 }
    }

    /// `writer.buffered()`
    pub(crate) fn buffered(&self) -> &[u8] {
        &self.buf[..self.end]
    }
}

#[cfg(test)]
impl io::Write for FixedIoWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .end
            .checked_add(bytes.len())
            .filter(|&next| next <= self.buf.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::WriteZero, "WriteFailed"))?;
        self.buf[self.end..next].copy_from_slice(bytes);
        self.end = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
