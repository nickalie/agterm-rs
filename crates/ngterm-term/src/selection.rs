//! The pane's selection: cell ranges, the word or line under a point, everything; read back as text.
//!
//! The terminal owns the selection and keeps it on the same cells while output scrolls, the screen reflows or
//! scrollback is pruned. Every call here runs under the VT lock; a change marks the capture so the next frame carries
//! the new selected ranges ([`crate::Row::selection`]).

use ngterm_ghostty_vt as vt;

use crate::PaneError;
use crate::pane::{Pane, Vt, lock};

/// The coordinate space of a [`GridPoint`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PointSpace {
    /// The visible rows, scrolled back or not (row 0 is the top of the pane).
    #[default]
    Viewport,
    /// The whole screen: row 0 is the oldest scrollback row.
    Screen,
}

/// A cell: column `x`, row `y`, zero-based, in `space`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct GridPoint {
    pub space: PointSpace,
    pub x: u16,
    pub y: u32,
}

impl GridPoint {
    #[must_use]
    pub const fn viewport(x: u16, y: u32) -> Self {
        Self {
            space: PointSpace::Viewport,
            x,
            y,
        }
    }

    #[must_use]
    pub const fn screen(x: u16, y: u32) -> Self {
        Self {
            space: PointSpace::Screen,
            x,
            y,
        }
    }

    pub(crate) fn engine(self) -> vt::Point {
        let tag = match self.space {
            PointSpace::Viewport => vt::PointTag::Viewport,
            PointSpace::Screen => vt::PointTag::Screen,
        };
        vt::Point::new(tag, self.x, self.y)
    }

    pub(crate) fn from_engine(point: vt::Point) -> Self {
        let space = match point.tag {
            vt::PointTag::Screen => PointSpace::Screen,
            _ => PointSpace::Viewport,
        };
        Self {
            space,
            x: point.x,
            y: point.y,
        }
    }
}

/// The selection: its ends in screen coordinates (inclusive, in the order made, so `start` may follow `end`), as a
/// run of text or the corners of a rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SelectionRange {
    pub start: GridPoint,
    pub end: GridPoint,
    pub rectangle: bool,
}

/// How [`Pane::selection_text`] lays the text out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SelectionText {
    /// As libghostty's `read_selection` returned it (the control API's read): soft wraps joined, trailing whitespace
    /// kept.
    #[default]
    Raw,
    /// As Ghostty copies it to the clipboard: trailing whitespace of each line trimmed too.
    Copy,
}

impl Pane {
    /// Select from `start` to `end` (inclusive, either order), as a run of text or a rectangle.
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] for a point outside its space.
    pub fn select(&self, start: GridPoint, end: GridPoint, rectangle: bool) -> Result<(), PaneError> {
        self.change_selection(|terminal| {
            terminal
                .set_selection(start.engine(), end.engine(), rectangle)
                .map(|()| true)
        })
        .map(|_| ())
    }

    /// Select the word under `at` (Ghostty's double click); `false`, the selection unchanged, where there is none.
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] for a point outside its space.
    pub fn select_word(&self, at: GridPoint) -> Result<bool, PaneError> {
        let boundaries = lock(&self.shared().vt).word_boundaries.clone();
        self.change_selection(|terminal| terminal.select_word(at.engine(), boundaries.as_deref()))
    }

    /// The word boundaries of word selections (Ghostty's `selection-word-chars`: the characters, U+0000 always among
    /// them); `None` restores Ghostty's default set.
    pub fn set_word_boundaries(&self, boundaries: Option<&[char]>) {
        lock(&self.shared().vt).word_boundaries = boundaries.map(|chars| {
            std::iter::once(0)
                .chain(chars.iter().map(|ch| u32::from(*ch)))
                .collect()
        });
    }

    /// Select the line under `at`, across soft wraps, without surrounding blanks (Ghostty's triple click); `false`,
    /// the selection unchanged, on a blank line.
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] for a point outside its space.
    pub fn select_line(&self, at: GridPoint) -> Result<bool, PaneError> {
        self.change_selection(|terminal| terminal.select_line(at.engine()))
    }

    /// Select everything (Ghostty's `select_all`); `false` on a blank screen.
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when the library refuses.
    pub fn select_all(&self) -> Result<bool, PaneError> {
        self.change_selection(vt::Terminal::select_all)
    }

    /// Drop the selection.
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when the library refuses.
    pub fn clear_selection(&self) -> Result<(), PaneError> {
        self.change_selection(|terminal| terminal.clear_selection().map(|()| true))
            .map(|_| ())
    }

    /// The selection, in screen coordinates; `None` with none.
    #[must_use]
    pub fn selection(&self) -> Option<SelectionRange> {
        let selection = lock(&self.shared().vt).terminal.selection(vt::PointTag::Screen)?;
        Some(SelectionRange {
            start: GridPoint::from_engine(selection.start),
            end: GridPoint::from_engine(selection.end),
            rectangle: selection.rectangle,
        })
    }

    /// The selection's text, formatted and copied under the VT lock; `None` with no or an empty selection.
    #[must_use]
    pub fn selection_text(&self, layout: SelectionText) -> Option<String> {
        let options = vt::FormatOptions {
            format: vt::Format::Plain,
            unwrap: true,
            trim: layout == SelectionText::Copy,
            extras: vt::FormatExtras::empty(),
        };
        let bytes = lock(&self.shared().vt)
            .terminal
            .format(vt::Region::Selection, options)
            .map_err(|err| tracing::warn!(%err, "reading the selection failed"))
            .ok()??;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        (!text.is_empty()).then_some(text)
    }

    /// Clear the selection with the VT lock already held, marking the capture and `vt` (the caller wakes the frontend
    /// once the lock is let go, see [`Vt::take_cleared_selection`]).
    pub(crate) fn clear_selection_held(&self, vt: &mut Vt) {
        if vt.terminal.selection(vt::PointTag::Screen).is_none() {
            return;
        }
        if let Err(err) = vt.terminal.clear_selection() {
            tracing::warn!(%err, "clearing the selection failed");
        }
        vt.cleared_selection = true;
        // VT → capture
        self.shared().touch_capture();
    }

    /// Run a selection change under the VT lock and mark the capture for the next frame.
    fn change_selection(
        &self,
        change: impl FnOnce(&mut vt::Terminal) -> Result<bool, vt::Error>,
    ) -> Result<bool, PaneError> {
        let shared = self.shared();
        let mut vt = lock(&shared.vt);
        let changed = change(&mut vt.terminal).map_err(PaneError::terminal)?;
        if changed {
            // VT → capture, the pane's lock order
            shared.touch_capture();
        }
        drop(vt);
        if changed {
            shared.wake();
        }
        Ok(changed)
    }
}
