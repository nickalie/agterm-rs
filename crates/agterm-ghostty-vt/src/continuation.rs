//! The stream continuation (`terminal.h`, `GHOSTTY_TERMINAL_OPT_CONTINUATION_MAX_BYTES` and
//! `ghostty_terminal_continuation_*`): the exact byte suffix that rebuilds an unfinished escape sequence or UTF-8
//! character in another terminal, so that a copy of a terminal taken between two reads parses what follows exactly as
//! the original does.
//!
//! Tracking is off until [`Terminal::set_continuation_limit`] turns it on, and it only covers input written after
//! that: turned on mid-sequence, or past its byte limit, the current continuation is [`Continuation::Unavailable`] until
//! a later write reaches ground or starts a fresh sequence. Capture never truncates or invents one: a caller that gets
//! `Unavailable` waits for the next cut instead.
//!
//! [`Terminal::replay_continuation`] feeds a captured continuation into a terminal holding the rest of the state (a
//! fresh one restored from the formatter's output, say): the original fed a suffix and the restored one fed the same
//! suffix then end in the same state.

use crate::{Error, Terminal, ffi, raw_slice};

/// A terminal's current continuation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Continuation {
    /// The bytes that rebuild the unfinished parser and UTF-8 decoder state; empty at ground.
    Bytes(Vec<u8>),
    /// Tracking is on but the current continuation cannot be rebuilt: it outgrew the limit, or tracking began inside
    /// it. A later write that reaches ground or starts a fresh sequence makes one available again.
    Unavailable,
}

impl Terminal {
    /// Track the continuation of what is written from now on, keeping at most `max_bytes` of it; zero turns tracking
    /// off. Lowering the limit below the retained continuation, or turning tracking on while a sequence is open,
    /// makes the current one [`Continuation::Unavailable`].
    ///
    /// # Errors
    ///
    /// The library's result when the value is refused.
    pub fn set_continuation_limit(&mut self, max_bytes: usize) -> Result<(), Error> {
        // SAFETY: the terminal is live and `&mut self` serializes the call; the option takes a `size_t *` read during
        // the call.
        let result = unsafe {
            ffi::ghostty_terminal_set(
                self.as_raw(),
                ffi::GHOSTTY_TERMINAL_OPT_CONTINUATION_MAX_BYTES,
                (&raw const max_bytes).cast(),
            )
        };
        Error::check(result)
    }

    /// The configured continuation limit in bytes; zero while tracking is off.
    #[must_use]
    pub fn continuation_limit(&self) -> usize {
        let mut value: usize = 0;
        // SAFETY: the terminal is live; `DATA_CONTINUATION_MAX_BYTES` writes a `size_t`.
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw_shared(),
                ffi::GHOSTTY_TERMINAL_DATA_CONTINUATION_MAX_BYTES,
                (&raw mut value).cast(),
            )
        };
        value
    }

    /// The current continuation, copied.
    ///
    /// # Errors
    ///
    /// [`Error::NoValue`] while tracking is off ([`Terminal::set_continuation_limit`]), [`Error::OutOfMemory`] when
    /// the copy cannot be allocated.
    pub fn continuation(&self) -> Result<Continuation, Error> {
        if self.continuation_limit() == 0 {
            return Err(Error::NoValue);
        }
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len = 0;
        // SAFETY: the terminal is live and `&self` keeps every mutating call out meanwhile; a null allocator selects
        // the default one, and the out-pointers are valid.
        let result = unsafe {
            ffi::ghostty_terminal_continuation_alloc(self.raw_shared(), std::ptr::null(), &raw mut ptr, &raw mut len)
        };
        match Error::check(result) {
            Ok(()) => {}
            // tracking is on (checked above) and the arguments are valid, so this is the unavailable continuation
            Err(Error::InvalidValue) => return Ok(Continuation::Unavailable),
            Err(err) => return Err(err),
        }
        // SAFETY: on success `ptr` holds `len` bytes from the default allocator, or is null for an empty continuation.
        let bytes = unsafe { raw_slice(ptr, len) }.to_vec();
        // SAFETY: frees exactly what the call returned, with the same (default) allocator and length; null is allowed.
        unsafe { ffi::ghostty_free(std::ptr::null(), ptr, len) };
        Ok(Continuation::Bytes(bytes))
    }

    /// Feed a continuation captured from another terminal, so this one resumes mid-sequence where that one stood. Set
    /// this terminal's own limit first when its continuation is to be captured again: tracking turned on afterwards
    /// starts inside the replayed sequence, which leaves it unavailable.
    pub fn replay_continuation(&mut self, continuation: &[u8]) {
        self.vt_write(continuation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Format, FormatExtras, FormatOptions, Region};

    const LIMIT: usize = 4096;

    fn tracked() -> Terminal {
        let mut terminal = Terminal::new(40, 6).unwrap();
        terminal.set_continuation_limit(LIMIT).unwrap();
        terminal
    }

    fn bytes(terminal: &Terminal) -> Vec<u8> {
        match terminal.continuation().unwrap() {
            Continuation::Bytes(bytes) => bytes,
            Continuation::Unavailable => panic!("the continuation is unavailable"),
        }
    }

    /// Everything a restored terminal must agree on with the original: the cells with their styles, the cursor and
    /// modes, the title.
    fn state(terminal: &mut Terminal) -> (Vec<u8>, (u16, u16), String, bool) {
        let options = FormatOptions {
            format: Format::Vt,
            unwrap: false,
            trim: false,
            extras: FormatExtras::RESTORE,
        };
        let screen = terminal.format(Region::Screen, options).unwrap().unwrap();
        (screen, terminal.cursor(), terminal.title(), terminal.alternate_screen())
    }

    /// Write `prefix` into a tracked terminal, copy it into a fresh one through the formatter (state) and the
    /// continuation (the open sequence), then feed `suffix` to both: they must end equal.
    fn assert_resumes(prefix: &[u8], suffix: &[u8]) {
        let mut original = tracked();
        original.vt_write(prefix);
        let continuation = bytes(&original);
        let options = FormatOptions {
            format: Format::Vt,
            unwrap: false,
            trim: false,
            extras: FormatExtras::RESTORE,
        };
        let restore = original.format(Region::Screen, options).unwrap().unwrap();
        let mut restored = Terminal::new(40, 6).unwrap();
        restored.set_continuation_limit(LIMIT).unwrap();
        restored.vt_write(&restore);
        restored.replay_continuation(&continuation);
        assert_eq!(
            bytes(&restored),
            continuation,
            "the restored terminal holds the same open sequence"
        );
        original.vt_write(suffix);
        restored.vt_write(suffix);
        assert_eq!(
            state(&mut original),
            state(&mut restored),
            "prefix {prefix:?}, suffix {suffix:?}"
        );
    }

    #[test]
    fn tracking_is_off_until_a_limit_is_set() {
        let mut terminal = Terminal::new(10, 2).unwrap();
        assert_eq!(terminal.continuation_limit(), 0);
        assert_eq!(terminal.continuation(), Err(Error::NoValue));
        terminal.set_continuation_limit(64).unwrap();
        assert_eq!(terminal.continuation_limit(), 64);
        assert_eq!(
            terminal.continuation(),
            Ok(Continuation::Bytes(Vec::new())),
            "empty at ground"
        );
        terminal.set_continuation_limit(0).unwrap();
        assert_eq!(terminal.continuation(), Err(Error::NoValue));
    }

    #[test]
    fn a_cut_inside_a_utf8_character_resumes() {
        let text = "日本".as_bytes();
        for cut in 1..3 {
            let mut terminal = tracked();
            terminal.vt_write(&text[..cut]);
            assert_eq!(bytes(&terminal), &text[..cut]);
            assert_resumes(&text[..cut], &text[cut..]);
        }
    }

    #[test]
    fn a_cut_inside_a_csi_resumes() {
        let csi = b"ab\x1b[1;31";
        let mut terminal = tracked();
        terminal.vt_write(csi);
        assert_eq!(bytes(&terminal), b"\x1b[1;31");
        assert_resumes(csi, b"mred\x1b[0m plain");
        // a cursor movement split after its introducer
        assert_resumes(b"\x1b[", b"3;5Hx");
    }

    #[test]
    fn a_cut_inside_an_osc_resumes() {
        let mut terminal = tracked();
        terminal.vt_write(b"\x1b]2;half a ti");
        assert_eq!(bytes(&terminal), b"\x1b]2;half a ti");
        terminal.vt_write(b"tle\x07");
        assert_eq!(terminal.title(), "half a title");
        assert_eq!(bytes(&terminal), b"", "the terminator brings it back to ground");

        let mut original = tracked();
        original.vt_write(b"\x1b]2;half a ti");
        let continuation = bytes(&original);
        let mut restored = tracked();
        restored.replay_continuation(&continuation);
        original.vt_write(b"tle\x1b\\after");
        restored.vt_write(b"tle\x1b\\after");
        assert_eq!(restored.title(), "half a title");
        assert_eq!(state(&mut original), state(&mut restored));
    }

    #[test]
    fn embedded_c0_controls_resume_inside_their_sequence() {
        // C0 controls inside a CSI execute and keep it open (a line feed here), and are part of the continuation
        let mut terminal = tracked();
        terminal.vt_write(b"x\x1b[2\n");
        assert!(!terminal.at_ground());
        assert_resumes(b"x\x1b[2\n", b"Cy");
        assert_resumes(b"\x1b]2;a\x05b", b"c\x07z");
    }

    #[test]
    fn an_overlong_sequence_is_unavailable_until_the_next_cut() {
        let mut terminal = Terminal::new(40, 6).unwrap();
        terminal.set_continuation_limit(16).unwrap();
        terminal.vt_write(b"\x1b]2;");
        terminal.vt_write(&[b'x'; 64]);
        assert_eq!(terminal.continuation(), Ok(Continuation::Unavailable));
        // the terminator ends it: back at ground, the continuation is available (and empty) again
        terminal.vt_write(b"\x07");
        assert_eq!(terminal.continuation(), Ok(Continuation::Bytes(Vec::new())));
        // and a fresh sequence after it is tracked again
        terminal.vt_write(b"\x1b[3");
        assert_eq!(bytes(&terminal), b"\x1b[3");
    }

    #[test]
    fn tracking_turned_on_mid_sequence_recovers_after_a_terminator() {
        let mut terminal = Terminal::new(40, 6).unwrap();
        terminal.vt_write(b"\x1b]2;open");
        terminal.set_continuation_limit(LIMIT).unwrap();
        assert_eq!(terminal.continuation(), Ok(Continuation::Unavailable));
        terminal.vt_write(b" still");
        assert_eq!(terminal.continuation(), Ok(Continuation::Unavailable));
        terminal.vt_write(b"\x07\x1b[1");
        assert_eq!(bytes(&terminal), b"\x1b[1");
    }
}
