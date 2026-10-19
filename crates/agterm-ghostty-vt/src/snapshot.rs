//! The binary snapshot codec (`snapshot.h`): a terminal's whole state (screens, history, modes, colors, the unfinished
//! VT input) encoded into one CRC-checked record stream, and decoded into a new terminal in one call or incrementally.
//!
//! The format is versioned (version 1) without a compatibility promise across library revisions, so both ends must
//! run the same `LIBGHOSTTY_VT_REV`; across a pin change the formatter's VT output is the portable form.
//!
//! An incremental decode ([`Terminal::restore_snapshot`]) returns the terminal at the stream's READY marker, which is
//! enough to render it and feed it live input; the older history then arrives a page at a time through
//! [`Terminal::restore_history_page`]. The decoder borrows the terminal it returned until the FINISH marker, so the
//! terminal owns it meanwhile (and drops it first): it cannot outlive the terminal or restore into another one.

use crate::{Error, Handle, Terminal, ffi, raw_slice};

/// How a snapshot is decoded (`GhosttySnapshotDecoderOption`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SnapshotOptions {
    /// The largest unfinished VT input accepted; zero accepts only snapshots taken at ground, `None` keeps the
    /// library's limit (that of its largest APC buffer, 65 MiB at the pin).
    pub max_continuation_bytes: Option<usize>,
    /// Keep tracking the continuation on the restored terminal, at `max_continuation_bytes`
    /// ([`Terminal::continuation`]); off by default.
    pub retain_continuation: bool,
    /// Compress each history page as it is restored, so the decode never holds more than one page uncompressed.
    pub compress_history: bool,
}

/// The history rows a snapshot declares, known once its READY marker validated (advisory: rows that cannot be
/// applied any more are skipped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotHistory {
    pub primary: u64,
    /// `None` when the snapshot has no alternate screen.
    pub alternate: Option<u64>,
}

/// One restored history page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HistoryPage {
    /// The page belongs to the alternate screen.
    pub alternate: bool,
    /// The rows it prepended; zero when it validated but could no longer be applied (the terminal changed under it).
    pub rows: usize,
    /// The pages left in the same screen's history (not in the whole snapshot).
    pub remaining: u32,
}

/// A decoder between READY and FINISH, with the bytes it reads.
pub(crate) struct PendingHistory {
    // Fields drop in declaration order: the decoder, which borrows the source, goes first.
    decoder: Handle<ffi::GhosttySnapshotDecoderImpl>,
    _source: Box<[u8]>,
}

impl std::fmt::Debug for PendingHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingHistory")
            .field("decoder", &self.decoder)
            .finish_non_exhaustive()
    }
}

impl Terminal {
    /// Encode the whole terminal: screens, history, state and the unfinished VT input.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] while a sequence or UTF-8 character is open and its continuation is not tracked
    /// ([`Terminal::set_continuation_limit`] before the input that opened it), [`Error::OutOfMemory`] when the output
    /// cannot be allocated.
    pub fn encode_snapshot(&self) -> Result<Vec<u8>, Error> {
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len = 0;
        // SAFETY: the terminal is live and `&self` keeps every mutating call out for the duration; a null allocator
        // selects the default one, and the out-pointers are valid.
        Error::check(unsafe {
            ffi::ghostty_snapshot_encode_alloc(self.raw_shared(), std::ptr::null(), &raw mut ptr, &raw mut len)
        })?;
        // SAFETY: on success `ptr` holds `len` bytes from the default allocator.
        let bytes = unsafe { raw_slice(ptr, len) }.to_vec();
        // SAFETY: frees exactly what the call returned, with the same (default) allocator and length.
        unsafe { ffi::ghostty_free(std::ptr::null(), ptr, len) };
        Ok(bytes)
    }

    /// Decode a complete snapshot into a new terminal (no effects installed). Bytes after its FINISH marker are left
    /// unread.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for malformed, corrupt or truncated data, [`Error::LimitExceeded`] for unfinished VT
    /// input over [`SnapshotOptions::max_continuation_bytes`], [`Error::OutOfMemory`] when the state cannot be
    /// allocated.
    pub fn decode_snapshot(bytes: &[u8], options: SnapshotOptions) -> Result<Self, Error> {
        let decoder = decoder(bytes, options)?;
        let mut raw: ffi::GhosttyTerminal = std::ptr::null_mut();
        // SAFETY: the decoder is live and has not started; `raw` is a valid out-pointer, set to null on every error.
        Error::check(unsafe { ffi::ghostty_snapshot_decoder_decode(decoder.ptr(), &raw mut raw) })?;
        // FINISH validated: the decoder no longer borrows the terminal or the bytes
        drop(decoder);
        Self::from_raw(raw)
    }

    /// Decode a snapshot through its READY marker into a new terminal (no effects installed), ready to render and to
    /// take live input; its history stays pending for [`Terminal::restore_history_page`].
    ///
    /// # Errors
    ///
    /// As [`Terminal::decode_snapshot`].
    pub fn restore_snapshot(bytes: Vec<u8>, options: SnapshotOptions) -> Result<Self, Error> {
        let source = bytes.into_boxed_slice();
        // the boxed bytes stay where they are when the box moves into the terminal below
        let decoder = decoder(&source, options)?;
        let mut raw: ffi::GhosttyTerminal = std::ptr::null_mut();
        // SAFETY: the decoder is live and has not started; `raw` is a valid out-pointer, set to null on every error.
        Error::check(unsafe { ffi::ghostty_snapshot_decoder_ready(decoder.ptr(), &raw mut raw) })?;
        let mut terminal = Self::from_raw(raw)?;
        terminal.history = Some(PendingHistory {
            decoder,
            _source: source,
        });
        Ok(terminal)
    }

    /// Whether a snapshot's history is still being restored ([`Terminal::restore_snapshot`]).
    #[must_use]
    pub fn history_pending(&self) -> bool {
        self.history.is_some()
    }

    /// The history rows the snapshot being restored declares; `None` once none is pending.
    #[must_use]
    pub fn snapshot_history(&self) -> Option<SnapshotHistory> {
        let pending = self.history.as_ref()?;
        let rows = |data| {
            let mut value: u64 = 0;
            // SAFETY: the decoder is live and past READY; both history keys write a `uint64_t`.
            let result =
                unsafe { ffi::ghostty_snapshot_decoder_get(pending.decoder.ptr(), data, (&raw mut value).cast()) };
            (result == ffi::GHOSTTY_SUCCESS).then_some(value)
        };
        Some(SnapshotHistory {
            primary: rows(ffi::GHOSTTY_SNAPSHOT_DECODER_DATA_HISTORY_ROWS_PRIMARY)?,
            alternate: rows(ffi::GHOSTTY_SNAPSHOT_DECODER_DATA_HISTORY_ROWS_ALTERNATE),
        })
    }

    /// Restore the next history page of the snapshot this terminal came from; `None` once its FINISH marker
    /// validated (or when none is pending). The terminal may be rendered, resized and written between pages.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for malformed, corrupt or truncated data, [`Error::OutOfMemory`] when a page cannot be
    /// allocated. The decode ends there: the terminal keeps the history restored so far, and nothing is pending.
    pub fn restore_history_page(&mut self) -> Result<Option<HistoryPage>, Error> {
        let Some(pending) = self.history.as_ref() else {
            return Ok(None);
        };
        let decoder = pending.decoder.ptr();
        // SAFETY: the decoder is live and past READY, the terminal it borrows is this live one, and `&mut self`
        // serializes the call with every other use of the terminal.
        let result = unsafe { ffi::ghostty_snapshot_decoder_next(decoder) };
        match result {
            ffi::GHOSTTY_SUCCESS => Ok(Some(progress(decoder))),
            ffi::GHOSTTY_NO_VALUE => {
                self.history = None;
                Ok(None)
            }
            failed => {
                // the decoder is poisoned: only freeing it is allowed
                self.history = None;
                Err(Error::check(failed).err().unwrap_or(Error::Unknown(failed)))
            }
        }
    }
}

/// A decoder over `bytes` with `options` applied, the process-global hooks sealed first (it creates a terminal).
fn decoder(bytes: &[u8], options: SnapshotOptions) -> Result<Handle<ffi::GhosttySnapshotDecoderImpl>, Error> {
    crate::sys::seal();
    let mut raw: ffi::GhosttySnapshotDecoder = std::ptr::null_mut();
    // SAFETY: a null allocator selects the default one; the bytes stay valid and unchanged for the decoder's life (the
    // callers free it before the bytes, or keep both together); `raw` is a valid out-pointer.
    Error::check(unsafe {
        ffi::ghostty_snapshot_decoder_new_buf(std::ptr::null(), &raw mut raw, bytes.as_ptr(), bytes.len())
    })?;
    let decoder = Handle::new(raw, ffi::ghostty_snapshot_decoder_free)?;
    if let Some(max) = options.max_continuation_bytes {
        // SAFETY: the decoder is live and has not started; the option takes a `size_t *` read during the call.
        Error::check(unsafe {
            ffi::ghostty_snapshot_decoder_set(
                decoder.ptr(),
                ffi::GHOSTTY_SNAPSHOT_DECODER_OPT_MAX_CONTINUATION_BYTES,
                (&raw const max).cast(),
            )
        })?;
    }
    for (option, value) in [
        (
            ffi::GHOSTTY_SNAPSHOT_DECODER_OPT_RETAIN_CONTINUATION,
            options.retain_continuation,
        ),
        (
            ffi::GHOSTTY_SNAPSHOT_DECODER_OPT_COMPRESS_HISTORY,
            options.compress_history,
        ),
    ] {
        // SAFETY: as above; both options take a `bool *`.
        Error::check(unsafe { ffi::ghostty_snapshot_decoder_set(decoder.ptr(), option, (&raw const value).cast()) })?;
    }
    Ok(decoder)
}

/// The page `next` just restored.
fn progress(decoder: ffi::GhosttySnapshotDecoder) -> HistoryPage {
    // `next` just succeeded, so the progress keys hold values of their documented types; on a failure the defaults stay
    let mut screen: ffi::GhosttyTerminalScreen = ffi::GHOSTTY_TERMINAL_SCREEN_PRIMARY;
    // SAFETY: the decoder is live; `PROGRESS_SCREEN` writes a `GhosttyTerminalScreen`.
    unsafe {
        ffi::ghostty_snapshot_decoder_get(
            decoder,
            ffi::GHOSTTY_SNAPSHOT_DECODER_DATA_PROGRESS_SCREEN,
            (&raw mut screen).cast(),
        )
    };
    let mut rows: usize = 0;
    // SAFETY: as above; `PROGRESS_ROWS` writes a `size_t`.
    unsafe {
        ffi::ghostty_snapshot_decoder_get(
            decoder,
            ffi::GHOSTTY_SNAPSHOT_DECODER_DATA_PROGRESS_ROWS,
            (&raw mut rows).cast(),
        )
    };
    let mut remaining: u32 = 0;
    // SAFETY: as above; `PROGRESS_REMAINING` writes a `uint32_t`.
    unsafe {
        ffi::ghostty_snapshot_decoder_get(
            decoder,
            ffi::GHOSTTY_SNAPSHOT_DECODER_DATA_PROGRESS_REMAINING,
            (&raw mut remaining).cast(),
        )
    };
    HistoryPage {
        alternate: screen == ffi::GHOSTTY_TERMINAL_SCREEN_ALTERNATE,
        rows,
        remaining,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Continuation, Format, FormatExtras, FormatOptions, Mode, Region, ScrollbackLimits};

    const ALL: FormatOptions = FormatOptions {
        format: Format::Vt,
        unwrap: false,
        trim: false,
        extras: FormatExtras::RESTORE,
    };

    const LINES: usize = 6000;

    /// A terminal with several pages of history, styles, a title, modes and an open CSI.
    fn source() -> Terminal {
        let mut terminal = Terminal::new(20, 4).unwrap();
        terminal
            .set_scrollback_limits(ScrollbackLimits {
                max_bytes: None,
                max_lines: Some(10_000),
            })
            .unwrap();
        terminal.set_continuation_limit(1024).unwrap();
        for line in 0..LINES {
            terminal.vt_write(format!("\x1b[3{}mline {line}\x1b[0m\r\n", line % 8).as_bytes());
        }
        terminal.vt_write(b"\x1b]2;the title\x07\x1b[?2004h\x1b[?1hlast\x1b[1;3");
        terminal
    }

    fn screen(terminal: &mut Terminal) -> Vec<u8> {
        terminal.format(Region::Screen, ALL).unwrap().unwrap()
    }

    #[test]
    fn a_snapshot_round_trips_with_history_and_continuation() {
        let mut original = source();
        let bytes = original.encode_snapshot().unwrap();
        assert!(bytes.starts_with(b"GHOSTSNP"), "the envelope's magic");
        let options = SnapshotOptions {
            max_continuation_bytes: Some(1024),
            retain_continuation: true,
            compress_history: false,
        };
        let mut restored = Terminal::decode_snapshot(&bytes, options).unwrap();
        assert!(!restored.history_pending());
        assert_eq!(restored.total_rows(), original.total_rows());
        assert_eq!(restored.cursor(), original.cursor());
        assert_eq!(restored.title(), "the title");
        assert!(restored.mode(Mode::dec(2004)), "bracketed paste");
        assert_eq!(
            restored.continuation(),
            Ok(Continuation::Bytes(b"\x1b[1;3".to_vec())),
            "the open CSI is retained"
        );
        // the open CSI completes the same way in both
        original.vt_write(b"1mred");
        restored.vt_write(b"1mred");
        assert_eq!(screen(&mut restored), screen(&mut original));
    }

    #[test]
    fn an_incremental_restore_renders_at_ready_and_prepends_history_page_by_page() {
        let mut original = source();
        let bytes = original.encode_snapshot().unwrap();
        let mut restored = Terminal::restore_snapshot(bytes, SnapshotOptions::default()).unwrap();
        assert!(restored.history_pending());
        let history = restored.snapshot_history().unwrap();
        assert!(history.primary >= LINES as u64 - 4, "{history:?}");
        assert!(
            restored.total_rows() < original.total_rows(),
            "history comes after READY"
        );
        assert_eq!(
            restored.format(Region::Viewport, ALL).unwrap(),
            original.format(Region::Viewport, ALL).unwrap(),
            "renderable at READY"
        );

        let mut pages = 0;
        while let Some(page) = restored.restore_history_page().unwrap() {
            assert!(!page.alternate);
            pages += 1;
        }
        assert!(pages > 0);
        assert!(!restored.history_pending());
        assert_eq!(restored.snapshot_history(), None);
        assert_eq!(restored.restore_history_page(), Ok(None), "nothing left after FINISH");
        assert_eq!(restored.total_rows(), original.total_rows());
        // the continuation was not retained, but the parser state was: the open CSI completes as in the original
        assert_eq!(restored.continuation(), Err(Error::NoValue));
        restored.vt_write(b"1mx");
        original.vt_write(b"1mx");
        assert_eq!(screen(&mut restored), screen(&mut original));
    }

    #[test]
    fn a_restoring_terminal_takes_input_between_pages() {
        let bytes = source().encode_snapshot().unwrap();
        let mut restored = Terminal::restore_snapshot(bytes, SnapshotOptions::default()).unwrap();
        restored.restore_history_page().unwrap();
        restored.vt_write(b"m live\r\n");
        restored.resize(30, 5, 8, 16).unwrap();
        while restored.restore_history_page().unwrap().is_some() {}
        let text = restored
            .format(Region::Screen, FormatOptions::default())
            .unwrap()
            .unwrap();
        let text = String::from_utf8(text).unwrap();
        assert!(text.contains("last live"), "{text}");
    }

    #[test]
    fn a_corrupt_or_truncated_snapshot_is_refused() {
        let bytes = source().encode_snapshot().unwrap();
        let mut corrupt = bytes.clone();
        let middle = corrupt.len() / 2;
        corrupt[middle] ^= 0xff;
        let refused = |bytes: &[u8]| Terminal::decode_snapshot(bytes, SnapshotOptions::default()).err();
        assert_eq!(refused(&corrupt), Some(Error::InvalidValue), "a CRC mismatch");
        assert_eq!(
            refused(&bytes[..bytes.len() - 1]),
            Some(Error::InvalidValue),
            "truncated"
        );
        assert_eq!(refused(b"not a snapshot"), Some(Error::InvalidValue));
        assert_eq!(refused(&[]), Some(Error::InvalidValue));

        // a page corrupted after READY: the restore ends with the error and the terminal keeps what it has
        let mut late = bytes.clone();
        let near_end = late.len() - 40;
        late[near_end] ^= 0xff;
        let mut restored = Terminal::restore_snapshot(late, SnapshotOptions::default()).unwrap();
        let result = loop {
            match restored.restore_history_page() {
                Ok(Some(_)) => {}
                other => break other,
            }
        };
        assert_eq!(result, Err(Error::InvalidValue));
        assert!(!restored.history_pending());
        assert_eq!(restored.restore_history_page(), Ok(None));
        restored.vt_write(b"m still usable");
    }

    #[test]
    fn an_open_sequence_needs_tracking_to_encode_and_a_limit_to_decode() {
        let mut untracked = Terminal::new(10, 2).unwrap();
        untracked.vt_write(b"ok");
        assert!(untracked.encode_snapshot().is_ok(), "at ground, tracking is not needed");
        untracked.vt_write(b"\x1b[3");
        assert_eq!(untracked.encode_snapshot().err(), Some(Error::InvalidValue));

        let bytes = source().encode_snapshot().unwrap();
        let ground_only = SnapshotOptions {
            max_continuation_bytes: Some(0),
            ..SnapshotOptions::default()
        };
        assert_eq!(
            Terminal::decode_snapshot(&bytes, ground_only).err(),
            Some(Error::LimitExceeded)
        );
    }

    #[test]
    fn compressed_history_restores_the_same_text() {
        let mut original = source();
        let bytes = original.encode_snapshot().unwrap();
        let compressed = SnapshotOptions {
            compress_history: true,
            ..SnapshotOptions::default()
        };
        let mut restored = Terminal::decode_snapshot(&bytes, compressed).unwrap();
        let plain = FormatOptions::default();
        assert_eq!(
            restored.format(Region::Screen, plain).unwrap(),
            original.format(Region::Screen, plain).unwrap()
        );
    }
}
