//! A row's drawing plan, built from a frame row and cached per view: merged background spans, text runs to shape (one
//! per run of cells with the same text style) with each cluster's column, and decoration spans.
//!
//! Adapted from the row rebuilding of Ghostty's renderer (`src/renderer/generic.zig` `rebuildRow`, MIT, Copyright (c)
//! 2024 Mitchell Hashimoto, Ghostty contributors) as qwertty-term's `qwertty-term-renderer` ports it (MIT, Copyright
//! (c) 2026 Josh `McKinney`), and from `gpui-ghostty`'s `TerminalView` (Apache-2.0, Copyright Xuanwo): a row is
//! rebuilt only when its content (the frame's `Arc<Row>`, which `agterm-term` carries over while the row is clean), the
//! colors, its marks or the hovered link changed; shaping is redone only for rebuilt rows or a font change.

use std::sync::Arc;

use agterm_term::{CellFlags, CellWidth, Colors, Frame, Row, SearchState, Underline};

use super::colors::{Mark, Palette, cell_paint};

/// Columns `start..start + len` of a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span<T> {
    pub start: u16,
    pub len: u16,
    pub value: T,
}

/// The text style a run shares: the face and the color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextStyle {
    pub bold: bool,
    pub italic: bool,
    pub color: agterm_term::Rgb,
    pub alpha: u8,
}

/// A grapheme cluster of a run: where its text starts in the run's text, and its column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cluster {
    pub byte: usize,
    pub col: u16,
}

/// Cells of one style with text, shaped together (blank and invisible cells end a run, spacer cells do not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRun {
    pub style: TextStyle,
    pub text: String,
    pub clusters: Vec<Cluster>,
}

impl TextRun {
    /// The cluster holding the byte `index` of the text.
    #[must_use]
    pub fn cluster_of(&self, index: usize) -> usize {
        self.clusters
            .partition_point(|cluster| cluster.byte <= index)
            .saturating_sub(1)
    }
}

/// A line drawn over cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Line {
    Underline(Underline),
    Strikethrough,
    Overline,
}

/// A decoration's line and color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Decoration {
    pub line: Line,
    pub color: agterm_term::Rgb,
    pub alpha: u8,
}

/// Everything a row draws, in columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowLayout {
    /// Backgrounds other than the default, merged across equal neighbours.
    pub backgrounds: Vec<Span<agterm_term::Rgb>>,
    pub runs: Vec<TextRun>,
    pub decorations: Vec<Span<Decoration>>,
}

/// The marks of a row: the selection over search matches, inclusive column ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct RowMarks {
    pub selection: Option<(u16, u16)>,
    pub search: Vec<(u16, u16, Mark)>,
    /// The hovered link's columns: underlined (a double underline when the cell is already underlined once).
    pub link: Option<(u16, u16)>,
}

impl RowMarks {
    fn at(&self, col: u16) -> Mark {
        if self.selection.is_some_and(|(start, end)| (start..=end).contains(&col)) {
            return Mark::Selection;
        }
        self.search
            .iter()
            .find(|(start, end, _)| (*start..=*end).contains(&col))
            .map_or(Mark::None, |(_, _, mark)| *mark)
    }

    fn linked(&self, col: u16) -> bool {
        self.link.is_some_and(|(start, end)| (start..=end).contains(&col))
    }
}

/// The search marks of every viewport row (a match may span rows): the selected match over the others.
#[must_use]
pub fn search_marks(state: &SearchState, rows: usize, cols: u16) -> Vec<Vec<(u16, u16, Mark)>> {
    let mut marks = vec![Vec::new(); rows];
    let last = cols.saturating_sub(1);
    let selected = state.selected_highlight.iter().map(|hl| (hl, Mark::SearchSelected));
    let others = state.highlights.iter().map(|hl| (hl, Mark::Search));
    for (highlight, mark) in selected.chain(others) {
        let (first, final_row) = (highlight.start.1, highlight.end.1);
        for row in first..=final_row {
            let Some(row_marks) = marks.get_mut(usize::from(row)) else {
                continue;
            };
            let start = if row == first { highlight.start.0 } else { 0 };
            let end = if row == final_row { highlight.end.0 } else { last };
            row_marks.push((start, end, mark));
        }
    }
    marks
}

/// One cell's share of the plan, before merging.
struct CellPlan<'a> {
    col: u16,
    width: u16,
    text: &'a str,
    background: Option<agterm_term::Rgb>,
    style: Option<TextStyle>,
    decorations: [Option<Decoration>; 3],
}

/// The cell's columns: two for a wide character, none for a spacer.
fn cell_width(width: CellWidth) -> u16 {
    match width {
        CellWidth::Narrow => 1,
        CellWidth::Wide => 2,
        CellWidth::SpacerTail | CellWidth::SpacerHead => 0,
    }
}

fn plan_cell<'a>(row: &'a Row, col: u16, colors: &Colors, palette: &Palette, marks: &RowMarks) -> CellPlan<'a> {
    let index = usize::from(col);
    let cell = row.cells()[index];
    let text = row.text(index);
    // a wide character's tail takes the mark of its head
    let mark_col = if cell.width == CellWidth::SpacerTail {
        col.saturating_sub(1)
    } else {
        col
    };
    let paint = cell_paint(&cell, text, marks.at(mark_col), colors, palette);
    let visible = !cell.flags.contains(CellFlags::INVISIBLE);
    let width = cell_width(cell.width);
    // a blank or a space has no glyph to shape
    let style = (visible && !text.trim().is_empty() && width > 0).then_some(TextStyle {
        bold: cell.flags.contains(CellFlags::BOLD),
        italic: cell.flags.contains(CellFlags::ITALIC),
        color: paint.foreground,
        alpha: paint.alpha,
    });
    let decorate = |line: Line, on: bool| {
        (on && visible && width > 0).then_some(Decoration {
            line,
            color: match line {
                Line::Underline(_) => cell.underline_color.unwrap_or(paint.foreground),
                Line::Strikethrough | Line::Overline => paint.foreground,
            },
            alpha: paint.alpha,
        })
    };
    let underline = match (cell.underline, marks.linked(mark_col)) {
        (Underline::Single, true) => Underline::Double,
        (Underline::None, true) => Underline::Single,
        (underline, _) => underline,
    };
    CellPlan {
        col,
        width,
        text,
        background: paint.background,
        style,
        decorations: [
            decorate(Line::Underline(underline), underline != Underline::None),
            decorate(Line::Strikethrough, cell.flags.contains(CellFlags::STRIKETHROUGH)),
            decorate(Line::Overline, cell.flags.contains(CellFlags::OVERLINE)),
        ],
    }
}

/// Extends the last span when `value` continues it at `col`, else starts a new one.
fn push_span<T: PartialEq>(spans: &mut Vec<Span<T>>, col: u16, len: u16, value: T) {
    if let Some(last) = spans.last_mut()
        && last.value == value
        && last.start + last.len == col
    {
        last.len += len;
        return;
    }
    spans.push(Span { start: col, len, value });
}

/// The plan of `row`.
#[must_use]
pub fn layout_row(row: &Row, colors: &Colors, palette: &Palette, marks: &RowMarks) -> RowLayout {
    let mut layout = RowLayout::default();
    // each line kind merges with its own previous span
    let mut lines: [Vec<Span<Decoration>>; 3] = Default::default();
    let mut current: Option<TextRun> = None;
    let cols = u16::try_from(row.cells().len()).unwrap_or(u16::MAX);
    for col in 0..cols {
        let cell = plan_cell(row, col, colors, palette, marks);
        if let Some(background) = cell.background {
            push_span(&mut layout.backgrounds, cell.col, 1, background);
        }
        for (spans, decoration) in lines.iter_mut().zip(cell.decorations) {
            if let Some(decoration) = decoration {
                push_span(spans, cell.col, cell.width, decoration);
            }
        }
        if cell.width == 0 {
            // a spacer: no text of its own, and the run goes on past it
            continue;
        }
        match cell.style {
            Some(style) => {
                let run = match current.take() {
                    Some(run) if run.style == style => run,
                    other => {
                        layout.runs.extend(other);
                        TextRun {
                            style,
                            text: String::new(),
                            clusters: Vec::new(),
                        }
                    }
                };
                let mut run = run;
                run.clusters.push(Cluster {
                    byte: run.text.len(),
                    col: cell.col,
                });
                run.text.push_str(cell.text);
                current = Some(run);
            }
            None => layout.runs.extend(current.take()),
        }
    }
    layout.runs.extend(current);
    layout.decorations = lines.into_iter().flatten().collect();
    layout
}

/// What a cached row was built from.
#[derive(Debug, Clone)]
struct Inputs {
    row: Arc<Row>,
    colors: Arc<Colors>,
    palette: Arc<Palette>,
    marks: RowMarks,
}

impl Inputs {
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.row, &other.row)
            && Arc::ptr_eq(&self.colors, &other.colors)
            && Arc::ptr_eq(&self.palette, &other.palette)
            && self.marks == other.marks
    }
}

/// One cached row: its plan and what the view derived from it for its font (`T`, the shaped glyphs).
#[derive(Debug)]
pub struct CachedRow<T> {
    inputs: Inputs,
    pub layout: RowLayout,
    /// Derived from the layout for the cache's font; `None` until the view shapes it.
    pub shaped: Option<T>,
}

/// A view's rows: rebuilt only when their inputs changed, shaped again only when rebuilt or the font changed.
#[derive(Debug)]
pub struct RowCache<T, K> {
    rows: Vec<Option<CachedRow<T>>>,
    font: Option<K>,
    /// Rows rebuilt by the last [`RowCache::update`].
    pub rebuilt: Vec<usize>,
}

impl<T, K> Default for RowCache<T, K> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            font: None,
            rebuilt: Vec::new(),
        }
    }
}

impl<T, K: PartialEq> RowCache<T, K> {
    /// Brings every row up to `frame`, with `marks` per row (missing rows count as unmarked); a changed `font` drops
    /// every shaping.
    pub fn update(&mut self, frame: &Frame, palette: &Arc<Palette>, marks: &[RowMarks], font: K) {
        if self.font.as_ref() != Some(&font) {
            for row in self.rows.iter_mut().flatten() {
                row.shaped = None;
            }
            self.font = Some(font);
        }
        self.rows.truncate(frame.rows.len());
        self.rows.resize_with(frame.rows.len(), || None);
        self.rebuilt.clear();
        let unmarked = RowMarks::default();
        for (index, row) in frame.rows.iter().enumerate() {
            let inputs = Inputs {
                row: Arc::clone(row),
                colors: Arc::clone(&frame.colors),
                palette: Arc::clone(palette),
                marks: marks.get(index).unwrap_or(&unmarked).clone(),
            };
            let slot = &mut self.rows[index];
            if slot.as_ref().is_some_and(|cached| cached.inputs.same(&inputs)) {
                continue;
            }
            let layout = layout_row(&inputs.row, &inputs.colors, &inputs.palette, &inputs.marks);
            *slot = Some(CachedRow {
                inputs,
                layout,
                shaped: None,
            });
            self.rebuilt.push(index);
        }
    }

    /// The cached rows, in order.
    pub fn rows(&self) -> impl Iterator<Item = &CachedRow<T>> {
        self.rows.iter().flatten()
    }

    /// The cached rows, to shape what is missing.
    pub fn rows_mut(&mut self) -> impl Iterator<Item = &mut CachedRow<T>> {
        self.rows.iter_mut().flatten()
    }

    /// Row `index`.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<&CachedRow<T>> {
        self.rows.get(index).and_then(Option::as_ref)
    }

    /// Forgets everything (a torn-down terminal).
    pub fn clear(&mut self) {
        self.rows.clear();
        self.font = None;
    }
}
