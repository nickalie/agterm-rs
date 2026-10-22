//! Reading a pane's text back (the control API's `read_text`, `read_selection` and cursor column).
//!
//! The text is what libghostty's `ghostty_surface_read_text` returned for the GTK build (`Surface.dumpTextLocked`):
//! plain text of the region from its top-left to its bottom-right cell, soft-wrapped rows joined, trailing whitespace
//! kept, the formatter created and run under the VT lock so only owned bytes leave it.

use ngterm_ghostty_vt as vt;

use crate::PaneError;
use crate::pane::{Pane, lock};

/// Which rows [`Pane::read_text`] reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TextRegion {
    /// The visible rows (the viewport).
    #[default]
    Viewport,
    /// The scrollback and the screen.
    All,
}

/// The layout of a read: plain text, wraps joined, nothing trimmed (`selectionString(.trim = false)`).
const READ: vt::FormatOptions = vt::FormatOptions {
    format: vt::Format::Plain,
    unwrap: true,
    trim: false,
    extras: vt::FormatExtras::empty(),
};

impl Pane {
    /// The text of `region`; an empty string for a blank region.
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when the library fails the read.
    pub fn read_text(&self, region: TextRegion) -> Result<String, PaneError> {
        let region = match region {
            TextRegion::Viewport => vt::Region::Viewport,
            TextRegion::All => vt::Region::Screen,
        };
        let bytes = lock(&self.shared().vt)
            .terminal
            .format(region, READ)
            .map_err(PaneError::terminal)?
            .unwrap_or_default();
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The cursor's zero-based column in the active area.
    #[must_use]
    pub fn cursor_column(&self) -> u16 {
        lock(&self.shared().vt).terminal.cursor().0
    }
}
