//! Points in the grid and the terminal's selection (`point.h`, `grid_ref.h`, `selection.h`).
//!
//! The C API hands out selections as snapshots of two grid references, valid only until the next mutating call on the
//! terminal. This wrapper never lets one escape: every call resolves points to references, uses them and converts
//! back to points within one borrow of the terminal. The terminal keeps its installed selection tracked across
//! scrolling, reflow and pruning by itself.

use crate::{Error, Terminal, ffi};

/// The coordinate space of a [`Point`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PointTag {
    /// The active area, where the cursor moves (the bottom `rows` rows of the screen).
    Active,
    /// What is visible now, scrolled back or not.
    #[default]
    Viewport,
    /// The whole screen: scrollback, then the active area.
    Screen,
    /// The scrollback only.
    History,
}

impl PointTag {
    pub(crate) fn raw(self) -> ffi::GhosttyPointTag {
        match self {
            Self::Active => ffi::GHOSTTY_POINT_TAG_ACTIVE,
            Self::Viewport => ffi::GHOSTTY_POINT_TAG_VIEWPORT,
            Self::Screen => ffi::GHOSTTY_POINT_TAG_SCREEN,
            Self::History => ffi::GHOSTTY_POINT_TAG_HISTORY,
        }
    }
}

/// A cell of the grid: column `x` and row `y` (zero-based) in `tag`'s space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Point {
    pub tag: PointTag,
    pub x: u16,
    pub y: u32,
}

impl Point {
    #[must_use]
    pub const fn new(tag: PointTag, x: u16, y: u32) -> Self {
        Self { tag, x, y }
    }

    fn raw(self) -> ffi::GhosttyPoint {
        ffi::GhosttyPoint {
            tag: self.tag.raw(),
            value: ffi::GhosttyPointValue {
                coordinate: ffi::GhosttyPointCoordinate { x: self.x, y: self.y },
            },
        }
    }
}

/// A selection's ends (both inclusive, in the order the user made them, so `start` may come after `end`), and
/// whether they are the corners of a rectangle rather than the ends of a run of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Selection {
    pub start: Point,
    pub end: Point,
    pub rectangle: bool,
}

/// How [`Terminal::adjust_selection`] moves the selection's end (Ghostty's `adjust_selection` directions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectionAdjust {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    BeginningOfLine,
    EndOfLine,
}

impl SelectionAdjust {
    fn raw(self) -> ffi::GhosttySelectionAdjust {
        match self {
            Self::Left => ffi::GHOSTTY_SELECTION_ADJUST_LEFT,
            Self::Right => ffi::GHOSTTY_SELECTION_ADJUST_RIGHT,
            Self::Up => ffi::GHOSTTY_SELECTION_ADJUST_UP,
            Self::Down => ffi::GHOSTTY_SELECTION_ADJUST_DOWN,
            Self::Home => ffi::GHOSTTY_SELECTION_ADJUST_HOME,
            Self::End => ffi::GHOSTTY_SELECTION_ADJUST_END,
            Self::PageUp => ffi::GHOSTTY_SELECTION_ADJUST_PAGE_UP,
            Self::PageDown => ffi::GHOSTTY_SELECTION_ADJUST_PAGE_DOWN,
            Self::BeginningOfLine => ffi::GHOSTTY_SELECTION_ADJUST_BEGINNING_OF_LINE,
            Self::EndOfLine => ffi::GHOSTTY_SELECTION_ADJUST_END_OF_LINE,
        }
    }
}

pub(crate) fn empty_ref() -> ffi::GhosttyGridRef {
    ffi::GhosttyGridRef {
        size: std::mem::size_of::<ffi::GhosttyGridRef>(),
        node: std::ptr::null_mut(),
        x: 0,
        y: 0,
    }
}

pub(crate) fn empty_selection() -> ffi::GhosttySelection {
    ffi::GhosttySelection {
        size: std::mem::size_of::<ffi::GhosttySelection>(),
        start: empty_ref(),
        end: empty_ref(),
        rectangle: false,
    }
}

impl Terminal {
    /// The grid reference of `point`, valid until the next mutating call.
    pub(crate) fn grid_ref(&self, point: Point) -> Result<ffi::GhosttyGridRef, Error> {
        let mut grid_ref = empty_ref();
        // SAFETY: the terminal is live; `grid_ref` is a sized out-struct the call fills on success.
        Error::check(unsafe { ffi::ghostty_terminal_grid_ref(self.raw_shared(), point.raw(), &raw mut grid_ref) })?;
        Ok(grid_ref)
    }

    /// Where `grid_ref` (taken from this terminal since its last mutation) is in `tag`'s space; `None` when it is
    /// outside that space (scrollback has no active coordinates).
    pub(crate) fn point_of(&self, grid_ref: &ffi::GhosttyGridRef, tag: PointTag) -> Option<Point> {
        let mut coordinate = ffi::GhosttyPointCoordinate { x: 0, y: 0 };
        // SAFETY: the terminal is live and the reference came from it with no mutation since (the caller's borrow);
        // `coordinate` is a valid out-pointer.
        let result = unsafe {
            ffi::ghostty_terminal_point_from_grid_ref(self.raw_shared(), grid_ref, tag.raw(), &raw mut coordinate)
        };
        (result == ffi::GHOSTTY_SUCCESS).then_some(Point::new(tag, coordinate.x, coordinate.y))
    }

    /// The snapshot of the cells `start` to `end`, valid until the next mutating call.
    pub(crate) fn snapshot(&self, start: Point, end: Point, rectangle: bool) -> Result<ffi::GhosttySelection, Error> {
        Ok(ffi::GhosttySelection {
            size: std::mem::size_of::<ffi::GhosttySelection>(),
            start: self.grid_ref(start)?,
            end: self.grid_ref(end)?,
            rectangle,
        })
    }

    /// The installed selection's snapshot, valid until the next mutating call; `None` with none.
    pub(crate) fn selection_snapshot(&self) -> Option<ffi::GhosttySelection> {
        let mut selection = empty_selection();
        // SAFETY: the terminal is live; `DATA_SELECTION` fills a sized `GhosttySelection`, or reports no value.
        let result = unsafe {
            ffi::ghostty_terminal_get(
                self.raw_shared(),
                ffi::GHOSTTY_TERMINAL_DATA_SELECTION,
                (&raw mut selection).cast(),
            )
        };
        (result == ffi::GHOSTTY_SUCCESS).then_some(selection)
    }

    /// The installed selection, its ends in `tag`'s space; `None` with no selection, or when an end is outside the
    /// space (use [`PointTag::Screen`], which holds every cell).
    #[must_use]
    pub fn selection(&self, tag: PointTag) -> Option<Selection> {
        let snapshot = self.selection_snapshot()?;
        Some(Selection {
            start: self.point_of(&snapshot.start, tag)?,
            end: self.point_of(&snapshot.end, tag)?,
            rectangle: snapshot.rectangle,
        })
    }

    /// Select from `start` to `end` (inclusive, either order), as a run of text or a rectangle; replaces the
    /// selection.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a point outside its space.
    pub fn set_selection(&mut self, start: Point, end: Point, rectangle: bool) -> Result<(), Error> {
        let snapshot = self.snapshot(start, end, rectangle)?;
        self.install(Some(&snapshot))
    }

    /// Drop the selection.
    ///
    /// # Errors
    ///
    /// The library's result when it refuses (it does not for a live terminal).
    pub fn clear_selection(&mut self) -> Result<(), Error> {
        self.install(None)
    }

    /// Select the word under `at` (Ghostty's word boundaries); `false`, with the selection unchanged, when there is
    /// no word there.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a point outside its space.
    pub fn select_word(&mut self, at: Point) -> Result<bool, Error> {
        let options = ffi::GhosttyTerminalSelectWordOptions {
            size: std::mem::size_of::<ffi::GhosttyTerminalSelectWordOptions>(),
            ref_: self.grid_ref(at)?,
            boundary_codepoints: std::ptr::null(),
            boundary_codepoints_len: 0,
        };
        self.derive(|terminal, out| {
            // SAFETY: the terminal is live and the reference is fresh (no mutation since); `out` is a valid sized
            // out-struct.
            unsafe { ffi::ghostty_terminal_select_word(terminal, &raw const options, out) }
        })
    }

    /// Select the line under `at`, across soft wraps, trimmed of surrounding whitespace and bounded by prompt marks
    /// (Ghostty's triple click); `false`, with the selection unchanged, for a blank line.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a point outside its space.
    pub fn select_line(&mut self, at: Point) -> Result<bool, Error> {
        let options = ffi::GhosttyTerminalSelectLineOptions {
            size: std::mem::size_of::<ffi::GhosttyTerminalSelectLineOptions>(),
            ref_: self.grid_ref(at)?,
            whitespace: std::ptr::null(),
            whitespace_len: 0,
            semantic_prompt_boundary: true,
        };
        self.derive(|terminal, out| {
            // SAFETY: as in `select_word`.
            unsafe { ffi::ghostty_terminal_select_line(terminal, &raw const options, out) }
        })
    }

    /// Select everything selectable (Ghostty's `select_all`); `false`, with the selection unchanged, on a blank
    /// screen.
    ///
    /// # Errors
    ///
    /// The library's result when it refuses.
    pub fn select_all(&mut self) -> Result<bool, Error> {
        self.derive(|terminal, out| {
            // SAFETY: the terminal is live; `out` is a valid sized out-struct.
            unsafe { ffi::ghostty_terminal_select_all(terminal, out) }
        })
    }

    /// The URI of the OSC 8 hyperlink on the cell at `at`; `None` for a cell without one.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a point outside its space.
    pub fn hyperlink_uri(&self, at: Point) -> Result<Option<String>, Error> {
        let grid_ref = self.grid_ref(at)?;
        let mut len = 0usize;
        // SAFETY: the reference is fresh (`&self` keeps mutations out); a null buffer of zero length asks for the
        // length, written to `len`.
        let probe =
            unsafe { ffi::ghostty_grid_ref_hyperlink_uri(&raw const grid_ref, std::ptr::null_mut(), 0, &raw mut len) };
        match Error::check(probe) {
            Ok(()) if len == 0 => return Ok(None),
            Ok(()) | Err(Error::OutOfSpace) => {}
            Err(Error::NoValue) => return Ok(None),
            Err(err) => return Err(err),
        }
        let mut buffer = vec![0u8; len];
        // SAFETY: as above; `buffer` holds `len` writable bytes.
        Error::check(unsafe {
            ffi::ghostty_grid_ref_hyperlink_uri(&raw const grid_ref, buffer.as_mut_ptr(), buffer.len(), &raw mut len)
        })?;
        buffer.truncate(len);
        Ok((!buffer.is_empty()).then(|| String::from_utf8_lossy(&buffer).into_owned()))
    }

    /// Run a `select_*` derivation and install what it found; `false` when it found nothing.
    fn derive(
        &mut self,
        select: impl FnOnce(ffi::GhosttyTerminal, *mut ffi::GhosttySelection) -> ffi::GhosttyResult,
    ) -> Result<bool, Error> {
        let mut selection = empty_selection();
        match Error::check(select(self.as_raw(), &raw mut selection)) {
            Ok(()) => self.install(Some(&selection)).map(|()| true),
            Err(Error::NoValue) => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// The screen row (0 is the top of the scrollback) the viewport starts at.
    #[must_use]
    pub fn viewport_top(&self) -> u32 {
        self.grid_ref(Point::new(PointTag::Viewport, 0, 0))
            .ok()
            .and_then(|top| self.point_of(&top, PointTag::Screen))
            .map_or(0, |top| top.y)
    }

    /// Move the selection's logical end as Ghostty's `adjust_selection` does (`Selection.adjust`); `false`, nothing
    /// changed, with no selection.
    ///
    /// # Errors
    ///
    /// The library's result when it refuses.
    pub fn adjust_selection(&mut self, adjust: SelectionAdjust) -> Result<bool, Error> {
        let Some(mut selection) = self.selection_snapshot() else {
            return Ok(false);
        };
        // SAFETY: the terminal is live and borrowed mutably; the snapshot came from it with no mutation since and is
        // adjusted in place.
        Error::check(unsafe {
            ffi::ghostty_terminal_selection_adjust(self.as_raw(), &raw mut selection, adjust.raw())
        })?;
        self.install(Some(&selection)).map(|()| true)
    }

    /// Install `selection` (a fresh snapshot of this terminal) as the terminal's own, tracked selection; `None`
    /// clears it.
    fn install(&mut self, selection: Option<&ffi::GhosttySelection>) -> Result<(), Error> {
        let value = selection.map_or(std::ptr::null(), |selection| std::ptr::from_ref(selection).cast());
        // SAFETY: the terminal is live and `&mut self` serializes the call; `OPT_SELECTION` copies the snapshot (taken
        // from this terminal with no mutation since) into tracked state during the call, or clears on null.
        Error::check(unsafe { ffi::ghostty_terminal_set(self.as_raw(), ffi::GHOSTTY_TERMINAL_OPT_SELECTION, value) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: u16, y: u32) -> Point {
        Point::new(PointTag::Screen, x, y)
    }

    #[test]
    fn a_selection_reads_back_in_any_space_that_holds_it() {
        let mut terminal = Terminal::new(10, 3).unwrap();
        assert_eq!(terminal.selection(PointTag::Screen), None);
        terminal.vt_write(b"1\r\n2\r\n3\r\n4\r\n5");
        // two rows scrolled off: screen row 3 is viewport row 1
        terminal.set_selection(screen(4, 3), screen(1, 2), false).unwrap();
        let selection = terminal.selection(PointTag::Screen).unwrap();
        assert_eq!((selection.start, selection.end), (screen(4, 3), screen(1, 2)));
        assert!(!selection.rectangle);
        assert_eq!(
            terminal.selection(PointTag::Viewport).map(|s| (s.start, s.end)),
            Some((
                Point::new(PointTag::Viewport, 4, 1),
                Point::new(PointTag::Viewport, 1, 0)
            ))
        );
        terminal.clear_selection().unwrap();
        assert_eq!(terminal.selection(PointTag::Screen), None);
    }

    #[test]
    fn a_point_outside_its_space_is_refused() {
        let mut terminal = Terminal::new(10, 3).unwrap();
        let outside = Point::new(PointTag::Viewport, 0, 9);
        assert_eq!(
            terminal.set_selection(outside, outside, false),
            Err(Error::InvalidValue)
        );
    }

    #[test]
    fn words_lines_and_everything() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.vt_write(b"  hello world  \r\nnext");
        let viewport = |x, y| Point::new(PointTag::Viewport, x, y);
        assert!(terminal.select_word(viewport(4, 0)).unwrap());
        let word = terminal.selection(PointTag::Viewport).unwrap();
        assert_eq!((word.start.x, word.end.x), (2, 6));
        assert!(terminal.select_line(viewport(9, 0)).unwrap());
        let line = terminal.selection(PointTag::Viewport).unwrap();
        assert_eq!((line.start.x, line.end.x), (2, 12));
        // a blank row has no word and no line: the selection stays
        assert!(!terminal.select_word(viewport(0, 2)).unwrap());
        assert!(!terminal.select_line(viewport(0, 2)).unwrap());
        assert_eq!(terminal.selection(PointTag::Viewport), Some(line));
        assert!(terminal.select_all().unwrap());
        let all = terminal.selection(PointTag::Viewport).unwrap();
        assert_eq!((all.start.y, all.end.y), (0, 1));
    }
}
