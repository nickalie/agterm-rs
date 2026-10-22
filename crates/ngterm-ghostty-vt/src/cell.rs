//! The cells of one captured row: [`Cells`] walks them, [`CellRef`] reads one (text, width, style, colors).

use std::marker::PhantomData;
use std::ptr::NonNull;

use bitflags::bitflags;

use crate::{Error, Rgb, ffi};

/// How much of a wide character a cell holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum CellWidth {
    /// One column.
    #[default]
    Narrow,
    /// The first of a two-column character.
    Wide,
    /// The second column of a wide character (no text of its own).
    SpacerTail,
    /// A wide character that did not fit at the end of a row leaves this in the last column and wraps.
    SpacerHead,
}

bitflags! {
    /// A cell's boolean text attributes (SGR).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub struct StyleFlags: u8 {
        const BOLD = 1;
        const ITALIC = 1 << 1;
        const FAINT = 1 << 2;
        const BLINK = 1 << 3;
        const INVERSE = 1 << 4;
        const INVISIBLE = 1 << 5;
        const STRIKETHROUGH = 1 << 6;
        const OVERLINE = 1 << 7;
    }
}

/// A style color as the program set it: unset, a palette index, or direct RGB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum StyleColor {
    #[default]
    None,
    Palette(u8),
    Rgb(Rgb),
}

/// The underline style (SGR 4 and its subparameters).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// A cell's style.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub foreground: StyleColor,
    pub background: StyleColor,
    pub underline_color: StyleColor,
    pub flags: StyleFlags,
    pub underline: Underline,
}

// `GhosttySgrUnderline` (`sgr.h`) is not among the bound type families; its values, mirrored.
const SGR_UNDERLINE_SINGLE: i32 = 1;
const SGR_UNDERLINE_DOUBLE: i32 = 2;
const SGR_UNDERLINE_CURLY: i32 = 3;
const SGR_UNDERLINE_DOTTED: i32 = 4;
const SGR_UNDERLINE_DASHED: i32 = 5;

impl Style {
    fn from_raw(raw: &ffi::GhosttyStyle) -> Self {
        let mut flags = StyleFlags::empty();
        for (on, flag) in [
            (raw.bold, StyleFlags::BOLD),
            (raw.italic, StyleFlags::ITALIC),
            (raw.faint, StyleFlags::FAINT),
            (raw.blink, StyleFlags::BLINK),
            (raw.inverse, StyleFlags::INVERSE),
            (raw.invisible, StyleFlags::INVISIBLE),
            (raw.strikethrough, StyleFlags::STRIKETHROUGH),
            (raw.overline, StyleFlags::OVERLINE),
        ] {
            flags.set(flag, on);
        }
        let underline = match raw.underline {
            SGR_UNDERLINE_SINGLE => Underline::Single,
            SGR_UNDERLINE_DOUBLE => Underline::Double,
            SGR_UNDERLINE_CURLY => Underline::Curly,
            SGR_UNDERLINE_DOTTED => Underline::Dotted,
            SGR_UNDERLINE_DASHED => Underline::Dashed,
            _ => Underline::None,
        };
        Self {
            foreground: style_color(&raw.fg_color),
            background: style_color(&raw.bg_color),
            underline_color: style_color(&raw.underline_color),
            flags,
            underline,
        }
    }
}

fn style_color(color: &ffi::GhosttyStyleColor) -> StyleColor {
    match color.tag {
        // SAFETY: the tag says the union holds a palette index.
        ffi::GHOSTTY_STYLE_COLOR_PALETTE => StyleColor::Palette(unsafe { color.value.palette }),
        // SAFETY: the tag says the union holds an RGB value.
        ffi::GHOSTTY_STYLE_COLOR_RGB => StyleColor::Rgb(unsafe { color.value.rgb }.into()),
        _ => StyleColor::None,
    }
}

/// The cells of one row, walked left to right with [`Cells::next_cell`] (each cell borrows the walk).
#[derive(Debug)]
pub struct Cells<'a> {
    cells: NonNull<ffi::GhosttyRenderStateRowCellsImpl>,
    row: PhantomData<&'a ()>,
}

impl Cells<'_> {
    /// # Safety
    ///
    /// `cells` must have just been filled for a row of a render state that stays borrowed for `'a`.
    pub(crate) unsafe fn new(cells: NonNull<ffi::GhosttyRenderStateRowCellsImpl>) -> Self {
        Self {
            cells,
            row: PhantomData,
        }
    }

    /// The next cell, or `None` past the last column.
    ///
    /// # Errors
    ///
    /// The library's result when the cell cannot be read.
    pub fn next_cell(&mut self) -> Option<Result<CellRef<'_>, Error>> {
        // SAFETY: the cells handle was filled for this row and the state stays borrowed (constructor contract).
        if !unsafe { ffi::ghostty_render_state_row_cells_next(self.cells.as_ptr()) } {
            return None;
        }
        let mut raw: ffi::GhosttyCell = 0;
        // SAFETY: positioned on a cell; `ROW_CELLS_DATA_RAW` writes a `GhosttyCell`.
        let result = unsafe { self.get(ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW, (&raw mut raw).cast()) };
        Some(Error::check(result).map(|()| CellRef {
            cells: self.cells,
            raw,
            walk: PhantomData,
        }))
    }

    /// # Safety
    ///
    /// The handle must be positioned on a cell and `out` valid for the key's output type.
    unsafe fn get(&self, data: ffi::GhosttyRenderStateRowCellsData, out: *mut std::ffi::c_void) -> ffi::GhosttyResult {
        // SAFETY: the caller's contract.
        unsafe { ffi::ghostty_render_state_row_cells_get(self.cells.as_ptr(), data, out) }
    }
}

/// One cell of a captured row.
#[derive(Debug)]
pub struct CellRef<'a> {
    cells: NonNull<ffi::GhosttyRenderStateRowCellsImpl>,
    raw: ffi::GhosttyCell,
    walk: PhantomData<&'a mut ()>,
}

impl CellRef<'_> {
    /// # Safety
    ///
    /// `out` must be valid for the key's output type (the handle is positioned on this cell while `self` lives).
    unsafe fn get(&self, data: ffi::GhosttyRenderStateRowCellsData, out: *mut std::ffi::c_void) -> ffi::GhosttyResult {
        // SAFETY: the caller's contract.
        unsafe { ffi::ghostty_render_state_row_cells_get(self.cells.as_ptr(), data, out) }
    }

    /// # Safety
    ///
    /// `out` must be valid for the key's output type.
    unsafe fn cell_get(&self, data: ffi::GhosttyCellData, out: *mut std::ffi::c_void) -> ffi::GhosttyResult {
        // SAFETY: `raw` is a cell value; the caller's contract for `out`.
        unsafe { ffi::ghostty_cell_get(self.raw, data, out) }
    }

    /// The cell's width class.
    #[must_use]
    pub fn width(&self) -> CellWidth {
        let mut wide: ffi::GhosttyCellWide = ffi::GHOSTTY_CELL_WIDE_NARROW;
        // SAFETY: `GHOSTTY_CELL_DATA_WIDE` writes a `GhosttyCellWide`; a failure leaves `NARROW`.
        unsafe { self.cell_get(ffi::GHOSTTY_CELL_DATA_WIDE, (&raw mut wide).cast()) };
        match wide {
            ffi::GHOSTTY_CELL_WIDE_WIDE => CellWidth::Wide,
            ffi::GHOSTTY_CELL_WIDE_SPACER_TAIL => CellWidth::SpacerTail,
            ffi::GHOSTTY_CELL_WIDE_SPACER_HEAD => CellWidth::SpacerHead,
            _ => CellWidth::Narrow,
        }
    }

    /// Whether the cell is part of an OSC 8 hyperlink.
    #[must_use]
    pub fn has_hyperlink(&self) -> bool {
        let mut value = false;
        // SAFETY: `GHOSTTY_CELL_DATA_HAS_HYPERLINK` writes a `bool`.
        unsafe { self.cell_get(ffi::GHOSTTY_CELL_DATA_HAS_HYPERLINK, (&raw mut value).cast()) };
        value
    }

    /// Whether the cell has any style beyond the default (so [`CellRef::style`] is worth reading).
    #[must_use]
    pub fn has_styling(&self) -> bool {
        let mut value = false;
        // SAFETY: `HAS_STYLING` writes a `bool`.
        unsafe {
            self.get(
                ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_HAS_STYLING,
                (&raw mut value).cast(),
            )
        };
        value
    }

    /// The cell's style (the default style when it has none).
    #[must_use]
    pub fn style(&self) -> Style {
        // SAFETY: an all-zero `GhosttyStyle` is a valid value (size 0, no colors, no flags), filled right below.
        let mut raw: ffi::GhosttyStyle = unsafe { std::mem::zeroed() };
        raw.size = std::mem::size_of::<ffi::GhosttyStyle>();
        // SAFETY: `ROW_CELLS_DATA_STYLE` writes a sized `GhosttyStyle`; on failure the default stays.
        let result = unsafe { self.get(ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, (&raw mut raw).cast()) };
        if result == ffi::GHOSTTY_SUCCESS {
            Style::from_raw(&raw)
        } else {
            Style::default()
        }
    }

    /// The resolved foreground (palette indices looked up, bold not applied), or `None` for the default.
    #[must_use]
    pub fn foreground(&self) -> Option<Rgb> {
        self.color(ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR)
    }

    /// The resolved background, or `None` for the default.
    #[must_use]
    pub fn background(&self) -> Option<Rgb> {
        self.color(ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR)
    }

    fn color(&self, data: ffi::GhosttyRenderStateRowCellsData) -> Option<Rgb> {
        let mut color = ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 };
        // SAFETY: the color keys write a `GhosttyColorRgb`, or fail when the cell has none.
        let result = unsafe { self.get(data, (&raw mut color).cast()) };
        (result == ffi::GHOSTTY_SUCCESS).then(|| color.into())
    }

    /// Append the cell's grapheme cluster as UTF-8 to `out`; nothing for a cell without text.
    ///
    /// # Errors
    ///
    /// The library's result when the text cannot be read.
    pub fn grapheme(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let mut stack = [0_u8; 32];
        let mut buffer = ffi::GhosttyBuffer {
            ptr: stack.as_mut_ptr(),
            cap: stack.len(),
            len: 0,
        };
        // SAFETY: `GRAPHEMES_UTF8` writes at most `cap` bytes to `ptr` (a live stack buffer of that size) and sets
        // `len`, or sets `len` to the size it needs and writes nothing.
        let result = unsafe {
            self.get(
                ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8,
                (&raw mut buffer).cast(),
            )
        };
        if result == ffi::GHOSTTY_OUT_OF_SPACE {
            let mut heap = vec![0_u8; buffer.len];
            buffer = ffi::GhosttyBuffer {
                ptr: heap.as_mut_ptr(),
                cap: heap.len(),
                len: 0,
            };
            // SAFETY: as above, with a heap buffer of the size the library asked for.
            let result = unsafe {
                self.get(
                    ffi::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8,
                    (&raw mut buffer).cast(),
                )
            };
            Error::check(result)?;
            out.extend_from_slice(&heap[..buffer.len.min(heap.len())]);
            return Ok(());
        }
        Error::check(result)?;
        out.extend_from_slice(&stack[..buffer.len.min(stack.len())]);
        Ok(())
    }
}
