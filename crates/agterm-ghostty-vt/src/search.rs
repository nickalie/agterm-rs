//! Scrollback search (`search.h`): a search bound to one terminal, matching a needle (byte-exact, ASCII letters
//! case-insensitive) over the active area and scrollback of both screens.
//!
//! Work is split the way Ghostty runs it on its search thread: the calls that read the terminal ([`Search::new`],
//! [`Search::feed`], [`Search::run`], [`Search::set_needle`], [`Search::select`], [`Search::close`]) take the
//! terminal `&mut`, so they are serialized with everything else on it; [`Search::tick`] and the counters touch only
//! the search's own memory and may run while another thread writes the terminal.
//!
//! The matches the library hands out are snapshots that die on the terminal's next mutation, so they are only read
//! inside the call that caught the search up ([`Search::feed`], [`Search::run`], [`Search::select`]), converted to
//! points before the terminal is let go ([`SearchResults`]).

use crate::selection::{Point, PointTag};
use crate::{Error, Terminal, ffi};

/// Where a search is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchStatus {
    /// [`Search::tick`] can make progress.
    Running,
    /// Blocked until [`Search::feed`] (also right after a needle is set).
    FeedRequired,
    /// Caught up with the terminal as of the last feed (or no needle).
    Complete,
}

/// Which way [`Search::select`] moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchDirection {
    /// Toward older content: up from the bottom of the screen into the scrollback (Ghostty's `navigate_search:next`).
    Older,
    /// Toward newer content (`navigate_search:previous`).
    Newer,
}

/// A match's first and last cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Match {
    pub start: Point,
    pub end: Point,
}

/// What a search found, read while it was caught up with the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResults {
    pub status: SearchStatus,
    /// The matches found so far on the active screen.
    pub total: usize,
    /// The selected match's index, newest first (0 is the match nearest the bottom of the screen).
    pub selected: Option<usize>,
    /// The matches on the viewport, in viewport coordinates, a match running off the top or the bottom clipped to it.
    pub viewport_matches: Vec<Match>,
    /// The selected match in viewport coordinates, when it is on the viewport.
    pub selected_match: Option<Match>,
}

/// A search over one terminal: an owned handle, `Send` and not `Sync` (crate docs).
///
/// Free it with [`Search::close`], which releases the state it tracks inside the terminal. Dropping it unclosed
/// leaks the handle rather than touch a terminal another thread may be writing (freeing the terminal later still
/// detaches it).
#[derive(Debug)]
pub struct Search {
    raw: ffi::GhosttySearch,
    /// The terminal it is bound to: the calls that take a terminal refuse any other.
    terminal: ffi::GhosttyTerminal,
}

// SAFETY: the search is a plain library allocation with no thread affinity; `&mut self` serializes the calls on it
// and every call touching the terminal also takes the terminal `&mut` (crate docs).
unsafe impl Send for Search {}

impl Search {
    /// A search bound to `terminal`, idle (no needle).
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] when the allocation fails.
    pub fn new(terminal: &mut Terminal) -> Result<Self, Error> {
        let mut raw: ffi::GhosttySearch = std::ptr::null_mut();
        let bound = terminal.as_raw();
        // SAFETY: the terminal is live and borrowed mutably (the call registers the search with it); `raw` is a valid
        // out-pointer.
        Error::check(unsafe { ffi::ghostty_search_new(std::ptr::null(), &raw mut raw, bound) })?;
        if raw.is_null() {
            return Err(Error::OutOfMemory);
        }
        Ok(Self { raw, terminal: bound })
    }

    /// Free the search and release what it tracks in `terminal`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for another terminal than the one it is bound to (the search is leaked then).
    pub fn close(self, terminal: &mut Terminal) -> Result<(), Error> {
        self.bound(terminal)?;
        // SAFETY: the search is live and its terminal is borrowed mutably, so the release is serialized with it;
        // `self` is consumed, so the handle is never used again.
        unsafe { ffi::ghostty_search_free(self.raw) };
        Ok(())
    }

    /// Set the needle: a new one restarts the search, the same one (compared as matching compares) keeps the results,
    /// an empty one returns the search to idle.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for another terminal, or the library's result.
    pub fn set_needle(&mut self, terminal: &mut Terminal, needle: &str) -> Result<(), Error> {
        self.bound(terminal)?;
        let value = ffi::GhosttyString {
            ptr: needle.as_ptr(),
            len: needle.len(),
        };
        // SAFETY: the search is live and its terminal borrowed mutably; `OPT_NEEDLE` copies the string during the call.
        Error::check(unsafe {
            ffi::ghostty_search_set(self.raw, ffi::GHOSTTY_SEARCH_OPT_NEEDLE, (&raw const value).cast())
        })
    }

    /// Catch up with the terminal (a bounded amount of work; the only way the search learns of changes) and read what
    /// it has found.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for another terminal, or the library's result.
    pub fn feed(&mut self, terminal: &mut Terminal) -> Result<SearchResults, Error> {
        self.bound(terminal)?;
        // SAFETY: the search is live and its terminal borrowed mutably.
        Error::check(unsafe { ffi::ghostty_search_feed(self.raw) })?;
        self.results(terminal)
    }

    /// Feed and tick until complete (blocking; for one-shot searches), and read what it found.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for another terminal, or the library's result.
    pub fn run(&mut self, terminal: &mut Terminal) -> Result<SearchResults, Error> {
        self.bound(terminal)?;
        // SAFETY: the search is live and its terminal borrowed mutably for the whole call.
        Error::check(unsafe { ffi::ghostty_search_run(self.raw) })?;
        self.results(terminal)
    }

    /// A bounded step on data already copied; never reads the terminal.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] once the terminal was freed.
    pub fn tick(&mut self) -> Result<SearchStatus, Error> {
        let mut status: ffi::GhosttySearchStatus = ffi::GHOSTTY_SEARCH_STATUS_COMPLETE;
        // SAFETY: the search is live and `&mut self` serializes calls on it; `status` is a valid out-pointer.
        Error::check(unsafe { ffi::ghostty_search_tick(self.raw, &raw mut status) })?;
        Ok(status_from_raw(status))
    }

    /// Select the next match in `direction` (wrapping), scrolling the viewport to it when it is not visible, and read
    /// what the search has found; `None` when there is no match to select.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for another terminal, or the library's result.
    pub fn select(
        &mut self,
        terminal: &mut Terminal,
        direction: SearchDirection,
    ) -> Result<Option<SearchResults>, Error> {
        self.bound(terminal)?;
        let option = match direction {
            SearchDirection::Older => ffi::GHOSTTY_SEARCH_OPT_SELECT_NEXT,
            SearchDirection::Newer => ffi::GHOSTTY_SEARCH_OPT_SELECT_PREV,
        };
        // SAFETY: the search is live and its terminal borrowed mutably (selecting catches up with it and may scroll
        // it); the select options take a null value.
        match Error::check(unsafe { ffi::ghostty_search_set(self.raw, option, std::ptr::null()) }) {
            Ok(()) => self.results(terminal).map(Some),
            Err(Error::NoValue) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// The status as of the last tick or feed.
    #[must_use]
    pub fn status(&self) -> SearchStatus {
        let mut status: ffi::GhosttySearchStatus = ffi::GHOSTTY_SEARCH_STATUS_COMPLETE;
        self.get(ffi::GHOSTTY_SEARCH_DATA_STATUS, (&raw mut status).cast());
        status_from_raw(status)
    }

    /// The matches found so far on the active screen.
    #[must_use]
    pub fn total(&self) -> usize {
        let mut total: usize = 0;
        self.get(ffi::GHOSTTY_SEARCH_DATA_TOTAL_MATCHES, (&raw mut total).cast());
        total
    }

    /// The selected match's index, newest first; `None` with none selected.
    #[must_use]
    pub fn selected_index(&self) -> Option<usize> {
        let mut index: usize = 0;
        (self.get(ffi::GHOSTTY_SEARCH_DATA_SELECTED_INDEX, (&raw mut index).cast()) == ffi::GHOSTTY_SUCCESS)
            .then_some(index)
    }

    /// Everything the search found, its match snapshots converted while they are valid: right after a feed, run or
    /// select, under the same `&mut` borrow of the terminal.
    fn results(&self, terminal: &mut Terminal) -> Result<SearchResults, Error> {
        let mut selection = crate::selection::empty_selection();
        let selected_match = (self.get(ffi::GHOSTTY_SEARCH_DATA_SELECTED_MATCH, (&raw mut selection).cast())
            == ffi::GHOSTTY_SUCCESS)
            .then(|| clip(terminal, &selection))
            .flatten();
        Ok(SearchResults {
            status: self.status(),
            total: self.total(),
            selected: self.selected_index(),
            viewport_matches: self.viewport_matches(terminal)?,
            selected_match,
        })
    }

    fn viewport_matches(&self, terminal: &Terminal) -> Result<Vec<Match>, Error> {
        let mut buffer = ffi::GhosttySelectionBuffer {
            ptr: std::ptr::null_mut(),
            cap: 0,
            len: 0,
        };
        let mut selections = Vec::new();
        loop {
            let result = self.get(ffi::GHOSTTY_SEARCH_DATA_VIEWPORT_MATCHES, (&raw mut buffer).cast());
            match Error::check(result) {
                Ok(()) => break,
                Err(Error::OutOfSpace) => {
                    selections = vec![crate::selection::empty_selection(); buffer.len];
                    buffer = ffi::GhosttySelectionBuffer {
                        ptr: selections.as_mut_ptr(),
                        cap: selections.len(),
                        len: 0,
                    };
                }
                Err(err) => return Err(err),
            }
        }
        selections.truncate(buffer.len);
        Ok(selections
            .iter()
            .filter_map(|selection| clip(terminal, selection))
            .collect())
    }

    fn get(&self, data: ffi::GhosttySearchData, value: *mut std::ffi::c_void) -> ffi::GhosttyResult {
        // SAFETY: the search is live and `&self` (with `!Sync`) keeps calls on it serialized; the callers pass the
        // output type each key documents. Reads touch only the search's memory.
        unsafe { ffi::ghostty_search_get(self.raw, data, value) }
    }

    fn bound(&self, terminal: &Terminal) -> Result<(), Error> {
        if terminal.raw_shared() == self.terminal {
            Ok(())
        } else {
            Err(Error::InvalidValue)
        }
    }
}

fn status_from_raw(raw: ffi::GhosttySearchStatus) -> SearchStatus {
    match raw {
        ffi::GHOSTTY_SEARCH_STATUS_RUNNING => SearchStatus::Running,
        ffi::GHOSTTY_SEARCH_STATUS_FEED_REQUIRED => SearchStatus::FeedRequired,
        _ => SearchStatus::Complete,
    }
}

/// A match snapshot (valid: the caller read it under the borrow that caught the search up) in viewport coordinates;
/// an end outside the viewport is clamped to its edge, a match wholly outside is `None`.
///
/// The conversion fails only above the viewport: a cell below it (on a page the viewport shares, which the viewport
/// match list may include, `search.h`) converts to a row past the last, so the bottom is bounded here.
fn clip(terminal: &Terminal, selection: &ffi::GhosttySelection) -> Option<Match> {
    let tag = PointTag::Viewport;
    let last = Point::new(
        tag,
        terminal.cols().saturating_sub(1),
        u32::from(terminal.rows().saturating_sub(1)),
    );
    let start = terminal.point_of(&selection.start, tag);
    let end = terminal.point_of(&selection.end, tag);
    clip_points(start, end, last)
}

/// Where a match end lies relative to the viewport.
enum Place {
    Above,
    In(Point),
    Below,
}

/// [`clip`] on converted ends (`None`: the conversion failed, above the viewport); `last` is the viewport's
/// bottom-right cell.
fn clip_points(start: Option<Point>, end: Option<Point>, last: Point) -> Option<Match> {
    let place = |point: Option<Point>| match point {
        None => Place::Above,
        Some(point) if point.y > last.y => Place::Below,
        Some(point) => Place::In(point),
    };
    let first = Point::new(last.tag, 0, 0);
    let (start, end) = match (place(start), place(end)) {
        (Place::In(start), Place::In(end)) => (start, end),
        (Place::Above, Place::In(end)) => (first, end),
        (Place::In(start), Place::Below) => (start, last),
        (Place::Above, Place::Below) => (first, last),
        _ => return None,
    };
    Some(Match { start, end })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn searched(terminal: &mut Terminal, needle: &str) -> (Search, SearchResults) {
        let mut search = Search::new(terminal).unwrap();
        search.set_needle(terminal, needle).unwrap();
        let results = search.run(terminal).unwrap();
        (search, results)
    }

    #[test]
    fn counts_matches_across_the_scrollback_case_insensitively() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        for line in 0..10 {
            terminal.vt_write(format!("line {line} Needle\r\n").as_bytes());
        }
        let (mut search, results) = searched(&mut terminal, "needle");
        assert_eq!(results.status, SearchStatus::Complete);
        assert_eq!((results.total, results.selected), (10, None));
        // the viewport shows the last two lines with a match, and the cursor's empty row
        assert_eq!(results.viewport_matches.len(), 2);
        search.set_needle(&mut terminal, "line 9").unwrap();
        assert_eq!(search.run(&mut terminal).unwrap().total, 1);
        search.close(&mut terminal).unwrap();
    }

    #[test]
    fn selecting_walks_from_the_newest_and_wraps() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.vt_write(b"a1\r\na2\r\na3");
        let (mut search, results) = searched(&mut terminal, "a");
        assert_eq!(results.total, 3);
        let mut select = |direction| search.select(&mut terminal, direction).unwrap().unwrap();
        let newest = select(SearchDirection::Older);
        assert_eq!(newest.selected, Some(0));
        let found = newest.selected_match.unwrap();
        assert_eq!((found.start.y, found.start.x, found.end.x), (2, 0, 0));
        assert_eq!(select(SearchDirection::Older).selected, Some(1));
        assert_eq!(select(SearchDirection::Newer).selected, Some(0));
        let wrapped = select(SearchDirection::Newer);
        assert_eq!(wrapped.selected, Some(2), "wraps past the newest");
        assert_eq!(wrapped.viewport_matches.len(), 3);
        search.close(&mut terminal).unwrap();
    }

    #[test]
    fn no_match_selects_nothing_and_another_terminal_is_refused() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.vt_write(b"nothing here");
        let (mut search, results) = searched(&mut terminal, "zzz");
        assert_eq!(results.total, 0);
        assert_eq!(search.select(&mut terminal, SearchDirection::Older), Ok(None));
        let mut other = Terminal::new(20, 3).unwrap();
        assert_eq!(search.feed(&mut other).err(), Some(Error::InvalidValue));
        assert_eq!(search.close(&mut other), Err(Error::InvalidValue));
    }

    #[test]
    fn matches_below_a_scrolled_back_viewport_are_dropped_or_clamped() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        for line in 0..10 {
            terminal.vt_write(format!("x{line}\r\n").as_bytes());
        }
        terminal.scroll_viewport(crate::Scroll::Rows(-4));
        let (search, results) = searched(&mut terminal, "x");
        assert_eq!(results.total, 10);
        assert!(!results.viewport_matches.is_empty());
        for found in &results.viewport_matches {
            assert!(found.start.y < 3 && found.end.y < 3, "{found:?} past the viewport");
        }
        search.close(&mut terminal).unwrap();

        let tag = PointTag::Viewport;
        let last = Point::new(tag, 19, 2);
        let at = |x, y| Some(Point::new(tag, x, y));
        assert_eq!(clip_points(at(1, 3), at(4, 3), last), None, "wholly below");
        assert_eq!(
            clip_points(at(5, 2), at(2, 3), last),
            Some(Match {
                start: Point::new(tag, 5, 2),
                end: last
            }),
            "running off the bottom"
        );
        assert_eq!(
            clip_points(None, at(2, 0), last),
            Some(Match {
                start: Point::new(tag, 0, 0),
                end: Point::new(tag, 2, 0)
            }),
            "running off the top"
        );
        assert_eq!(
            clip_points(None, at(2, 4), last),
            Some(Match {
                start: Point::new(tag, 0, 0),
                end: last
            }),
            "across the whole viewport"
        );
        assert_eq!(clip_points(None, None, last), None, "wholly above");
    }

    #[test]
    fn ticks_progress_off_the_terminal() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.vt_write(b"x\r\nx");
        let mut search = Search::new(&mut terminal).unwrap();
        search.set_needle(&mut terminal, "x").unwrap();
        assert_eq!(search.status(), SearchStatus::FeedRequired);
        let mut results = None;
        for _ in 0..100 {
            let fed = search.feed(&mut terminal).unwrap();
            if fed.status == SearchStatus::Complete {
                results = Some(fed);
                break;
            }
            while search.tick().unwrap() == SearchStatus::Running {}
        }
        assert_eq!(results.map(|results| results.total), Some(2));
        search.set_needle(&mut terminal, "").unwrap();
        assert_eq!(search.total(), 0);
        search.close(&mut terminal).unwrap();
    }
}
