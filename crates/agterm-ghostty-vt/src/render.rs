//! [`RenderState`]: an owned `GhosttyRenderState` with its row iterator and row cells, read through [`Rows`].

use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{CellWidth, Cells, Error, Handle, HeldTerminal, Rgb, Terminal, ffi};

/// How much of the capture changed since the last [`RenderState::clean`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dirty {
    /// Nothing changed; drawing can be skipped.
    Clean,
    /// Some rows changed ([`RowRef::is_dirty`] says which).
    Partial,
    /// Global state changed; every row is redrawn.
    Full,
}

/// A row's identity across captures ([`RowRef::id`]); compare for equality only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct RowId(pub [u64; 2]);

/// The cursor's visual style in a capture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum CursorStyle {
    Bar,
    #[default]
    Block,
    Underline,
    BlockHollow,
}

/// Where the cursor sits in the viewport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CursorPosition {
    pub x: u16,
    pub y: u16,
    /// On the tail of a wide character.
    pub wide_tail: bool,
}

/// The cursor in a capture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct RenderCursor {
    /// The cursor's viewport cell when it is inside the viewport.
    pub position: Option<CursorPosition>,
    /// Visible per the terminal's modes (DECTCEM).
    pub visible: bool,
    /// Blinking per the terminal's modes.
    pub blinking: bool,
    /// At a password prompt.
    pub password_input: bool,
    pub style: CursorStyle,
}

/// The colors of a capture: effective defaults (OSC overrides applied) and the 256-color palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderColors {
    pub background: Rgb,
    pub foreground: Rgb,
    /// The cursor color when one is set explicitly.
    pub cursor: Option<Rgb>,
    pub palette: Box<[Rgb; 256]>,
}

/// The capture a renderer draws from: the viewport's rows, cells, colors and cursor as of the last update.
///
/// Updating needs the terminal only for the begin phase ([`RenderState::begin_update`]), so a caller sharing the
/// terminal behind a lock holds it for that call alone. Rows read through [`RenderState::rows`] borrow the state, so
/// the compiler keeps them from outliving the next update, which invalidates them in C. A failed update can leave the
/// C state half built (its row count set before its row storage grew), so until an update succeeds again the state
/// reads as empty: no rows, size (0, 0). `Send`, not `Sync`.
#[derive(Debug)]
pub struct RenderState {
    // Allocated once and refilled on every read, as `render.h` suggests, rather than per frame or per row. Fields drop
    // in declaration order: the cells and the iterator go before the state they read.
    cells: Handle<ffi::GhosttyRenderStateRowCellsImpl>,
    rows: Handle<ffi::GhosttyRenderStateRowIteratorImpl>,
    raw: Handle<ffi::GhosttyRenderStateImpl>,
    // False after a failed update or begin: the C capture may claim rows its storage does not hold.
    valid: bool,
}

// SAFETY: the render state, its row iterator and its row cells are plain library-owned memory with no thread
// affinity (no threads, no thread-local state in libghostty-vt at LIBGHOSTTY_VT_REV); `render.h` documents a renderer
// thread updating it from a terminal another thread writes, under a lock. Every call goes through `&mut self` or a
// borrow of it, so moving the owner across threads is sound; it is not `Sync`.
unsafe impl Send for RenderState {}

impl RenderState {
    /// An empty render state; update it from a terminal before reading.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] when an allocation fails.
    pub fn new() -> Result<Self, Error> {
        let mut raw: ffi::GhosttyRenderState = std::ptr::null_mut();
        // SAFETY: a null allocator selects the default one; `raw` is a valid out-pointer filled on success only.
        Error::check(unsafe { ffi::ghostty_render_state_new(std::ptr::null(), &raw mut raw) })?;
        // Owned from here on, so an early return below frees what was already allocated.
        let raw = Handle::new(raw, ffi::ghostty_render_state_free)?;
        let mut rows: ffi::GhosttyRenderStateRowIterator = std::ptr::null_mut();
        // SAFETY: as above, for the row iterator.
        Error::check(unsafe { ffi::ghostty_render_state_row_iterator_new(std::ptr::null(), &raw mut rows) })?;
        let rows = Handle::new(rows, ffi::ghostty_render_state_row_iterator_free)?;
        let mut cells: ffi::GhosttyRenderStateRowCells = std::ptr::null_mut();
        // SAFETY: as above, for the row cells.
        Error::check(unsafe { ffi::ghostty_render_state_row_cells_new(std::ptr::null(), &raw mut cells) })?;
        let cells = Handle::new(cells, ffi::ghostty_render_state_row_cells_free)?;
        Ok(Self {
            cells,
            rows,
            raw,
            valid: true,
        })
    }

    /// Capture the terminal in one call (both phases).
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] when the capture cannot allocate.
    pub fn update(&mut self, terminal: &mut Terminal) -> Result<(), Error> {
        // SAFETY: both handles are live and owned by their `&mut` borrowers, so no other call runs on either.
        let result = unsafe { ffi::ghostty_render_state_update(self.raw.ptr(), terminal.as_raw()) };
        self.record(Error::check(result))
    }

    /// Begin a two-phase capture: the only phase that reads the terminal. The returned [`PendingUpdate`] completes
    /// the capture without the terminal ([`PendingUpdate::end`], or on drop), so a lock around the terminal can be
    /// released in between; the state cannot be read until then.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] when the capture cannot allocate.
    pub fn begin_update(&mut self, terminal: &mut Terminal) -> Result<PendingUpdate<'_>, Error> {
        // SAFETY: both handles are live and owned by their `&mut` borrowers, so no other call runs on either.
        let result = unsafe { ffi::ghostty_render_state_begin_update(self.raw.ptr(), terminal.as_raw()) };
        let result = self.record(Error::check(result));
        let pending = PendingUpdate {
            state: self,
            ended: false,
        };
        result.map(|()| pending)
    }

    /// Keep the outcome of an update: a failure invalidates the capture until the next success.
    fn record(&mut self, result: Result<(), Error>) -> Result<(), Error> {
        self.valid = result.is_ok();
        result
    }

    /// How much changed since the last [`RenderState::clean`].
    #[must_use]
    pub fn dirty(&self) -> Dirty {
        let mut value: ffi::GhosttyRenderStateDirty = ffi::GHOSTTY_RENDER_STATE_DIRTY_FULL;
        // SAFETY: the state is live; `GHOSTTY_RENDER_STATE_DATA_DIRTY` writes a `GhosttyRenderStateDirty`, the type
        // of `value`. A failure leaves `FULL` in place, the safe answer.
        unsafe {
            ffi::ghostty_render_state_get(
                self.raw.ptr(),
                ffi::GHOSTTY_RENDER_STATE_DATA_DIRTY,
                (&raw mut value).cast(),
            )
        };
        match value {
            ffi::GHOSTTY_RENDER_STATE_DIRTY_FALSE => Dirty::Clean,
            ffi::GHOSTTY_RENDER_STATE_DIRTY_PARTIAL => Dirty::Partial,
            _ => Dirty::Full,
        }
    }

    /// Mark the whole capture drawn: the global and every row's dirty flag.
    pub fn clean(&mut self) {
        // SAFETY: the state is live and `&mut self` makes this the only call on it; it cannot fail for a live state.
        unsafe { ffi::ghostty_render_state_clean(self.raw.ptr()) };
    }

    /// The captured viewport's width and height in cells; (0, 0) after a failed update.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        if !self.valid {
            return (0, 0);
        }
        (
            self.get_u16(ffi::GHOSTTY_RENDER_STATE_DATA_COLS),
            self.get_u16(ffi::GHOSTTY_RENDER_STATE_DATA_ROWS),
        )
    }

    /// The captured rows, top to bottom. They borrow the state, so the next update waits until they are dropped.
    ///
    /// # Errors
    ///
    /// [`Error::NoValue`] after a failed update, until an update succeeds (the C capture may be half built); the
    /// library's result when the iterator cannot be filled. A state never updated yields no rows.
    pub fn rows(&mut self) -> Result<Rows<'_>, Error> {
        if !self.valid {
            return Err(Error::NoValue);
        }
        let mut iterator = self.rows.ptr();
        // SAFETY: the state and its iterator are live and owned by `self`; `GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR`
        // takes a pointer to the pre-allocated iterator handle and refills that iterator.
        let result = unsafe {
            ffi::ghostty_render_state_get(
                self.raw.ptr(),
                ffi::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                (&raw mut iterator).cast(),
            )
        };
        Error::check(result)?;
        Ok(Rows {
            iterator: self.rows.non_null(),
            cells: self.cells.non_null(),
            state: PhantomData,
        })
    }

    /// Capture the terminal from inside [`Effects::render_hold`](crate::Effects::render_hold) (both phases), as
    /// `terminal.h` asks when a hold begins.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] when the capture cannot allocate.
    pub fn update_held(&mut self, terminal: &mut HeldTerminal<'_>) -> Result<(), Error> {
        // SAFETY: the state is live and owned by `&mut self`; `terminal` is the terminal running the hold callback,
        // whose caller holds whatever serializes it, and capturing from there is what the C API documents.
        let result = unsafe { ffi::ghostty_render_state_update(self.raw.ptr(), terminal.as_raw()) };
        self.record(Error::check(result))
    }

    /// The captured colors, or `None` after a failed update or before the first one.
    #[must_use]
    pub fn colors(&self) -> Option<RenderColors> {
        if !self.valid {
            return None;
        }
        // SAFETY: an all-zero `GhosttyRenderStateColors` is a valid value (plain integers and a bool), sized below.
        let mut raw: ffi::GhosttyRenderStateColors = unsafe { std::mem::zeroed() };
        raw.size = std::mem::size_of::<ffi::GhosttyRenderStateColors>();
        // SAFETY: the state is live; `DATA_COLORS` writes a sized `GhosttyRenderStateColors`.
        let result = unsafe {
            ffi::ghostty_render_state_get(
                self.raw.ptr(),
                ffi::GHOSTTY_RENDER_STATE_DATA_COLORS,
                (&raw mut raw).cast(),
            )
        };
        if result != ffi::GHOSTTY_SUCCESS {
            return None;
        }
        Some(RenderColors {
            background: raw.background.into(),
            foreground: raw.foreground.into(),
            cursor: raw.cursor_has_value.then(|| raw.cursor.into()),
            palette: Box::new(raw.palette.map(Rgb::from)),
        })
    }

    /// The captured cursor, or `None` after a failed update or before the first one.
    #[must_use]
    pub fn cursor(&self) -> Option<RenderCursor> {
        if !self.valid {
            return None;
        }
        // SAFETY: an all-zero `GhosttyRenderStateCursor` is a valid value, sized below.
        let mut raw: ffi::GhosttyRenderStateCursor = unsafe { std::mem::zeroed() };
        raw.size = std::mem::size_of::<ffi::GhosttyRenderStateCursor>();
        // SAFETY: the state is live; `DATA_CURSOR` writes a sized `GhosttyRenderStateCursor`.
        let result = unsafe {
            ffi::ghostty_render_state_get(
                self.raw.ptr(),
                ffi::GHOSTTY_RENDER_STATE_DATA_CURSOR,
                (&raw mut raw).cast(),
            )
        };
        if result != ffi::GHOSTTY_SUCCESS {
            return None;
        }
        let style = match raw.visual_style {
            ffi::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BAR => CursorStyle::Bar,
            ffi::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_UNDERLINE => CursorStyle::Underline,
            ffi::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BLOCK_HOLLOW => CursorStyle::BlockHollow,
            _ => CursorStyle::Block,
        };
        Some(RenderCursor {
            position: raw.viewport_has_value.then_some(CursorPosition {
                x: raw.viewport_x,
                y: raw.viewport_y,
                wide_tail: raw.wide_tail,
            }),
            visible: raw.visible,
            blinking: raw.blinking,
            password_input: raw.password_input,
            style,
        })
    }

    fn get_u16(&self, data: ffi::GhosttyRenderStateData) -> u16 {
        let mut value: u16 = 0;
        // SAFETY: the state is live; the keys passed here write a `uint16_t`, the type of `value`.
        unsafe { ffi::ghostty_render_state_get(self.raw.ptr(), data, (&raw mut value).cast()) };
        value
    }
}

/// A begun two-phase update ([`RenderState::begin_update`]); the capture is complete once it ends.
#[derive(Debug)]
pub struct PendingUpdate<'a> {
    state: &'a mut RenderState,
    ended: bool,
}

impl PendingUpdate<'_> {
    /// Complete the capture with render-state memory only (safe while the terminal is being written).
    ///
    /// # Errors
    ///
    /// The library's result; it fails only for a null state, which this type cannot hold.
    pub fn end(mut self) -> Result<(), Error> {
        self.ended = true;
        // SAFETY: the state is live and borrowed mutably by `self`; end touches only memory the state owns.
        Error::check(unsafe { ffi::ghostty_render_state_end_update(self.state.raw.ptr()) })
    }
}

impl Drop for PendingUpdate<'_> {
    fn drop(&mut self) {
        if !self.ended {
            // SAFETY: as in `end`; a begun update must always be ended before the state is read again.
            unsafe { ffi::ghostty_render_state_end_update(self.state.raw.ptr()) };
        }
    }
}

/// The captured rows of a [`RenderState`], walked with [`Rows::next_row`] (each row borrows the walk).
#[derive(Debug)]
pub struct Rows<'a> {
    iterator: NonNull<ffi::GhosttyRenderStateRowIteratorImpl>,
    cells: NonNull<ffi::GhosttyRenderStateRowCellsImpl>,
    state: PhantomData<&'a mut RenderState>,
}

impl Rows<'_> {
    /// The next row, top to bottom, or `None` past the last captured row.
    pub fn next_row(&mut self) -> Option<RowRef<'_>> {
        // SAFETY: the iterator is live, owned by the borrowed state, and was filled by `RenderState::rows` from a
        // state that cannot be updated while `self` borrows it.
        let moved = unsafe { ffi::ghostty_render_state_row_iterator_next(self.iterator.as_ptr()) };
        moved.then_some(RowRef {
            iterator: self.iterator,
            cells: self.cells,
            rows: PhantomData,
        })
    }
}

/// One captured row.
#[derive(Debug)]
pub struct RowRef<'a> {
    iterator: NonNull<ffi::GhosttyRenderStateRowIteratorImpl>,
    cells: NonNull<ffi::GhosttyRenderStateRowCellsImpl>,
    rows: PhantomData<&'a mut ()>,
}

impl RowRef<'_> {
    /// Whether the row changed since the last [`RenderState::clean`].
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        let mut dirty = true;
        // SAFETY: the iterator is positioned on this row (`next_row` returned it); `ROW_DATA_DIRTY` writes a `bool`.
        // A failure leaves `true`, the safe answer.
        unsafe {
            ffi::ghostty_render_state_row_get(
                self.iterator.as_ptr(),
                ffi::GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY,
                (&raw mut dirty).cast(),
            )
        };
        dirty
    }

    /// The row's identity across updates (`render.h` "Row Identity"): a row keeps its id while it moves, loses it
    /// when changed in place, and ids are never reused.
    #[must_use]
    pub fn id(&self) -> RowId {
        let mut id = ffi::GhosttyRenderStateRowId { bits: [0, 0] };
        // SAFETY: the iterator is positioned on this row; `ROW_DATA_ID` writes a `GhosttyRenderStateRowId`. A failure
        // leaves the all-zero id, which is never valid and so never matches a cached row.
        unsafe {
            ffi::ghostty_render_state_row_get(
                self.iterator.as_ptr(),
                ffi::GHOSTTY_RENDER_STATE_ROW_DATA_ID,
                (&raw mut id).cast(),
            )
        };
        RowId(id.bits)
    }

    /// The row's cells, left to right.
    ///
    /// # Errors
    ///
    /// The library's result when the row's cells cannot be read.
    pub fn cells(&mut self) -> Result<Cells<'_>, Error> {
        let mut cells = self.cells.as_ptr();
        // SAFETY: the iterator is positioned on this row; `ROW_DATA_CELLS` refills the pre-allocated cells handle
        // through a pointer to it.
        let result = unsafe {
            ffi::ghostty_render_state_row_get(
                self.iterator.as_ptr(),
                ffi::GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                (&raw mut cells).cast(),
            )
        };
        Error::check(result)?;
        // SAFETY: just filled for this row; `&mut self` keeps the row (and so the state) borrowed while it lives.
        Ok(unsafe { Cells::new(self.cells) })
    }

    /// The row's text: each cell's grapheme cluster, a blank cell as one space and the spacer cell of a wide
    /// character as nothing, so the string reads as the screen shows it (trailing blanks included).
    ///
    /// # Errors
    ///
    /// The library's result when the row's cells cannot be read.
    pub fn text(&mut self) -> Result<String, Error> {
        let mut cells = self.cells()?;
        let mut text = Vec::new();
        while let Some(cell) = cells.next_cell() {
            let cell = cell?;
            if matches!(cell.width(), CellWidth::SpacerTail | CellWidth::SpacerHead) {
                continue;
            }
            let before = text.len();
            cell.grapheme(&mut text)?;
            if text.len() == before {
                text.push(b' ');
            }
        }
        Ok(String::from_utf8_lossy(&text).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(state: &mut RenderState) -> Vec<String> {
        let mut rows = state.rows().unwrap();
        let mut lines = Vec::new();
        while let Some(mut row) = rows.next_row() {
            lines.push(row.text().unwrap().trim_end().to_owned());
        }
        lines
    }

    #[test]
    fn a_state_never_updated_is_empty_and_clean() {
        let mut state = RenderState::new().unwrap();
        assert_eq!(state.size(), (0, 0));
        assert_eq!(state.dirty(), Dirty::Clean);
        assert!(state.rows().unwrap().next_row().is_none());
    }

    #[test]
    fn a_failed_update_reads_as_empty_until_the_next_success() {
        let mut terminal = Terminal::new(10, 2).unwrap();
        terminal.vt_write(b"kept");
        let mut state = RenderState::new().unwrap();
        state.update(&mut terminal).unwrap();
        // the native failure (allocation) cannot be provoked here, so feed its result to the bookkeeping directly
        assert_eq!(state.record(Err(Error::OutOfMemory)), Err(Error::OutOfMemory));
        assert_eq!(state.size(), (0, 0));
        assert_eq!(state.rows().err(), Some(Error::NoValue));
        state.update(&mut terminal).unwrap();
        assert_eq!(state.size(), (10, 2));
        assert_eq!(screen(&mut state), ["kept", ""]);
        state.record(Err(Error::OutOfMemory)).unwrap_err();
        state.begin_update(&mut terminal).unwrap().end().unwrap();
        assert_eq!(screen(&mut state), ["kept", ""]);
    }

    #[test]
    fn an_update_captures_the_size_and_dirties_the_frame_until_clean() {
        let mut terminal = Terminal::new(20, 4).unwrap();
        let mut state = RenderState::new().unwrap();
        state.update(&mut terminal).unwrap();
        assert_eq!(state.size(), (20, 4));
        assert_ne!(state.dirty(), Dirty::Clean);
        state.clean();
        assert_eq!(state.dirty(), Dirty::Clean);
    }

    #[test]
    fn a_two_phase_update_reads_the_same_screen() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.vt_write(b"one\r\ntwo");
        let mut state = RenderState::new().unwrap();
        state.begin_update(&mut terminal).unwrap().end().unwrap();
        assert_eq!(screen(&mut state), ["one", "two", ""]);
        terminal.vt_write(b"\r\nthree");
        drop(state.begin_update(&mut terminal).unwrap());
        assert_eq!(screen(&mut state), ["one", "two", "three"]);
    }

    #[test]
    fn only_rows_written_after_clean_are_dirty() {
        let mut terminal = Terminal::new(10, 3).unwrap();
        // the cursor's row is dirty too, so it waits on the row the write lands on
        terminal.vt_write(b"\r\n");
        let mut state = RenderState::new().unwrap();
        state.update(&mut terminal).unwrap();
        state.clean();
        terminal.vt_write(b"x");
        state.update(&mut terminal).unwrap();
        let mut rows = state.rows().unwrap();
        let mut dirty = Vec::new();
        while let Some(row) = rows.next_row() {
            dirty.push(row.is_dirty());
        }
        assert_eq!(dirty, [false, true, false]);
    }
}
