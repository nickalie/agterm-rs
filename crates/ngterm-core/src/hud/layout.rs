//! The panel's sizing math and the body file the painter reads (upstream's `HudLayout`, `PaneMetrics`,
//! `HudPaneFrame(s)` and `HudPanelSize`): message to a cell box, cell box to the pane percentages the overlay slot
//! understands, and the exact bytes `hud.sh` reads.

use unicode_normalization::UnicodeNormalization;

use super::markdown::{self, Run, Style};
use super::{HudPosition, HudSpec, HudSpinner};
use crate::model::is_valid_color_hex;

/// Widest content line before wrapping; the frame padding sits outside it.
pub const MAX_COLUMNS: usize = 60;
pub const MAX_SIZE_PERCENT: i64 = 80;
pub const MIN_SIZE_PERCENT: i64 = 10;
/// The only HUD-specific variable in the painter's environment: the path to the body file. Everything an update may
/// change rides in that file's header instead (see [`rendered_body`]).
pub const FILE_ENV_KEY: &str = "NGTERM_HUD_FILE";
/// Frame padding in cells, on both sides of the content.
pub const HORIZONTAL_PADDING: usize = 2;
pub const VERTICAL_PADDING: usize = 1;
/// Cells the spinner glyph and its trailing space claim, so turning the spinner on cannot rewrap text.
pub const SPINNER_WIDTH: usize = 2;
/// The header's spelling for "no text color": never empty, since the painter word-splits the header.
pub const NO_TEXT_COLOR: &str = "-";

/// Terminal cell and pane dimensions in points, as the frontend measures them. `padding_*` is the terminal's own
/// padding inside the panel, per side; zero when unknown.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PaneMetrics {
    pub cell_width: f64,
    pub cell_height: f64,
    pub pane_width: f64,
    pub pane_height: f64,
    pub padding_width: f64,
    pub padding_height: f64,
}

impl PaneMetrics {
    /// Metrics with no terminal padding.
    #[must_use]
    pub fn new(cell_width: f64, cell_height: f64, pane_width: f64, pane_height: f64) -> Self {
        Self {
            cell_width,
            cell_height,
            pane_width,
            pane_height,
            padding_width: 0.0,
            padding_height: 0.0,
        }
    }
}

/// One deck pane host's bounds in its session detail coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HudPaneFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The live deck frames the pane hosts report; a session keeps the last non-empty ones so a hidden pane's HUD can
/// still update its paint grid.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HudPaneFrames {
    pub left: Option<HudPaneFrame>,
    pub right: Option<HudPaneFrame>,
    /// The session detail area a session-wide HUD is laid out in.
    pub detail: Option<HudPaneFrame>,
}

impl HudPaneFrames {
    /// The right pane's frame when `right`, else the left's (upstream's `subscript(OverlayPane)`).
    #[must_use]
    pub fn pane(&self, right: bool) -> Option<HudPaneFrame> {
        if right { self.right } else { self.left }
    }

    /// Takes every frame `other` has, keeping the rest.
    pub fn merge(&mut self, other: &HudPaneFrames) {
        self.left = other.left.or(self.left);
        self.right = other.right.or(self.right);
        self.detail = other.detail.or(self.detail);
    }
}

/// The panel's share of the pane on each axis, decided together by [`panel_size`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HudPanelSize {
    pub width_percent: i64,
    pub height_percent: i64,
    /// The height the panel is drawn and painted at, already capped; `None` for an unmeasured pane, which falls back
    /// to `height_percent`.
    pub height_points: Option<f64>,
}

impl HudPanelSize {
    /// A size with no measured height in points.
    #[must_use]
    pub fn new(width_percent: i64, height_percent: i64) -> Self {
        Self {
            width_percent,
            height_percent,
            height_points: None,
        }
    }
}

/// A cell grid: upstream's `(columns: Int, rows: Int)` tuples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Grid {
    pub columns: usize,
    pub rows: usize,
}

/// A markdown body's finished rows and the widest painted row, gutter included (never 0, which the header reserves
/// for plain mode).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownBody {
    pub lines: Vec<String>,
    pub block_width: usize,
}

/// Bounds a width into the range a panel holding its edge margin may take. Every path sizing a live HUD takes
/// [`clamp_size_percent_for`] instead, since a sticky spec may take more.
#[must_use]
pub fn clamp_size_percent(requested: i64) -> i64 {
    requested.clamp(MIN_SIZE_PERCENT, MAX_SIZE_PERCENT)
}

/// The widest `spec`'s panel may be: a sticky panel off center owes no edge margin, so it may span the pane.
#[must_use]
pub fn max_width_percent(spec: &HudSpec) -> i64 {
    if spec.sticky && spec.position != HudPosition::Center {
        100
    } else {
        MAX_SIZE_PERCENT
    }
}

/// Bounds a width for `spec`'s own panel (upstream's `clampSizePercent(_:for:)`).
#[must_use]
pub fn clamp_size_percent_for(requested: i64, spec: &HudSpec) -> i64 {
    requested.clamp(MIN_SIZE_PERCENT, max_width_percent(spec))
}

/// Blank rows above and below the text: one each for a framed panel, none for a frameless one.
fn vertical_padding(spec: &HudSpec) -> usize {
    if spec.frame { VERTICAL_PADDING } else { 0 }
}

fn spinner_gutter(spec: &HudSpec) -> usize {
    if spec.spinner.is_some() { SPINNER_WIDTH } else { 0 }
}

/// The cell box `spec`'s panel needs: the content wrapped at `wrap_columns` (a plain message always wraps at
/// [`MAX_COLUMNS`]) plus the frame padding (upstream's `box(for:wrapColumns:)`).
#[must_use]
pub fn box_for(spec: &HudSpec, wrap_columns: usize) -> Grid {
    let widths: Vec<usize> = if spec.markdown {
        markdown_rows(spec, wrap_columns)
            .iter()
            .map(|row| markdown::width(row))
            .collect()
    } else {
        body_lines(spec).iter().map(|line| cell_count(line)).collect()
    };
    let content = (widths.iter().copied().max().unwrap_or(0) + spinner_gutter(spec)).max(1);
    Grid {
        columns: content + HORIZONTAL_PADDING * 2,
        rows: widths.len().max(1) + vertical_padding(spec) * 2,
    }
}

/// Decides both axes off one [`box_for`]: the caller's `size_percent` reaches the width alone, the height is always
/// measured.
#[must_use]
pub fn panel_size(spec: &HudSpec, pane: &PaneMetrics) -> HudPanelSize {
    let grid = box_for(spec, wrap_columns(spec, pane));
    HudPanelSize {
        width_percent: spec
            .size_percent
            .unwrap_or_else(|| width_percent(grid, pane, max_width_percent(spec))),
        height_percent: height_percent(grid, pane),
        height_points: height_points(grid, pane),
    }
}

/// The width a markdown message wraps at before its panel exists: the text columns of the widest panel this pane
/// allows, at the caller's percent clamped as the store clamps it. [`MAX_COLUMNS`] for a plain message and for an
/// unmeasured pane.
#[must_use]
pub fn wrap_columns(spec: &HudSpec, pane: &PaneMetrics) -> usize {
    if !spec.markdown {
        return MAX_COLUMNS;
    }
    let widest = HudPanelSize::new(
        spec.size_percent.map_or_else(
            || max_width_percent(spec),
            |percent| clamp_size_percent_for(percent, spec),
        ),
        MAX_SIZE_PERCENT,
    );
    panel_grid(&widest, pane).map_or(MAX_COLUMNS, |grid| text_columns(grid.columns, spec))
}

/// What a panel `columns` wide leaves for text once its padding and the spinner's gutter are taken, bounded by
/// [`MAX_COLUMNS`] only while the app measures the width.
#[must_use]
pub fn text_columns(columns: usize, spec: &HudSpec) -> usize {
    let available = columns
        .saturating_sub(HORIZONTAL_PADDING * 2 + spinner_gutter(spec))
        .max(1);
    if spec.size_percent.is_none() {
        available.min(MAX_COLUMNS)
    } else {
        available
    }
}

/// The share of the pane's width the panel takes: the box's columns plus the terminal padding, clamped into
/// `MIN_SIZE_PERCENT..=max_percent`. An unmeasured pane takes `max_percent`.
#[must_use]
pub fn width_percent(grid: Grid, pane: &PaneMetrics, max_percent: i64) -> i64 {
    let needed = points(grid.columns) * pane.cell_width + pane.padding_width * 2.0;
    percent(needed, pane.pane_width)
        .unwrap_or(max_percent)
        .clamp(MIN_SIZE_PERCENT, max_percent)
}

/// The share of the pane's height the panel takes, measured from the content alone and with no minimum floor (the
/// box already carries its padding rows). An unmeasured pane falls back to [`MIN_SIZE_PERCENT`].
#[must_use]
pub fn height_percent(grid: Grid, pane: &PaneMetrics) -> i64 {
    let needed = points(grid.rows) * pane.cell_height + pane.padding_height * 2.0;
    percent(needed, pane.pane_height).map_or(MIN_SIZE_PERCENT, |measured| measured.clamp(1, MAX_SIZE_PERCENT))
}

/// The same need in points under the same cap: what the panel is laid out and painted at. `None` for an unmeasured
/// pane.
#[must_use]
pub fn height_points(grid: Grid, pane: &PaneMetrics) -> Option<f64> {
    if pane.pane_height <= 0.0 || pane.cell_height <= 0.0 {
        return None;
    }
    let needed = points(grid.rows) * pane.cell_height + pane.padding_height * 2.0;
    Some(needed.min(pane.pane_height * float(MAX_SIZE_PERCENT) / 100.0))
}

/// The cell grid the panel itself gets: its width percent of the pane and its height in points (the percent only
/// when no points were measured), each less the terminal padding, over one cell. `None` for an unmeasured pane or
/// one that leaves no whole cell.
#[must_use]
pub fn panel_grid(size: &HudPanelSize, pane: &PaneMetrics) -> Option<Grid> {
    let measured = [pane.cell_width, pane.cell_height, pane.pane_width, pane.pane_height];
    if measured.iter().any(|value| *value <= 0.0 || value.is_nan()) {
        return None;
    }
    let width = pane.pane_width * float(size.width_percent) / 100.0;
    let height = size
        .height_points
        .unwrap_or(pane.pane_height * float(size.height_percent) / 100.0);
    Some(Grid {
        columns: whole_cells((width - pane.padding_width * 2.0) / pane.cell_width)?,
        // the points were built from whole rows, and the division can land a hair under the integer
        rows: whole_cells((height - pane.padding_height * 2.0) / pane.cell_height + 0.001)?,
    })
}

/// The grid the body's header carries: the panel's own, or the content box when nothing was measured. `size` must be
/// the effective one the panel took.
#[must_use]
pub fn paint_grid(spec: &HudSpec, size: &HudPanelSize, pane: &PaneMetrics) -> Grid {
    panel_grid(size, pane).unwrap_or_else(|| box_for(spec, MAX_COLUMNS))
}

/// The SGR parameters of a truecolor foreground (`38;2;<r>;<g>;<b>`), or [`NO_TEXT_COLOR`] for no or a malformed
/// color. Parameters only: the painter wraps them in the escape.
#[must_use]
pub fn foreground_sgr(hex: Option<&str>) -> String {
    let Some(hex) = hex.filter(|hex| is_valid_color_hex(hex)) else {
        return NO_TEXT_COLOR.to_owned();
    };
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    let channels: Option<Vec<String>> = (0..3)
        .map(|index| {
            let pair = digits.get(index * 2..index * 2 + 2)?;
            u8::from_str_radix(pair, 16).ok().map(|channel| channel.to_string())
        })
        .collect();
    channels.map_or_else(
        || NO_TEXT_COLOR.to_owned(),
        |channels| format!("38;2;{}", channels.join(";")),
    )
}

/// The bytes written to [`FILE_ENV_KEY`]'s file: a `<columns> <rows> <spinner> <pid> <interval> <textcolor>
/// <blockwidth> [frame...]` header line, then the body. `blockwidth` 0 is plain mode (the wrapped message, one empty
/// line, the wrapped detail); any other is markdown mode, finished rows from [`markdown_body`]. The frames come last,
/// the only variable-length part. `grid` is the panel's own from [`paint_grid`]; `owner_pid` is the writing
/// process's, which the painter polls so a hard-killed app stops it.
#[must_use]
pub fn rendered_body(spec: &HudSpec, grid: Grid, owner_pid: u32) -> String {
    let interval = spec.spinner.map_or(HudSpinner::STATIC_INTERVAL, HudSpinner::interval);
    let frames: String = spec
        .spinner
        .map(HudSpinner::frames)
        .unwrap_or_default()
        .iter()
        .flat_map(|frame| [" ", frame])
        .collect();
    let (lines, block_width) = if spec.markdown {
        let body = markdown_body(spec, grid);
        (body.lines, body.block_width)
    } else {
        (body_lines(spec), 0)
    };
    let mut out = format!(
        "{} {} {} {owner_pid} {interval} {} {block_width}{frames}\n",
        grid.columns,
        grid.rows,
        u8::from(spec.spinner.is_some()),
        foreground_sgr(spec.text_color.as_deref()),
    );
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// A markdown spec laid out unclipped: the message's rows at `columns`, then, when a detail is set, a blank row and
/// the detail's rows dimmed.
#[must_use]
pub fn markdown_rows(spec: &HudSpec, columns: usize) -> Vec<Vec<Run>> {
    let mut rows = markdown::rows(&markdown::lines(&spec.message), columns);
    let detail = wrap(spec.detail.as_deref().unwrap_or_default(), columns);
    if !detail.is_empty() {
        rows.push(Vec::new());
        rows.extend(detail.into_iter().map(|line| vec![Run::new(line, Style::DIM)]));
    }
    rows
}

/// The markdown rows wrapped at `grid` less its padding and the spinner's gutter, clipped to what fits, then every row
/// after the first indented by that gutter (the painter draws the glyph on the first).
#[must_use]
pub fn markdown_body(spec: &HudSpec, grid: Grid) -> MarkdownBody {
    let gutter = spinner_gutter(spec);
    let rows = markdown::fitted(
        &markdown_rows(spec, text_columns(grid.columns, spec)),
        grid.columns.saturating_sub(HORIZONTAL_PADDING * 2 + gutter),
        grid.rows.saturating_sub(vertical_padding(spec) * 2),
    );
    let block_width = (rows.iter().map(|row| markdown::width(row)).max().unwrap_or(0) + gutter).max(1);
    let indent = " ".repeat(gutter);
    let lines = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let prefix = if index > 0 && !row.is_empty() {
                indent.as_str()
            } else {
                ""
            };
            format!("{prefix}{}", markdown::sgr(row))
        })
        .collect();
    MarkdownBody { lines, block_width }
}

/// A plain spec's body: the wrapped message, then one empty line and the wrapped detail when there is one.
#[must_use]
pub fn body_lines(spec: &HudSpec) -> Vec<String> {
    let mut lines = wrap(&spec.message, MAX_COLUMNS);
    let detail = wrap(spec.detail.as_deref().unwrap_or_default(), MAX_COLUMNS);
    if detail.is_empty() {
        return lines;
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.extend(detail);
    lines
}

/// Breaks `text` into lines of at most `columns` cells, a newline being a hard break, splitting a word longer than
/// the line. Blank lines are dropped, which keeps the single empty line in [`rendered_body`] unambiguous. The text is
/// precomposed first, or a combining accent counts as a cell of its own.
#[must_use]
pub fn wrap(text: &str, columns: usize) -> Vec<String> {
    let width = columns.max(1);
    let precomposed: String = text.nfc().collect();
    let mut lines = Vec::new();
    for paragraph in precomposed.split('\n').filter(|paragraph| !paragraph.is_empty()) {
        let mut current = String::new();
        for chunk in paragraph.split(' ').filter(|chunk| !chunk.is_empty()) {
            let mut word: Vec<char> = chunk.chars().collect();
            while word.len() > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                lines.push(word.drain(..width).collect());
            }
            if word.is_empty() {
                continue;
            }
            if current.is_empty() {
                current = word.into_iter().collect();
            } else if cell_count(&current) + 1 + word.len() <= width {
                current.push(' ');
                current.extend(word);
            } else {
                lines.push(std::mem::replace(&mut current, word.into_iter().collect()));
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines
}

/// The unit both the core and the painter count in: Unicode scalars (`${#line}` under a UTF-8 locale), not grapheme
/// clusters and not display columns.
#[must_use]
pub fn cell_count(text: &str) -> usize {
    text.chars().count()
}

/// [`cell_count`] of the precomposed text: the unit [`HudSpec::MAX_TEXT_LENGTH`] caps, on the form [`wrap`] lays out.
#[must_use]
pub fn text_length(text: &str) -> usize {
    text.nfc().count()
}

/// `size` as a whole percent of `available`, rounded up so a panel never lands a cell short of its content; `None`
/// when the pane is unmeasured.
fn percent(size: f64, available: f64) -> Option<i64> {
    if available <= 0.0 || available.is_nan() {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a saturating conversion of a rounded percent"
    )]
    let rounded = (size / available * 100.0).ceil() as i64;
    Some(rounded)
}

/// `value` truncated to whole cells, `None` below one cell.
fn whole_cells(value: f64) -> Option<usize> {
    if value.is_nan() || value < 1.0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "positive, and truncation is the intent"
    )]
    let cells = value as usize;
    Some(cells)
}

#[expect(clippy::cast_precision_loss, reason = "cell counts are far below 2^52")]
fn points(cells: usize) -> f64 {
    cells as f64
}

#[expect(clippy::cast_precision_loss, reason = "percents are far below 2^52")]
fn float(percent: i64) -> f64 {
    percent as f64
}
