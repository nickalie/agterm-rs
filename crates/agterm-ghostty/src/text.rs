//! Reading a surface's text: the region a read covers ([`Selection`] of two [`Point`]s, as `ghostty.h` spells them),
//! what comes back ([`SurfaceText`]), and the cursor column solved from libghostty's geometry
//! ([`solve_cursor_column`]).

use agterm_ghostty_sys as sys;

use crate::surface::{ImePoint, SurfaceSize};

/// Which coordinate space a point is in (`ghostty_point_tag_e`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointTag {
    /// The active area: the screen's bottom rows a program writes to.
    Active,
    /// What is visible now, scrolled or not.
    Viewport,
    /// The whole screen, scrollback included.
    Screen,
    /// Surface pixels.
    Surface,
}

impl PointTag {
    fn raw(self) -> sys::ghostty_point_tag_e {
        match self {
            Self::Active => sys::GHOSTTY_POINT_ACTIVE,
            Self::Viewport => sys::GHOSTTY_POINT_VIEWPORT,
            Self::Screen => sys::GHOSTTY_POINT_SCREEN,
            Self::Surface => sys::GHOSTTY_POINT_SURFACE,
        }
    }
}

/// How a point's `x`/`y` are read (`ghostty_point_coord_e`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointCoord {
    /// The cell at `x`/`y`.
    Exact,
    /// The space's top-left cell; `x`/`y` are ignored.
    TopLeft,
    /// The space's bottom-right cell; `x`/`y` are ignored.
    BottomRight,
}

impl PointCoord {
    fn raw(self) -> sys::ghostty_point_coord_e {
        match self {
            Self::Exact => sys::GHOSTTY_POINT_COORD_EXACT,
            Self::TopLeft => sys::GHOSTTY_POINT_COORD_TOP_LEFT,
            Self::BottomRight => sys::GHOSTTY_POINT_COORD_BOTTOM_RIGHT,
        }
    }
}

/// One end of a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Point {
    pub tag: PointTag,
    pub coord: PointCoord,
    pub x: u32,
    pub y: u32,
}

impl Point {
    pub(crate) fn raw(self) -> sys::ghostty_point_s {
        sys::ghostty_point_s {
            tag: self.tag.raw(),
            coord: self.coord.raw(),
            x: self.x,
            y: self.y,
        }
    }
}

/// A region to read. Both ends always carry their tag and coord: a zeroed `ghostty_point_s` is the active area's
/// exact cell (both enums' 0), not the viewport's top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Selection {
    pub top_left: Point,
    pub bottom_right: Point,
    pub rectangle: bool,
}

impl Selection {
    /// All of `tag`'s space, from its top-left cell to its bottom-right one.
    #[must_use]
    pub fn whole(tag: PointTag) -> Self {
        let corner = |coord| Point { tag, coord, x: 0, y: 0 };
        Self {
            top_left: corner(PointCoord::TopLeft),
            bottom_right: corner(PointCoord::BottomRight),
            rectangle: false,
        }
    }

    /// The one cell at `x`/`y` of `tag`'s space.
    #[must_use]
    pub fn cell(tag: PointTag, x: u32, y: u32) -> Self {
        let point = Point {
            tag,
            coord: PointCoord::Exact,
            x,
            y,
        };
        Self {
            top_left: point,
            bottom_right: point,
            rectangle: false,
        }
    }

    pub(crate) fn raw(self) -> sys::ghostty_selection_s {
        sys::ghostty_selection_s {
            top_left: self.top_left.raw(),
            bottom_right: self.bottom_right.raw(),
            rectangle: self.rectangle,
        }
    }
}

/// Text read from a surface, copied out of libghostty's buffer (which is freed).
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceText {
    /// The text, UTF-8 with invalid sequences replaced; empty for a blank region.
    pub text: String,
    /// The region's top-left cell in logical units from the surface's top left (padding included); negative when
    /// libghostty could not place it.
    pub top_left_x: f64,
    pub top_left_y: f64,
    pub offset_start: u32,
    pub offset_len: u32,
}

/// The cursor's zero-based column from libghostty's geometry (upstream's `readCursorColumn`); `None` when it cannot
/// be trusted.
///
/// libghostty exports no cursor accessor, so the column is solved for. `ghostty_surface_ime_point` reports the cursor
/// cell's horizontal midpoint as `(column * cellWidth + paddingLeft + cellWidth / 2) / contentScale`, and the padding
/// term is unknown (the size carries none, and a user config may set any). The viewport's top-left cell measures it:
/// its `tl_px_x` (`column_zero_x`) is `(0 * cellWidth + paddingLeft) / contentScale`, so subtracting leaves
/// `column + 0.5` cells, under any padding or balance mode.
///
/// The calls behind the inputs each take the renderer lock separately, so `before` (read ahead of the IME point) and
/// `after` (read behind it) must agree on the grid, or a font-size change or resize mixed two coordinate systems.
/// `ime.height` is one cell height over the content scale, the divisor behind `ime.x` and `column_zero_x` too; with
/// equal X/Y scales it recovers the logical cell width.
#[must_use]
pub fn solve_cursor_column(column_zero_x: f64, before: SurfaceSize, ime: ImePoint, after: SurfaceSize) -> Option<u32> {
    // -1 marks a cell libghostty could not place; the viewport's own top left always resolves, so it is a failed
    // calibration, not a padding of zero (NaN fails too)
    if column_zero_x.is_nan() || column_zero_x < 0.0 {
        return None;
    }
    if before.cell_width_px == 0 || before.cell_height_px == 0 || before.columns == 0 {
        return None;
    }
    let same_grid = after.cell_width_px == before.cell_width_px
        && after.cell_height_px == before.cell_height_px
        && after.columns == before.columns;
    if !same_grid || !ime.height.is_finite() || ime.height <= 0.0 {
        return None;
    }
    let cell_width = ime.height * f64::from(before.cell_width_px) / f64::from(before.cell_height_px);
    let column = ((ime.x - column_zero_x) / cell_width).floor();
    if !column.is_finite() || column < 0.0 || column >= f64::from(before.columns) {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a finite whole number in 0..columns (a u16)"
    )]
    Some(column as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_region_sets_tag_and_corner_on_both_ends() {
        let raw = Selection::whole(PointTag::Viewport).raw();
        assert_eq!(raw.top_left.tag, sys::GHOSTTY_POINT_VIEWPORT);
        assert_eq!(raw.top_left.coord, sys::GHOSTTY_POINT_COORD_TOP_LEFT);
        assert_eq!(raw.bottom_right.tag, sys::GHOSTTY_POINT_VIEWPORT);
        assert_eq!(raw.bottom_right.coord, sys::GHOSTTY_POINT_COORD_BOTTOM_RIGHT);
        assert!(!raw.rectangle);
        let screen = Selection::whole(PointTag::Screen).raw();
        assert_eq!(screen.top_left.tag, sys::GHOSTTY_POINT_SCREEN);
        assert_eq!(screen.bottom_right.tag, sys::GHOSTTY_POINT_SCREEN);
    }

    #[test]
    fn a_cell_region_is_one_exact_point() {
        let raw = Selection::cell(PointTag::Viewport, 0, 0).raw();
        for end in [raw.top_left, raw.bottom_right] {
            assert_eq!(end.tag, sys::GHOSTTY_POINT_VIEWPORT);
            assert_eq!(end.coord, sys::GHOSTTY_POINT_COORD_EXACT);
            assert_eq!((end.x, end.y), (0, 0));
        }
        assert_eq!(PointTag::Active.raw(), sys::GHOSTTY_POINT_ACTIVE);
        assert_eq!(PointTag::Surface.raw(), sys::GHOSTTY_POINT_SURFACE);
    }

    /// 80 columns of 8x16-pixel cells at content scale 2: a logical cell is 4 wide and 8 high.
    fn grid() -> SurfaceSize {
        SurfaceSize {
            columns: 80,
            rows: 24,
            width_px: 640,
            height_px: 384,
            cell_width_px: 8,
            cell_height_px: 16,
        }
    }

    /// The IME point of column `column` with `padding` logical units of left padding: the cell's midpoint.
    fn ime_at(column: f64, padding: f64) -> ImePoint {
        ImePoint {
            x: column * 4.0 + padding + 2.0,
            y: 0.0,
            width: 0.0,
            height: 8.0,
        }
    }

    #[test]
    fn the_column_is_solved_under_any_padding() {
        for padding in [0.0, 1.5, 6.0, 37.25] {
            for column in [0_u32, 1, 9, 79] {
                let ime = ime_at(f64::from(column), padding);
                assert_eq!(
                    solve_cursor_column(padding, grid(), ime, grid()),
                    Some(column),
                    "column {column} at padding {padding}"
                );
            }
        }
    }

    #[test]
    fn an_unplaced_calibration_cell_is_no_reading() {
        assert_eq!(solve_cursor_column(-1.0, grid(), ime_at(3.0, 0.0), grid()), None);
        assert_eq!(solve_cursor_column(f64::NAN, grid(), ime_at(3.0, 0.0), grid()), None);
    }

    #[test]
    fn a_grid_change_between_the_reads_is_no_reading() {
        let resized = SurfaceSize { columns: 100, ..grid() };
        let refonted = SurfaceSize {
            cell_width_px: 9,
            ..grid()
        };
        let taller = SurfaceSize {
            cell_height_px: 18,
            ..grid()
        };
        for after in [resized, refonted, taller] {
            assert_eq!(solve_cursor_column(0.0, grid(), ime_at(3.0, 0.0), after), None);
        }
    }

    #[test]
    fn an_empty_grid_or_a_bad_cell_height_is_no_reading() {
        let no_columns = SurfaceSize { columns: 0, ..grid() };
        assert_eq!(solve_cursor_column(0.0, no_columns, ime_at(0.0, 0.0), no_columns), None);
        let no_cell = SurfaceSize {
            cell_height_px: 0,
            ..grid()
        };
        assert_eq!(solve_cursor_column(0.0, no_cell, ime_at(0.0, 0.0), no_cell), None);
        for height in [0.0, -8.0, f64::INFINITY, f64::NAN] {
            let ime = ImePoint {
                height,
                ..ime_at(3.0, 0.0)
            };
            assert_eq!(solve_cursor_column(0.0, grid(), ime, grid()), None, "height {height}");
        }
    }

    #[test]
    fn a_cursor_outside_the_grid_is_no_reading() {
        assert_eq!(solve_cursor_column(10.0, grid(), ime_at(0.0, 0.0), grid()), None);
        assert_eq!(solve_cursor_column(0.0, grid(), ime_at(80.0, 0.0), grid()), None);
    }
}
