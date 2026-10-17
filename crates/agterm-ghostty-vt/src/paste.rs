//! Pasting into a terminal (`paste.h`): the terminal frames the text for its current modes (bracketed paste 2004,
//! line endings as carriage returns otherwise, unsafe control bytes replaced) and refuses text that could run a
//! command unless the paste allows it; or, with Kitty paste events on (mode 5522), it sends the program an event
//! instead. The bytes go out through the installed [`Effects::write_pty`](crate::Effects::write_pty).

use std::ffi::c_void;

use crate::effects::ClipboardLocation;
use crate::{Error, Terminal, ffi};

/// Why a paste happens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PasteSource {
    /// The user pasted from a clipboard (a key binding, a menu, a middle click).
    #[default]
    Clipboard,
    /// Text inserted another way (an IME commit, a drop, scripted input): always written as text.
    Text,
}

/// What a paste did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PasteOutcome {
    /// Bytes went to the program (the framed text, or a paste event).
    Written,
    /// There was nothing to paste (empty text).
    Empty,
    /// The text could run a command (a line ending outside bracketed paste, or the bracketed-paste terminator) and
    /// the paste did not allow it; nothing was written. Confirm with the user, then paste again allowing it.
    Unsafe,
}

/// The MIME type a text paste offers.
const TEXT: &[u8] = b"text/plain";

/// Writes the paste's text to the library's writer (the one representation offered, so `mime` needs no check).
unsafe extern "C" fn read_text(userdata: *mut c_void, _mime: ffi::GhosttyString, writer: ffi::GhosttyWriter) -> bool {
    // SAFETY: `userdata` is the `&str` the paste borrows for the call (`Terminal::paste`).
    let text = unsafe { &*userdata.cast::<&str>() };
    if text.is_empty() {
        return true;
    }
    let Some(write) = writer.write else {
        return false;
    };
    // SAFETY: the writer is valid during this callback; the bytes are borrowed for the call.
    unsafe { write(writer.userdata, text.as_ptr(), text.len()) }
}

impl Terminal {
    /// Paste `text` per the terminal's state; the bytes stream through [`Effects::write_pty`](crate::Effects).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] when no effects are installed (nothing to write to), or the library's result when the
    /// paste fails (nothing is written then).
    pub fn paste(
        &mut self,
        text: &str,
        source: PasteSource,
        location: ClipboardLocation,
        allow_unsafe: bool,
    ) -> Result<PasteOutcome, Error> {
        let mime = ffi::GhosttyString {
            ptr: TEXT.as_ptr(),
            len: TEXT.len(),
        };
        let mut text = text;
        let paste = ffi::GhosttyPaste {
            size: std::mem::size_of::<ffi::GhosttyPaste>(),
            location: location.raw(),
            source: match source {
                PasteSource::Clipboard => ffi::GHOSTTY_PASTE_SOURCE_CLIPBOARD,
                PasteSource::Text => ffi::GHOSTTY_PASTE_SOURCE_TEXT,
            },
            mimes: &raw const mime,
            mimes_len: 1,
            reader: ffi::GhosttyMimeReader {
                read: Some(read_text),
                userdata: (&raw mut text).cast(),
            },
            allow_unsafe,
        };
        let mut written = false;
        // SAFETY: the terminal is live and `&mut self` serializes the call; the paste, its MIME list and the text the
        // reader's userdata points at all outlive the call, which borrows them only for its duration.
        let result = unsafe { ffi::ghostty_terminal_paste(self.as_raw(), &raw const paste, &raw mut written) };
        match Error::check(result) {
            Ok(()) if written => Ok(PasteOutcome::Written),
            Ok(()) => Ok(PasteOutcome::Empty),
            Err(Error::Rejected) => Ok(PasteOutcome::Unsafe),
            Err(err) => Err(err),
        }
    }
}

/// Whether `text` is safe to paste whatever the terminal's state (no line ending, no bracketed-paste terminator).
#[must_use]
pub fn paste_is_safe(text: &str) -> bool {
    // SAFETY: the pointer and length describe the borrowed string for the call.
    unsafe { ffi::ghostty_paste_is_safe(text.as_ptr().cast(), text.len()) }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::Effects;

    /// Collects what the terminal writes to the program.
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Effects for Sink {
        fn write_pty(&mut self, data: &[u8]) {
            self.0.lock().unwrap().extend_from_slice(data);
        }
    }

    fn terminal() -> (Terminal, Arc<Mutex<Vec<u8>>>) {
        let mut terminal = Terminal::new(80, 24).unwrap();
        let out = Arc::new(Mutex::new(Vec::new()));
        terminal.set_effects(Box::new(Sink(Arc::clone(&out)))).unwrap();
        (terminal, out)
    }

    fn take(out: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
        std::mem::take(&mut *out.lock().unwrap())
    }

    #[test]
    fn a_plain_paste_turns_newlines_into_returns_only_when_allowed() {
        let (mut terminal, out) = terminal();
        let paste = |terminal: &mut Terminal, text, allow| {
            terminal
                .paste(text, PasteSource::Clipboard, ClipboardLocation::Standard, allow)
                .unwrap()
        };
        assert_eq!(paste(&mut terminal, "echo hi", false), PasteOutcome::Written);
        assert_eq!(take(&out), b"echo hi");
        assert_eq!(paste(&mut terminal, "ls\nrm", false), PasteOutcome::Unsafe);
        assert!(take(&out).is_empty());
        assert_eq!(paste(&mut terminal, "ls\nrm", true), PasteOutcome::Written);
        assert_eq!(take(&out), b"ls\rrm");
        assert_eq!(paste(&mut terminal, "", false), PasteOutcome::Empty);
    }

    #[test]
    fn a_bracketed_paste_is_wrapped_and_its_newlines_are_safe() {
        let (mut terminal, out) = terminal();
        terminal.vt_write(b"\x1b[?2004h");
        let outcome = terminal
            .paste("a\nb\x1bc", PasteSource::Clipboard, ClipboardLocation::Standard, false)
            .unwrap();
        assert_eq!(outcome, PasteOutcome::Written);
        assert_eq!(take(&out), b"\x1b[200~a\nb c\x1b[201~");
        let terminator = terminal
            .paste(
                "x\x1b[201~y",
                PasteSource::Clipboard,
                ClipboardLocation::Standard,
                false,
            )
            .unwrap();
        assert_eq!(terminator, PasteOutcome::Unsafe);
    }

    #[test]
    fn a_paste_with_nowhere_to_write_fails() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        assert_eq!(
            terminal.paste("x", PasteSource::Text, ClipboardLocation::Standard, false),
            Err(Error::InvalidValue)
        );
    }

    #[test]
    fn safety_is_conservative() {
        assert!(paste_is_safe("plain"));
        assert!(!paste_is_safe("a\nb"));
        assert!(!paste_is_safe("a\rb"));
    }
}
