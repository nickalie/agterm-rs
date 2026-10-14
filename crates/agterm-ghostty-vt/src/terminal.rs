//! [`Terminal`]: an owned `GhosttyTerminal`.

use crate::{Error, Handle, sys};

/// One VT: parser, screens and scrollback, owned and freed on drop.
///
/// Mutating calls take `&mut self`, which serializes them as the C API asks; the handle is `Send` and not `Sync`
/// (crate docs).
#[derive(Debug)]
pub struct Terminal {
    raw: Handle<sys::GhosttyTerminalImpl>,
}

// SAFETY: libghostty-vt creates no threads and keeps no thread-local state (`grep threadlocal src/terminal` is empty
// at LIBGHOSTTY_VT_REV; its process-global sys hooks are plain variables), and a terminal may be used from any thread
// as long as calls are serialized (`render.h`, `terminal.h`). Moving the owned handle to another thread is therefore
// sound. It is deliberately not `Sync`: `&Terminal` reads must not race the `&mut` calls of another thread.
unsafe impl Send for Terminal {}

impl Terminal {
    /// A terminal of `cols` x `rows` cells with the library's defaults (scrollback limits and the rest).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a zero dimension, [`Error::OutOfMemory`] when the allocation fails.
    pub fn new(cols: u16, rows: u16) -> Result<Self, Error> {
        let mut raw: sys::GhosttyTerminal = std::ptr::null_mut();
        // SAFETY: a null allocator selects the library's default one; `raw` is a valid out-pointer that the call
        // fills on success only.
        let result = unsafe { sys::ghostty_terminal_new(std::ptr::null(), &raw mut raw, cols, rows) };
        Error::check(result)?;
        Ok(Self {
            raw: Handle::new(raw, sys::ghostty_terminal_free)?,
        })
    }

    /// Feed bytes from the PTY through the VT parser. Malformed input never fails: the library logs it and keeps the
    /// terminal consistent.
    pub fn vt_write(&mut self, bytes: &[u8]) {
        // SAFETY: `self.raw` is a live terminal owned by `self`, `&mut self` makes this the only call on it, and the
        // pointer and length describe the borrowed slice for the duration of the call.
        unsafe { sys::ghostty_terminal_vt_write(self.raw.ptr(), bytes.as_ptr(), bytes.len()) };
    }

    /// Resize to `cols` x `rows` cells of `cell_width_px` x `cell_height_px` pixels: the primary screen reflows,
    /// synchronized output ends, and an in-band size report goes out when mode 2048 is on.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a zero dimension, [`Error::OutOfMemory`] when reflow cannot allocate.
    pub fn resize(&mut self, cols: u16, rows: u16, cell_width_px: u32, cell_height_px: u32) -> Result<(), Error> {
        // SAFETY: `self.raw` is a live terminal owned by `self` and `&mut self` makes this the only call on it.
        let result = unsafe { sys::ghostty_terminal_resize(self.raw.ptr(), cols, rows, cell_width_px, cell_height_px) };
        Error::check(result)
    }

    /// The width in cells.
    #[must_use]
    pub fn cols(&self) -> u16 {
        self.get_u16(sys::GHOSTTY_TERMINAL_DATA_COLS)
    }

    /// The height in cells.
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.get_u16(sys::GHOSTTY_TERMINAL_DATA_ROWS)
    }

    /// The cursor's column and row in the active area, zero-based.
    #[must_use]
    pub fn cursor(&self) -> (u16, u16) {
        (
            self.get_u16(sys::GHOSTTY_TERMINAL_DATA_CURSOR_X),
            self.get_u16(sys::GHOSTTY_TERMINAL_DATA_CURSOR_Y),
        )
    }

    /// The raw handle, for the wrapper's other types; it stays owned by `self`.
    pub(crate) fn as_raw(&mut self) -> sys::GhosttyTerminal {
        self.raw.ptr()
    }

    /// A `uint16_t` datum (`terminal.h` documents the output type of each key read through here).
    fn get_u16(&self, data: sys::GhosttyTerminalData) -> u16 {
        let mut value: u16 = 0;
        // SAFETY: `self.raw` is a live terminal owned by `self`; every key passed here has `uint16_t *` as its output
        // type, so `value` is a valid, correctly sized out-pointer. The keys are always set, so the result is ignored
        // and a failure would leave the zero in place.
        unsafe { sys::ghostty_terminal_get(self.raw.ptr(), data, (&raw mut value).cast()) };
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_dimension_is_refused() {
        assert_eq!(Terminal::new(0, 24).err(), Some(Error::InvalidValue));
        assert_eq!(Terminal::new(80, 0).err(), Some(Error::InvalidValue));
    }

    #[test]
    fn a_new_terminal_reports_its_size_and_a_home_cursor() {
        let terminal = Terminal::new(80, 24).unwrap();
        assert_eq!((terminal.cols(), terminal.rows()), (80, 24));
        assert_eq!(terminal.cursor(), (0, 0));
    }

    #[test]
    fn writes_move_the_cursor_and_resize_changes_the_size() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        terminal.vt_write(b"abc\r\nde");
        assert_eq!(terminal.cursor(), (2, 1));
        terminal.resize(100, 30, 8, 16).unwrap();
        assert_eq!((terminal.cols(), terminal.rows()), (100, 30));
        assert_eq!(terminal.resize(0, 30, 8, 16), Err(Error::InvalidValue));
    }
}
