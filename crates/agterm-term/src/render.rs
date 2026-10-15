//! The pane's frame in `agterm-term`'s own types, and the one capture render state it is converted from.
//!
//! A [`Frame`] is immutable and shared (`Arc`): every view of a pane (the pane itself, the dashboard) draws the same
//! frame. Its rows are `Arc`s too; a row the capture reports clean is carried over from the previous frame by its
//! identity, so a view can tell what to redraw from [`Row::changed`] against the last frame sequence it drew.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agterm_ghostty_vt as vt;
use bitflags::bitflags;

use crate::kitty::KittyView;

/// A 24-bit color.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Conversions from the engine's types stay private: no libghostty-vt type is reachable through this crate's API.
pub(crate) fn rgb(color: vt::Rgb) -> Rgb {
    Rgb::new(color.r, color.g, color.b)
}

bitflags! {
    /// A cell's boolean attributes.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub struct CellFlags: u16 {
        const BOLD = 1;
        const ITALIC = 1 << 1;
        const FAINT = 1 << 2;
        const BLINK = 1 << 3;
        const INVERSE = 1 << 4;
        const INVISIBLE = 1 << 5;
        const STRIKETHROUGH = 1 << 6;
        const OVERLINE = 1 << 7;
        /// Part of an OSC 8 hyperlink.
        const HYPERLINK = 1 << 8;
    }
}

/// The underline style.
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

/// How much of a wide character a cell holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum CellWidth {
    #[default]
    Narrow,
    /// The first column of a two-column character.
    Wide,
    /// The second column of a wide character; it has no text.
    SpacerTail,
    /// The last column of a row whose wide character wrapped to the next row; it has no text.
    SpacerHead,
}

/// One cell of a [`Row`]. Colors are resolved (palette indices looked up); `None` is the default color. Bold-as-bright
/// and inverse are left to the renderer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Cell {
    pub width: CellWidth,
    pub flags: CellFlags,
    pub underline: Underline,
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub underline_color: Option<Rgb>,
    text_start: u32,
    text_len: u32,
}

/// One row of a [`Frame`]: its cells, their text, and the selected columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    changed: u64,
    id: [u64; 2],
    cells: Vec<Cell>,
    text: String,
    selection: Option<(u16, u16)>,
    wrapped: bool,
}

impl Row {
    /// Whether the row soft-wraps: its text continues on the next row.
    #[must_use]
    pub fn wrapped(&self) -> bool {
        self.wrapped
    }

    /// The selected columns of the row, first and last inclusive; `None` when the selection misses it.
    #[must_use]
    pub fn selection(&self) -> Option<(u16, u16)> {
        self.selection
    }

    /// The frame sequence number this row's content was captured in; a view that last drew frame `n` redraws the
    /// row when `changed > n`.
    #[must_use]
    pub fn changed(&self) -> u64 {
        self.changed
    }

    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// The grapheme cluster of the cell at `column`; empty for a blank or spacer cell or past the end.
    #[must_use]
    pub fn text(&self, column: usize) -> &str {
        self.cells.get(column).map_or("", |cell| {
            let start = cell.text_start as usize;
            self.text.get(start..start + cell.text_len as usize).unwrap_or("")
        })
    }

    /// The row as the screen shows it: blank cells as spaces, spacer cells as nothing, trailing blanks trimmed.
    #[must_use]
    pub fn line(&self) -> String {
        let mut line = String::new();
        for (column, cell) in self.cells.iter().enumerate() {
            match cell.width {
                CellWidth::SpacerTail | CellWidth::SpacerHead => {}
                _ => match self.text(column) {
                    "" => line.push(' '),
                    text => line.push_str(text),
                },
            }
        }
        line.truncate(line.trim_end().len());
        line
    }
}

/// The cursor's visual style.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum CursorStyle {
    Bar,
    #[default]
    Block,
    Underline,
    BlockHollow,
}

impl CursorStyle {
    pub(crate) fn engine(self) -> vt::CursorStyle {
        match self {
            Self::Bar => vt::CursorStyle::Bar,
            Self::Block => vt::CursorStyle::Block,
            Self::Underline => vt::CursorStyle::Underline,
            Self::BlockHollow => vt::CursorStyle::BlockHollow,
        }
    }
}

/// The cursor a pane's terminal starts with and returns to (`CSI 0 q`, a full reset) while no program chose another:
/// Ghostty's `cursor-style` and `cursor-style-blink`, which its termio sets on every terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CursorDefaults {
    pub style: CursorStyle,
    pub blink: bool,
}

impl Default for CursorDefaults {
    /// Ghostty's: a block that blinks (`cursor-style-blink` unset counts as on).
    fn default() -> Self {
        Self::from_config(&agterm_core::ghostty_config::Cursor::default())
    }
}

impl CursorDefaults {
    /// From the ghostty config's `cursor-style` and `cursor-style-blink` (unset blinks, as in Ghostty).
    #[must_use]
    pub fn from_config(cursor: &agterm_core::ghostty_config::Cursor) -> Self {
        use agterm_core::ghostty_config::CursorStyle as Config;
        Self {
            style: match cursor.style {
                Config::Bar => CursorStyle::Bar,
                Config::Block => CursorStyle::Block,
                Config::Underline => CursorStyle::Underline,
                Config::BlockHollow => CursorStyle::BlockHollow,
            },
            blink: cursor.style_blink.unwrap_or(true),
        }
    }

    pub(crate) fn apply(self, terminal: &mut vt::Terminal) -> Result<(), vt::Error> {
        terminal.set_default_cursor(self.style.engine(), self.blink)
    }
}

/// Where the cursor sits in the viewport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CursorPosition {
    pub column: u16,
    pub row: u16,
    /// On the tail of a wide character.
    pub wide_tail: bool,
}

/// The cursor of a frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Cursor {
    /// `None` when the cursor is outside the viewport (scrolled back).
    pub position: Option<CursorPosition>,
    /// Visible per the terminal's modes (DECTCEM).
    pub visible: bool,
    pub blinking: bool,
    /// At a password prompt.
    pub password_input: bool,
    pub style: CursorStyle,
}

/// The colors a frame is drawn with: effective defaults and the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Colors {
    pub background: Rgb,
    pub foreground: Rgb,
    /// The cursor color when a program or the config set one.
    pub cursor: Option<Rgb>,
    pub palette: Box<[Rgb; 256]>,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            background: Rgb::default(),
            foreground: Rgb::new(0xff, 0xff, 0xff),
            cursor: None,
            palette: Box::new([Rgb::default(); 256]),
        }
    }
}

/// The theme a pane's terminal starts from: its default colors, which programs override (OSC 4/10/11/12) and a
/// reset restores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Option<Rgb>,
    /// Palette slots the theme sets; the rest keep the built-in palette.
    pub palette: std::collections::BTreeMap<u8, Rgb>,
}

impl Default for ThemeColors {
    /// Ghostty's defaults, as the config parser has them.
    fn default() -> Self {
        Self::from_config(
            &agterm_core::ghostty_config::Colors::default(),
            &agterm_core::ghostty_config::Cursor::default(),
        )
    }
}

impl ThemeColors {
    /// The theme from the ghostty config: `background`, `foreground`, `palette` and a fixed `cursor-color` (a cursor
    /// color that follows the cell is the renderer's business).
    #[must_use]
    pub fn from_config(
        colors: &agterm_core::ghostty_config::Colors,
        cursor: &agterm_core::ghostty_config::Cursor,
    ) -> Self {
        let rgb = |color: agterm_core::chrome::Rgb| Rgb::new(color.r, color.g, color.b);
        Self {
            foreground: rgb(colors.foreground),
            background: rgb(colors.background),
            cursor: match cursor.color {
                Some(agterm_core::ghostty_config::TerminalColor::Color(color)) => Some(rgb(color)),
                _ => None,
            },
            palette: colors
                .palette
                .iter()
                .map(|(&slot, &color)| (slot, rgb(color)))
                .collect(),
        }
    }

    /// Apply to `terminal`: the defaults, and the palette slots over the built-in palette.
    pub(crate) fn apply(&self, terminal: &mut vt::Terminal) -> Result<(), vt::Error> {
        let vt_rgb = |color: Rgb| vt::Rgb::new(color.r, color.g, color.b);
        terminal.set_default_colors(
            Some(vt_rgb(self.foreground)),
            Some(vt_rgb(self.background)),
            self.cursor.map(vt_rgb),
        )?;
        terminal.set_default_palette(None)?;
        if !self.palette.is_empty() {
            let mut palette = *terminal.default_palette();
            for (&slot, &color) in &self.palette {
                palette[usize::from(slot)] = vt_rgb(color);
            }
            terminal.set_default_palette(Some(&palette))?;
        }
        Ok(())
    }
}

/// The mouse pointer shape a program asked for (OSC 22), as CSS cursor names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MouseShape {
    #[default]
    Default,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    AllScroll,
    ColResize,
    RowResize,
    NResize,
    EResize,
    SResize,
    WResize,
    NeResize,
    NwResize,
    SeResize,
    SwResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ZoomIn,
    ZoomOut,
}

impl MouseShape {
    pub(crate) fn from_engine(shape: vt::MouseShape) -> Self {
        use vt::MouseShape as S;
        match shape {
            S::Default => Self::Default,
            S::ContextMenu => Self::ContextMenu,
            S::Help => Self::Help,
            S::Pointer => Self::Pointer,
            S::Progress => Self::Progress,
            S::Wait => Self::Wait,
            S::Cell => Self::Cell,
            S::Crosshair => Self::Crosshair,
            S::Text => Self::Text,
            S::VerticalText => Self::VerticalText,
            S::Alias => Self::Alias,
            S::Copy => Self::Copy,
            S::Move => Self::Move,
            S::NoDrop => Self::NoDrop,
            S::NotAllowed => Self::NotAllowed,
            S::Grab => Self::Grab,
            S::Grabbing => Self::Grabbing,
            S::AllScroll => Self::AllScroll,
            S::ColResize => Self::ColResize,
            S::RowResize => Self::RowResize,
            S::NResize => Self::NResize,
            S::EResize => Self::EResize,
            S::SResize => Self::SResize,
            S::WResize => Self::WResize,
            S::NeResize => Self::NeResize,
            S::NwResize => Self::NwResize,
            S::SeResize => Self::SeResize,
            S::SwResize => Self::SwResize,
            S::EwResize => Self::EwResize,
            S::NsResize => Self::NsResize,
            S::NeswResize => Self::NeswResize,
            S::NwseResize => Self::NwseResize,
            S::ZoomIn => Self::ZoomIn,
            S::ZoomOut => Self::ZoomOut,
        }
    }
}

/// One captured screen of a pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    /// Grows by one with every frame that differs from the previous one; 0 for the empty frame before any capture.
    pub seq: u64,
    pub cols: u16,
    pub rows: Vec<Arc<Row>>,
    pub cursor: Cursor,
    pub colors: Arc<Colors>,
    pub mouse_shape: MouseShape,
    /// Captured at the start of a synchronized-output hold (mode 2026): the screen the program wants left up while it
    /// draws the next one.
    pub held: bool,
    /// The Kitty images and their placements on this screen.
    pub kitty: Arc<KittyView>,
}

impl Frame {
    /// Whether row `index` changed after frame `seen` (a view's last drawn sequence); true past the end.
    #[must_use]
    pub fn row_changed_since(&self, index: usize, seen: u64) -> bool {
        self.rows.get(index).is_none_or(|row| row.changed > seen)
    }

    /// Every row as [`Row::line`] (tests, read-back of the visible screen).
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.rows.iter().map(|row| row.line()).collect()
    }
}

/// The longest a program may hold the screen (mode 2026) before the pane ends the hold itself, as Ghostty does.
pub(crate) const HOLD_TIMEOUT: Duration = Duration::from_secs(1);

/// The pane's one capture render state (dirty flags have a single consumer), behind its own lock: VT, then capture.
#[derive(Debug)]
pub(crate) struct Capture {
    pub(crate) state: vt::RenderState,
    held_since: Option<Instant>,
    /// The hold callback captured a frame that is not converted yet.
    fresh: bool,
    seq: u64,
    /// The Kitty view of the last frame made.
    kitty: Arc<KittyView>,
    /// A Kitty view captured with the state and not converted yet (`None`: unchanged since `kitty`).
    pending_kitty: Option<KittyView>,
}

impl Capture {
    pub(crate) fn new() -> Result<Self, vt::Error> {
        Ok(Self {
            state: vt::RenderState::new()?,
            held_since: None,
            fresh: false,
            seq: 0,
            kitty: Arc::default(),
            pending_kitty: None,
        })
    }

    /// A hold began: capture the frame the program left on screen, its Kitty graphics included, from inside the
    /// callback (VT already held).
    pub(crate) fn begin_hold(&mut self, terminal: &mut vt::HeldTerminal<'_>) {
        if let Err(err) = self.state.update_held(terminal) {
            tracing::warn!(%err, "capturing the held frame failed");
        }
        self.capture_kitty(terminal.kitty_graphics());
        self.held_since = Some(Instant::now());
        self.fresh = true;
    }

    /// Capture the Kitty graphics with the state (the terminal held, as for the state's update); the next conversion
    /// carries them.
    pub(crate) fn capture_kitty(&mut self, graphics: Option<vt::KittyGraphics<'_>>) {
        self.pending_kitty = crate::kitty::capture(graphics, &self.kitty);
    }

    /// Something the dirty flags do not track changed (the selection): the next conversion makes a frame and checks
    /// every row.
    pub(crate) fn touch(&mut self) {
        self.fresh = true;
    }

    pub(crate) fn end_hold(&mut self) {
        self.held_since = None;
    }

    pub(crate) fn held(&self) -> bool {
        self.held_since.is_some()
    }

    /// When the current hold expires, if one is on.
    pub(crate) fn hold_deadline(&self) -> Option<Instant> {
        self.held_since.map(|since| since + HOLD_TIMEOUT)
    }

    /// Whether a hold has lasted longer than [`HOLD_TIMEOUT`].
    pub(crate) fn hold_expired(&self, now: Instant) -> bool {
        self.held_since
            .is_some_and(|since| now.duration_since(since) >= HOLD_TIMEOUT)
    }

    /// Whether the state holds something [`Capture::convert`] has not turned into a frame yet.
    pub(crate) fn has_news(&self) -> bool {
        self.fresh || self.state.dirty() != vt::Dirty::Clean
    }

    /// Turn the state into the next frame after `previous`, or `None` when nothing changed; marks the state clean.
    ///
    /// The cursor, the colors and the mouse shape are compared with `previous` as well: the capture's dirty flags
    /// track cell content, and a cursor that only moved or changed style leaves them clean. The Kitty view is the one
    /// captured with the state ([`Capture::capture_kitty`], or the hold's).
    pub(crate) fn convert(&mut self, previous: &Frame, mouse_shape: MouseShape) -> Result<Option<Frame>, vt::Error> {
        let held = self.held();
        let cursor = self.state.cursor().map(cursor).unwrap_or_default();
        let colors = match self.state.colors().as_ref().map(colors) {
            Some(colors) if colors != *previous.colors => Arc::new(colors),
            _ => Arc::clone(&previous.colors),
        };
        let unchanged = !self.has_news()
            && mouse_shape == previous.mouse_shape
            && held == previous.held
            && cursor == previous.cursor
            && Arc::ptr_eq(&colors, &previous.colors)
            && self.pending_kitty.is_none();
        if unchanged {
            return Ok(None);
        }
        let kitty = self
            .pending_kitty
            .take()
            .map_or_else(|| Arc::clone(&previous.kitty), Arc::new);
        self.kitty = Arc::clone(&kitty);
        let seq = self.seq + 1;
        let full = self.state.dirty() == vt::Dirty::Full;
        let (cols, _) = self.state.size();
        let reuse = if full || cols != previous.cols {
            HashMap::new()
        } else {
            previous.rows.iter().map(|row| (row.id, row)).collect()
        };
        let mut rows = Vec::with_capacity(previous.rows.len());
        let mut walk = self.state.rows()?;
        while let Some(mut row) = walk.next_row() {
            let id = row.id().0;
            let selection = row.selection();
            let kept = (!row.is_dirty()).then(|| reuse.get(&id)).flatten();
            rows.push(match kept {
                Some(kept) if kept.selection == selection => Arc::clone(kept),
                // the same cells under a changed selection
                Some(kept) => Arc::new(Row {
                    changed: seq,
                    selection,
                    ..Row::clone(kept)
                }),
                None => Arc::new(build_row(&mut row, id, seq, &colors.palette)?),
            });
        }
        self.state.clean();
        self.fresh = false;
        self.seq = seq;
        Ok(Some(Frame {
            seq,
            cols,
            rows,
            cursor,
            colors,
            mouse_shape,
            held,
            kitty,
        }))
    }
}

fn colors(colors: &vt::RenderColors) -> Colors {
    Colors {
        background: rgb(colors.background),
        foreground: rgb(colors.foreground),
        cursor: colors.cursor.map(rgb),
        palette: Box::new(colors.palette.map(rgb)),
    }
}

fn cursor(cursor: vt::RenderCursor) -> Cursor {
    Cursor {
        position: cursor.position.map(|position| CursorPosition {
            column: position.x,
            row: position.y,
            wide_tail: position.wide_tail,
        }),
        visible: cursor.visible,
        blinking: cursor.blinking,
        password_input: cursor.password_input,
        style: match cursor.style {
            vt::CursorStyle::Bar => CursorStyle::Bar,
            vt::CursorStyle::Block => CursorStyle::Block,
            vt::CursorStyle::Underline => CursorStyle::Underline,
            vt::CursorStyle::BlockHollow => CursorStyle::BlockHollow,
        },
    }
}

fn build_row(row: &mut vt::RowRef<'_>, id: [u64; 2], seq: u64, palette: &[Rgb; 256]) -> Result<Row, vt::Error> {
    let selection = row.selection();
    let wrapped = row.wrapped();
    let mut text = Vec::new();
    let mut cells = Vec::new();
    let mut walk = row.cells()?;
    while let Some(cell) = walk.next_cell() {
        let cell = cell?;
        let start = text.len();
        cell.grapheme(&mut text)?;
        let mut built = Cell {
            width: width(cell.width()),
            foreground: cell.foreground().map(rgb),
            background: cell.background().map(rgb),
            text_start: u32::try_from(start).unwrap_or(u32::MAX),
            text_len: u32::try_from(text.len() - start).unwrap_or(0),
            ..Cell::default()
        };
        if cell.has_styling() {
            apply_style(&mut built, cell.style(), palette);
        }
        built.flags.set(CellFlags::HYPERLINK, cell.has_hyperlink());
        cells.push(built);
    }
    Ok(Row {
        changed: seq,
        id,
        cells,
        // the library writes UTF-8; a lossy copy can only differ on bytes it never produces
        text: String::from_utf8(text).unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned()),
        selection,
        wrapped,
    })
}

fn width(width: vt::CellWidth) -> CellWidth {
    match width {
        vt::CellWidth::Narrow => CellWidth::Narrow,
        vt::CellWidth::Wide => CellWidth::Wide,
        vt::CellWidth::SpacerTail => CellWidth::SpacerTail,
        vt::CellWidth::SpacerHead => CellWidth::SpacerHead,
    }
}

fn apply_style(cell: &mut Cell, style: vt::Style, palette: &[Rgb; 256]) {
    for (flag, ours) in [
        (vt::StyleFlags::BOLD, CellFlags::BOLD),
        (vt::StyleFlags::ITALIC, CellFlags::ITALIC),
        (vt::StyleFlags::FAINT, CellFlags::FAINT),
        (vt::StyleFlags::BLINK, CellFlags::BLINK),
        (vt::StyleFlags::INVERSE, CellFlags::INVERSE),
        (vt::StyleFlags::INVISIBLE, CellFlags::INVISIBLE),
        (vt::StyleFlags::STRIKETHROUGH, CellFlags::STRIKETHROUGH),
        (vt::StyleFlags::OVERLINE, CellFlags::OVERLINE),
    ] {
        cell.flags.set(ours, style.flags.contains(flag));
    }
    cell.underline = match style.underline {
        vt::Underline::None => Underline::None,
        vt::Underline::Single => Underline::Single,
        vt::Underline::Double => Underline::Double,
        vt::Underline::Curly => Underline::Curly,
        vt::Underline::Dotted => Underline::Dotted,
        vt::Underline::Dashed => Underline::Dashed,
    };
    cell.underline_color = match style.underline_color {
        vt::StyleColor::None => None,
        vt::StyleColor::Palette(index) => Some(palette[usize::from(index)]),
        vt::StyleColor::Rgb(color) => Some(rgb(color)),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal as the crate makes one: the process-global hooks are installed before it seals them.
    fn terminal(cols: u16, rows: u16) -> vt::Terminal {
        crate::init();
        vt::Terminal::new(cols, rows).unwrap()
    }

    fn capture(terminal: &mut vt::Terminal, capture: &mut Capture, previous: &Frame) -> Option<Frame> {
        capture.state.update(terminal).unwrap();
        capture.convert(previous, MouseShape::Default).unwrap()
    }

    #[test]
    fn an_unchanged_screen_makes_no_new_frame_and_clean_rows_are_shared() {
        let mut terminal = terminal(10, 3);
        terminal.vt_write(b"one\r\ntwo\r\n");
        let mut state = Capture::new().unwrap();
        let first = capture(&mut terminal, &mut state, &Frame::default()).unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(first.lines(), ["one", "two", ""]);
        assert!(capture(&mut terminal, &mut state, &first).is_none());

        terminal.vt_write(b"three");
        let second = capture(&mut terminal, &mut state, &first).unwrap();
        assert_eq!(second.seq, 2);
        assert_eq!(second.lines(), ["one", "two", "three"]);
        assert!(Arc::ptr_eq(&first.rows[0], &second.rows[0]));
        assert!(!second.row_changed_since(0, first.seq));
        assert!(second.row_changed_since(2, first.seq));
        assert!(Arc::ptr_eq(&first.colors, &second.colors));
    }

    #[test]
    fn cells_carry_styles_colors_and_widths() {
        let mut terminal = terminal(12, 2);
        terminal.vt_write(
            "\x1b[1;3;4:3;38;2;1;2;3;48;5;1;58;5;2mA\x1b[0m日\x1b]8;;https://x\x1b\\L\x1b]8;;\x1b\\".as_bytes(),
        );
        let mut state = Capture::new().unwrap();
        let frame = capture(&mut terminal, &mut state, &Frame::default()).unwrap();
        let row = &frame.rows[0];
        let palette = &frame.colors.palette;
        let a = row.cells()[0];
        assert_eq!(row.text(0), "A");
        assert!(a.flags.contains(CellFlags::BOLD | CellFlags::ITALIC));
        assert_eq!(a.underline, Underline::Curly);
        assert_eq!(a.foreground, Some(Rgb::new(1, 2, 3)));
        assert_eq!(a.background, Some(palette[1]));
        assert_eq!(a.underline_color, Some(palette[2]));
        assert_eq!(row.text(1), "日");
        assert_eq!(row.cells()[1].width, CellWidth::Wide);
        assert_eq!(row.cells()[2].width, CellWidth::SpacerTail);
        assert_eq!(row.cells()[1].foreground, None);
        assert!(row.cells()[3].flags.contains(CellFlags::HYPERLINK));
        assert_eq!(row.line(), "A日L");
    }

    #[test]
    fn the_cursor_and_colors_are_captured() {
        let mut terminal = terminal(10, 3);
        let theme = ThemeColors {
            cursor: Some(Rgb::new(1, 2, 3)),
            palette: [(5, Rgb::new(5, 5, 5))].into(),
            ..ThemeColors::default()
        };
        theme.apply(&mut terminal).unwrap();
        terminal.vt_write(b"ab\x1b[2 q\x1b]11;rgb:10/20/30\x07");
        let mut state = Capture::new().unwrap();
        let frame = capture(&mut terminal, &mut state, &Frame::default()).unwrap();
        assert_eq!(
            frame.cursor.position,
            Some(CursorPosition {
                column: 2,
                row: 0,
                wide_tail: false
            })
        );
        assert!(frame.cursor.visible);
        assert_eq!(frame.cursor.style, CursorStyle::Block);
        assert_eq!(frame.colors.background, Rgb::new(0x10, 0x20, 0x30));
        assert_eq!(frame.colors.foreground, theme.foreground);
        assert_eq!(frame.colors.cursor, Some(Rgb::new(1, 2, 3)));
        assert_eq!(frame.colors.palette[5], Rgb::new(5, 5, 5));
        terminal.vt_write(b"\x1b[?25l\x1b[6 q");
        let next = capture(&mut terminal, &mut state, &frame).unwrap();
        assert!(!next.cursor.visible);
        assert_eq!(next.cursor.style, CursorStyle::Bar);
    }

    #[test]
    fn a_mouse_shape_change_alone_makes_a_new_frame() {
        let mut terminal = terminal(4, 1);
        let mut state = Capture::new().unwrap();
        let first = capture(&mut terminal, &mut state, &Frame::default()).unwrap();
        state.state.update(&mut terminal).unwrap();
        let next = state.convert(&first, MouseShape::Pointer).unwrap().unwrap();
        assert_eq!(next.mouse_shape, MouseShape::Pointer);
        assert!(Arc::ptr_eq(&first.rows[0], &next.rows[0]));
    }

    #[test]
    fn a_hold_expires_after_the_timeout() {
        let mut state = Capture::new().unwrap();
        assert!(!state.hold_expired(Instant::now()));
        state.held_since = Some(Instant::now());
        assert!(state.held());
        assert!(!state.hold_expired(Instant::now()));
        assert!(state.hold_expired(Instant::now() + HOLD_TIMEOUT));
        state.end_hold();
        assert!(!state.held());
    }
}
