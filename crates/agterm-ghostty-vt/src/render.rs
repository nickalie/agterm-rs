//! [`RenderState`]: an owned `GhosttyRenderState` with its row iterator and row cells, read through [`Rows`].

use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::{Error, Handle, Terminal, sys};

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
    cells: Handle<sys::GhosttyRenderStateRowCellsImpl>,
    rows: Handle<sys::GhosttyRenderStateRowIteratorImpl>,
    raw: Handle<sys::GhosttyRenderStateImpl>,
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
        let mut raw: sys::GhosttyRenderState = std::ptr::null_mut();
        // SAFETY: a null allocator selects the default one; `raw` is a valid out-pointer filled on success only.
        Error::check(unsafe { sys::ghostty_render_state_new(std::ptr::null(), &raw mut raw) })?;
        // Owned from here on, so an early return below frees what was already allocated.
        let raw = Handle::new(raw, sys::ghostty_render_state_free)?;
        let mut rows: sys::GhosttyRenderStateRowIterator = std::ptr::null_mut();
        // SAFETY: as above, for the row iterator.
        Error::check(unsafe { sys::ghostty_render_state_row_iterator_new(std::ptr::null(), &raw mut rows) })?;
        let rows = Handle::new(rows, sys::ghostty_render_state_row_iterator_free)?;
        let mut cells: sys::GhosttyRenderStateRowCells = std::ptr::null_mut();
        // SAFETY: as above, for the row cells.
        Error::check(unsafe { sys::ghostty_render_state_row_cells_new(std::ptr::null(), &raw mut cells) })?;
        let cells = Handle::new(cells, sys::ghostty_render_state_row_cells_free)?;
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
        let result = unsafe { sys::ghostty_render_state_update(self.raw.ptr(), terminal.as_raw()) };
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
        let result = unsafe { sys::ghostty_render_state_begin_update(self.raw.ptr(), terminal.as_raw()) };
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
        let mut value: sys::GhosttyRenderStateDirty = sys::GHOSTTY_RENDER_STATE_DIRTY_FULL;
        // SAFETY: the state is live; `GHOSTTY_RENDER_STATE_DATA_DIRTY` writes a `GhosttyRenderStateDirty`, the type
        // of `value`. A failure leaves `FULL` in place, the safe answer.
        unsafe {
            sys::ghostty_render_state_get(
                self.raw.ptr(),
                sys::GHOSTTY_RENDER_STATE_DATA_DIRTY,
                (&raw mut value).cast(),
            )
        };
        match value {
            sys::GHOSTTY_RENDER_STATE_DIRTY_FALSE => Dirty::Clean,
            sys::GHOSTTY_RENDER_STATE_DIRTY_PARTIAL => Dirty::Partial,
            _ => Dirty::Full,
        }
    }

    /// Mark the whole capture drawn: the global and every row's dirty flag.
    pub fn clean(&mut self) {
        // SAFETY: the state is live and `&mut self` makes this the only call on it; it cannot fail for a live state.
        unsafe { sys::ghostty_render_state_clean(self.raw.ptr()) };
    }

    /// The captured viewport's width and height in cells; (0, 0) after a failed update.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        if !self.valid {
            return (0, 0);
        }
        (
            self.get_u16(sys::GHOSTTY_RENDER_STATE_DATA_COLS),
            self.get_u16(sys::GHOSTTY_RENDER_STATE_DATA_ROWS),
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
            sys::ghostty_render_state_get(
                self.raw.ptr(),
                sys::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
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

    fn get_u16(&self, data: sys::GhosttyRenderStateData) -> u16 {
        let mut value: u16 = 0;
        // SAFETY: the state is live; the keys passed here write a `uint16_t`, the type of `value`.
        unsafe { sys::ghostty_render_state_get(self.raw.ptr(), data, (&raw mut value).cast()) };
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
        Error::check(unsafe { sys::ghostty_render_state_end_update(self.state.raw.ptr()) })
    }
}

impl Drop for PendingUpdate<'_> {
    fn drop(&mut self) {
        if !self.ended {
            // SAFETY: as in `end`; a begun update must always be ended before the state is read again.
            unsafe { sys::ghostty_render_state_end_update(self.state.raw.ptr()) };
        }
    }
}

/// The captured rows of a [`RenderState`], walked with [`Rows::next_row`] (each row borrows the walk).
#[derive(Debug)]
pub struct Rows<'a> {
    iterator: NonNull<sys::GhosttyRenderStateRowIteratorImpl>,
    cells: NonNull<sys::GhosttyRenderStateRowCellsImpl>,
    state: PhantomData<&'a mut RenderState>,
}

impl Rows<'_> {
    /// The next row, top to bottom, or `None` past the last captured row.
    pub fn next_row(&mut self) -> Option<RowRef<'_>> {
        // SAFETY: the iterator is live, owned by the borrowed state, and was filled by `RenderState::rows` from a
        // state that cannot be updated while `self` borrows it.
        let moved = unsafe { sys::ghostty_render_state_row_iterator_next(self.iterator.as_ptr()) };
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
    iterator: NonNull<sys::GhosttyRenderStateRowIteratorImpl>,
    cells: NonNull<sys::GhosttyRenderStateRowCellsImpl>,
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
            sys::ghostty_render_state_row_get(
                self.iterator.as_ptr(),
                sys::GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY,
                (&raw mut dirty).cast(),
            )
        };
        dirty
    }

    /// The row's text: each cell's grapheme cluster, a blank cell as one space and the spacer cell of a wide
    /// character as nothing, so the string reads as the screen shows it (trailing blanks included).
    ///
    /// # Errors
    ///
    /// The library's result when the row's cells cannot be read.
    pub fn text(&self) -> Result<String, Error> {
        let mut cells = self.cells.as_ptr();
        // SAFETY: the iterator is positioned on this row; `ROW_DATA_CELLS` refills the pre-allocated cells handle
        // through a pointer to it.
        let result = unsafe {
            sys::ghostty_render_state_row_get(
                self.iterator.as_ptr(),
                sys::GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                (&raw mut cells).cast(),
            )
        };
        Error::check(result)?;
        let mut text = Vec::new();
        // SAFETY: the cells handle was just filled for this row and stays valid while the state is borrowed.
        while unsafe { sys::ghostty_render_state_row_cells_next(self.cells.as_ptr()) } {
            if !is_spacer(self.cells)? {
                push_grapheme(self.cells, &mut text)?;
            }
        }
        Ok(String::from_utf8_lossy(&text).into_owned())
    }
}

/// Whether the current cell is the spacer half of a wide character (or the head spacer a wrapped one leaves).
fn is_spacer(cells: NonNull<sys::GhosttyRenderStateRowCellsImpl>) -> Result<bool, Error> {
    let mut raw: sys::GhosttyCell = 0;
    // SAFETY: the cells handle is positioned on a cell; `ROW_CELLS_DATA_RAW` writes a `GhosttyCell`.
    let result = unsafe {
        sys::ghostty_render_state_row_cells_get(
            cells.as_ptr(),
            sys::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
            (&raw mut raw).cast(),
        )
    };
    Error::check(result)?;
    let mut wide: sys::GhosttyCellWide = sys::GHOSTTY_CELL_WIDE_NARROW;
    // SAFETY: `raw` is a cell value; `GHOSTTY_CELL_DATA_WIDE` writes a `GhosttyCellWide`.
    Error::check(unsafe { sys::ghostty_cell_get(raw, sys::GHOSTTY_CELL_DATA_WIDE, (&raw mut wide).cast()) })?;
    Ok(matches!(
        wide,
        sys::GHOSTTY_CELL_WIDE_SPACER_TAIL | sys::GHOSTTY_CELL_WIDE_SPACER_HEAD
    ))
}

/// Append the current cell's grapheme cluster as UTF-8, or a space for a cell without text.
fn push_grapheme(cells: NonNull<sys::GhosttyRenderStateRowCellsImpl>, text: &mut Vec<u8>) -> Result<(), Error> {
    let mut stack = [0_u8; 32];
    let mut buffer = sys::GhosttyBuffer {
        ptr: stack.as_mut_ptr(),
        cap: stack.len(),
        len: 0,
    };
    // SAFETY: the cells handle is positioned on a cell; `GRAPHEMES_UTF8` writes at most `cap` bytes to `ptr` (a live
    // stack buffer of that size) and sets `len`, or sets `len` to the size it needs and writes nothing.
    let result = unsafe { read_grapheme(cells, &mut buffer) };
    if result == sys::GHOSTTY_OUT_OF_SPACE {
        let mut heap = vec![0_u8; buffer.len];
        buffer = sys::GhosttyBuffer {
            ptr: heap.as_mut_ptr(),
            cap: heap.len(),
            len: 0,
        };
        // SAFETY: as above, with a heap buffer of the size the library asked for.
        Error::check(unsafe { read_grapheme(cells, &mut buffer) })?;
        text.extend_from_slice(&heap[..buffer.len.min(heap.len())]);
        return Ok(());
    }
    Error::check(result)?;
    if buffer.len == 0 {
        text.push(b' ');
    } else {
        text.extend_from_slice(&stack[..buffer.len.min(stack.len())]);
    }
    Ok(())
}

/// # Safety
///
/// `cells` must be positioned on a cell and `buffer.ptr` must be valid for `buffer.cap` bytes of writes.
unsafe fn read_grapheme(
    cells: NonNull<sys::GhosttyRenderStateRowCellsImpl>,
    buffer: &mut sys::GhosttyBuffer,
) -> sys::GhosttyResult {
    // SAFETY: the caller's contract; `GRAPHEMES_UTF8` takes a `GhosttyBuffer *`.
    unsafe {
        sys::ghostty_render_state_row_cells_get(
            cells.as_ptr(),
            sys::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8,
            std::ptr::from_mut(buffer).cast(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(state: &mut RenderState) -> Vec<String> {
        let mut rows = state.rows().unwrap();
        let mut lines = Vec::new();
        while let Some(row) = rows.next_row() {
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
