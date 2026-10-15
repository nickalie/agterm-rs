//! Reading a terminal's content back as text (`formatter.h`): plain text, VT sequences or HTML, of a region of the
//! active screen or of the selection. The formatter borrows the terminal and reads it live, so it is created, run and
//! freed inside one call that holds the terminal (`&mut`); only owned bytes come out.

use crate::selection::{Point, PointTag};
use crate::{Error, Handle, Terminal, ffi};

/// The output format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Format {
    #[default]
    Plain,
    /// VT sequences keeping colors, styles and links.
    Vt,
    Html,
}

impl Format {
    fn raw(self) -> ffi::GhosttyFormatterFormat {
        match self {
            Self::Plain => ffi::GHOSTTY_FORMATTER_FORMAT_PLAIN,
            Self::Vt => ffi::GHOSTTY_FORMATTER_FORMAT_VT,
            Self::Html => ffi::GHOSTTY_FORMATTER_FORMAT_HTML,
        }
    }
}

/// How the text is laid out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FormatOptions {
    pub format: Format,
    /// Join soft-wrapped rows into one line.
    pub unwrap: bool,
    /// Drop trailing whitespace of non-blank lines.
    pub trim: bool,
}

/// What to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Region {
    /// The visible rows (viewport), top-left to bottom-right.
    Viewport,
    /// The whole active screen: scrollback, then the active area.
    Screen,
    /// The cells from one point to another, inclusive, as a run of text or a rectangle.
    Range { start: Point, end: Point, rectangle: bool },
    /// The installed selection.
    Selection,
}

impl Terminal {
    /// The text of `region`; `None` for [`Region::Selection`] with no selection. A blank region reads as empty.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a range point outside its space, or the library's result when formatting fails.
    pub fn format(&mut self, region: Region, options: FormatOptions) -> Result<Option<Vec<u8>>, Error> {
        let Some(selection) = self.region(region)? else {
            return Ok(None);
        };
        let raw_options = ffi::GhosttyFormatterTerminalOptions {
            size: std::mem::size_of::<ffi::GhosttyFormatterTerminalOptions>(),
            emit: options.format.raw(),
            unwrap: options.unwrap,
            trim: options.trim,
            extra: no_extras(),
            selection: &raw const selection,
        };
        let mut formatter: ffi::GhosttyFormatter = std::ptr::null_mut();
        // SAFETY: the terminal is live and borrowed mutably for the whole call, which frees the formatter (its
        // borrower) before returning; the selection is a fresh snapshot of this terminal; `formatter` is a valid
        // out-pointer.
        Error::check(unsafe {
            ffi::ghostty_formatter_terminal_new(std::ptr::null(), &raw mut formatter, self.as_raw(), raw_options)
        })?;
        let formatter = Handle::new(formatter, ffi::ghostty_formatter_free)?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len = 0;
        // SAFETY: the formatter is live and its terminal unchanged; the out-pointers are valid.
        Error::check(unsafe {
            ffi::ghostty_formatter_format_alloc(formatter.ptr(), std::ptr::null(), &raw mut ptr, &raw mut len)
        })?;
        // SAFETY: on success `ptr` holds `len` bytes allocated by the default allocator (or is null for none).
        let bytes = unsafe { crate::raw_slice(ptr, len) }.to_vec();
        // SAFETY: frees exactly what `format_alloc` returned, with the same (default) allocator and length; null is
        // allowed.
        unsafe { ffi::ghostty_free(std::ptr::null(), ptr, len) };
        Ok(Some(bytes))
    }

    /// The snapshot `region` covers, valid until the next mutating call; `None` for a missing selection.
    fn region(&self, region: Region) -> Result<Option<ffi::GhosttySelection>, Error> {
        let last_col = self.cols().saturating_sub(1);
        let snapshot = match region {
            Region::Viewport => {
                let last_row = u32::from(self.rows().saturating_sub(1));
                self.snapshot(
                    Point::new(PointTag::Viewport, 0, 0),
                    Point::new(PointTag::Viewport, last_col, last_row),
                    false,
                )?
            }
            Region::Screen => {
                let last_row = u32::try_from(self.total_rows().saturating_sub(1)).unwrap_or(u32::MAX);
                self.snapshot(
                    Point::new(PointTag::Screen, 0, 0),
                    Point::new(PointTag::Screen, last_col, last_row),
                    false,
                )?
            }
            Region::Range { start, end, rectangle } => self.snapshot(start, end, rectangle)?,
            Region::Selection => return Ok(self.selection_snapshot()),
        };
        Ok(Some(snapshot))
    }
}

fn no_extras() -> ffi::GhosttyFormatterTerminalExtra {
    ffi::GhosttyFormatterTerminalExtra {
        size: std::mem::size_of::<ffi::GhosttyFormatterTerminalExtra>(),
        palette: false,
        modes: false,
        scrolling_region: false,
        tabstops: false,
        pwd: false,
        keyboard: false,
        screen: ffi::GhosttyFormatterScreenExtra {
            size: std::mem::size_of::<ffi::GhosttyFormatterScreenExtra>(),
            cursor: false,
            style: false,
            hyperlink: false,
            protection: false,
            kitty_keyboard: false,
            charsets: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: FormatOptions = FormatOptions {
        format: Format::Plain,
        unwrap: true,
        trim: false,
    };

    fn text(terminal: &mut Terminal, region: Region, options: FormatOptions) -> Option<String> {
        terminal
            .format(region, options)
            .unwrap()
            .map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn the_viewport_and_the_screen_read_back() {
        let mut terminal = Terminal::new(8, 2).unwrap();
        assert_eq!(text(&mut terminal, Region::Viewport, PLAIN).as_deref(), Some(""));
        terminal.vt_write(b"one\r\ntwo\r\nthree");
        assert_eq!(
            text(&mut terminal, Region::Viewport, PLAIN).as_deref(),
            Some("two\nthree")
        );
        assert_eq!(
            text(&mut terminal, Region::Screen, PLAIN).as_deref(),
            Some("one\ntwo\nthree")
        );
    }

    #[test]
    fn wrapped_rows_unwrap_and_wide_characters_keep_their_width() {
        let mut terminal = Terminal::new(4, 3).unwrap();
        terminal.vt_write("abcdef\r\n日本".as_bytes());
        assert_eq!(
            text(&mut terminal, Region::Viewport, PLAIN).as_deref(),
            Some("abcdef\n日本")
        );
        let wrapped = FormatOptions { unwrap: false, ..PLAIN };
        assert_eq!(
            text(&mut terminal, Region::Viewport, wrapped).as_deref(),
            Some("abcd\nef\n日本")
        );
    }

    #[test]
    fn a_range_and_the_selection_read_back() {
        let mut terminal = Terminal::new(10, 2).unwrap();
        terminal.vt_write(b"hello  \r\nworld");
        assert_eq!(text(&mut terminal, Region::Selection, PLAIN), None);
        let start = Point::new(PointTag::Viewport, 1, 0);
        let end = Point::new(PointTag::Viewport, 2, 1);
        let range = Region::Range {
            start,
            end,
            rectangle: false,
        };
        assert_eq!(text(&mut terminal, range, PLAIN).as_deref(), Some("ello  \nwor"));
        let trimmed = FormatOptions { trim: true, ..PLAIN };
        assert_eq!(text(&mut terminal, range, trimmed).as_deref(), Some("ello\nwor"));
        terminal.set_selection(start, end, true).unwrap();
        assert_eq!(text(&mut terminal, Region::Selection, PLAIN).as_deref(), Some("el\nor"));
        let outside = Region::Range {
            start,
            end: Point::new(PointTag::Viewport, 0, 7),
            rectangle: false,
        };
        assert_eq!(terminal.format(outside, PLAIN), Err(Error::InvalidValue));
    }

    #[test]
    fn styled_output_keeps_escapes() {
        let mut terminal = Terminal::new(10, 1).unwrap();
        terminal.vt_write(b"\x1b[1mbold");
        let vt = FormatOptions {
            format: Format::Vt,
            ..PLAIN
        };
        let styled = text(&mut terminal, Region::Viewport, vt).unwrap();
        assert!(styled.contains("\x1b[") && styled.contains("bold"), "{styled:?}");
        let html = FormatOptions {
            format: Format::Html,
            ..PLAIN
        };
        assert!(text(&mut terminal, Region::Viewport, html).unwrap().contains("bold"));
    }
}
