//! [`TerminalElement`]: lays a terminal's grid out and paints its frame (plan > Technical Details > Frame).
//!
//! Adapted from gpui-ghostty's `TerminalElement` (<https://github.com/Xuanwo/gpui-ghostty>, Apache-2.0, Copyright
//! Xuanwo; see its LICENSE) and the Task 1 spike: the element fills its parent, the owner view sizes the grid from
//! the cell metrics and the padding, and paint draws, in Ghostty's layer order: the default background, the cell
//! backgrounds (selection and search marks included), underlines and overlines, the text, strikethroughs, the cursor
//! (block, bar, underline, hollow when unfocused; the text under a block in the cursor-text color) and the input
//! method's composition. The `background-image` (`background.rs`) lies over the default background, and the Kitty
//! placements (`kitty.rs`) in Ghostty's three layers: below the cell backgrounds, between the backgrounds and the
//! underlines and text, and above the text (under the cursor and the composition). Box drawing, block, braille,
//! powerline and legacy computing characters are not shaped: the procedural sprites (`sprite.rs`) draw them with the
//! text, as Ghostty's sprite font does. At a password prompt the cursor is Ghostty's lock.
//!
//! Text is shaped without GPUI's forced widths: each glyph is placed at the column of the cluster it belongs to,
//! keeping its offset from the cluster's first glyph (fallback fonts split a cluster's glyphs across shaped runs), so
//! wide characters, grapheme clusters and ligatures stay on the grid. Decorations follow Ghostty's sprites
//! (`src/font/sprite/draw/special.zig`, MIT) per cell.

use std::rc::Rc;
use std::sync::Arc;

use agterm_core::ghostty_config::PaddingColor;
use agterm_term::{CellWidth, Colors, Cursor, CursorStyle, Frame, KittyLayer, Row, Underline};
use gpui_kit::{
    App, BorderStyle, Bounds, ContentMask, Context, DevicePixels, Element, ElementId, Entity, FontId, GlobalElementId,
    GlyphId, Hsla, InspectorElementId, IntoElement, LayoutId, PathBuilder, Pixels, Point, RenderAlphaMaskParams, Style,
    TextRun as GpuiRun, Window, WindowTextSystem, fill, outline, point, px, relative, size,
};

use super::colors::{self, Palette, hsla};
use super::font::Fonts;
use super::kitty::Images;
use super::metrics::{CellMetrics, Geometry, Padding, points_to_px};
use super::row_cache::{Decoration, Line, RowLayout, RowMarks, SpriteCell, search_marks};
use super::sprite;
use super::{Laid, Shared, TerminalView, ViewRole};

/// One glyph of a row, placed on the grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedGlyph {
    /// The column of the glyph's cluster.
    pub col: u16,
    /// From the left of that column (logical pixels).
    pub x: Pixels,
    /// From the baseline.
    pub y: Pixels,
    pub font_id: FontId,
    pub glyph_id: GlyphId,
    pub emoji: bool,
    pub color: Hsla,
}

/// A row's glyphs for the view's font.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShapedRow {
    pub glyphs: Vec<PlacedGlyph>,
}

/// Draws a [`TerminalView`].
pub struct TerminalElement {
    view: Entity<TerminalView>,
    focused: bool,
}

impl TerminalElement {
    #[must_use]
    pub fn new(view: Entity<TerminalView>, focused: bool) -> Self {
        Self { view, focused }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// What prepaint hands to paint.
pub struct Plan {
    /// The grid's top-left corner (logical pixels).
    origin: Point<Pixels>,
    scale: f32,
    metrics: CellMetrics,
    font_size: Pixels,
    fonts: Rc<Fonts>,
    palette: Arc<Palette>,
    padding_color: PaddingColor,
    /// The theme's background, drawn before the pane has a frame.
    background: agterm_term::Rgb,
    frame: Option<Arc<Frame>>,
    preedit: Option<String>,
    /// The program on the PTY reads a password (`Pane::password_input`): the cursor is the lock.
    password_input: bool,
    /// The background image and the Kitty placements.
    images: Images,
}

impl Plan {
    /// A device-pixel length in logical pixels.
    fn px(&self, device: f64) -> Pixels {
        #[expect(clippy::cast_possible_truncation, reason = "pixel lengths fit an f32")]
        let logical = (device / f64::from(self.scale)) as f32;
        px(logical)
    }

    fn cell_width(&self) -> Pixels {
        self.px(f64::from(self.metrics.cell_width))
    }

    fn cell_height(&self) -> Pixels {
        self.px(f64::from(self.metrics.cell_height))
    }

    /// The top-left corner of a cell.
    fn cell(&self, col: u16, row: usize) -> Point<Pixels> {
        #[expect(clippy::cast_precision_loss, reason = "row indices are far below 2^24")]
        let row = row as f32;
        point(
            self.origin.x + self.cell_width() * f32::from(col),
            self.origin.y + self.cell_height() * row,
        )
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<Plan>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.view.update(cx, |view, cx| view.prepaint(bounds, window, cx))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        plan: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(plan) = plan.as_ref() else {
            return;
        };
        let focused = self.focused;
        // a viewer smaller than the pane's grid shows its top-left part
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.view
                .update(cx, |view, _| view.paint(bounds, plan, focused, window));
        });
    }
}

/// The view's side of the element.
impl TerminalView {
    fn prepaint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> Option<Plan> {
        let shared = Rc::clone(&self.shared);
        if shared.is_torn_down() {
            self.cache.clear();
            self.painted = None;
            self.release_images(window);
            return None;
        }
        let scale = window.scale_factor();
        let points = self.font_points();
        let metrics = shared.metrics_at(points, scale);
        let config = shared.config();
        let explicit = Padding::from_config(&config.padding, scale);
        let screen = (
            device_from(f32::from(bounds.size.width), scale),
            device_from(f32::from(bounds.size.height), scale),
        );
        let padding = match self.role {
            ViewRole::Owner => lay_out(&shared, screen, scale, &metrics, explicit, cx),
            ViewRole::Viewer => explicit,
        };
        let fonts = shared.fonts();
        let font_size = px(points_to_px(points));
        let frame = shared.pane().map(|pane| {
            let snapshot = pane.frame();
            if snapshot.stale {
                // drawn from the previous frame: a lock was busy, so ask for another
                window.request_animation_frame();
            }
            let marks = self.marks(&snapshot.frame, &pane.search_state());
            let key = (points.to_bits(), scale.to_bits(), shared.font_generation());
            self.cache.update(&snapshot.frame, &config.palette, &marks, key);
            shape_missing(&mut self.cache, &fonts, font_size, window.text_system());
            snapshot.frame
        });
        self.painted.clone_from(&frame);
        let images = self.prepare_images(frame.as_deref(), &metrics, window, cx);
        let origin = bounds.origin + point(px(device_to(padding.left, scale)), px(device_to(padding.top, scale)));
        Some(Plan {
            origin,
            scale,
            metrics,
            font_size,
            fonts,
            palette: Arc::clone(&config.palette),
            padding_color: config.padding.color,
            background: config.theme.background,
            frame,
            preedit: shared.preedit(),
            password_input: shared.pane().is_some_and(|pane| pane.password_input()),
            images,
        })
    }

    /// Each row's marks: the selection, the search matches, the hovered link.
    fn marks(&self, frame: &Frame, search: &agterm_term::SearchState) -> Vec<RowMarks> {
        let mut search = search_marks(search, frame.rows.len(), frame.cols);
        frame
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| RowMarks {
                selection: row.selection(),
                search: std::mem::take(&mut search[index]),
                link: self
                    .link
                    .filter(|(link_row, _, _)| usize::from(*link_row) == index)
                    .map(|(_, start, end)| (start, end)),
            })
            .collect()
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, plan: &Plan, focused: bool, window: &mut Window) {
        let background = plan
            .frame
            .as_ref()
            .map_or(plan.background, |frame| frame.colors.background);
        window.paint_quad(fill(bounds, hsla(background, plan.palette.background_alpha)));
        if let Some(image) = plan.images.background.as_ref() {
            image.paint(window, bounds, plan.scale);
        }
        let Some(frame) = plan.frame.as_ref() else {
            return;
        };
        let kitty = &plan.images.kitty;
        kitty.paint(window, KittyLayer::BelowBackground, plan.origin, plan.scale);
        let extend = Extend::new(plan.padding_color, frame);
        for (index, row) in self.cache.rows().enumerate() {
            paint_backgrounds(
                window,
                plan,
                bounds,
                index,
                &row.layout,
                extend.for_row(index, frame.rows.len()),
            );
        }
        kitty.paint(window, KittyLayer::BelowText, plan.origin, plan.scale);
        for (index, row) in self.cache.rows().enumerate() {
            paint_decorations(window, plan, index, &row.layout, |line| {
                !matches!(line, Line::Strikethrough)
            });
        }
        for (index, row) in self.cache.rows().enumerate() {
            if let Some(shaped) = row.shaped.as_ref() {
                paint_glyphs(window, plan, index, &shaped.glyphs, None);
            }
            paint_sprites(window, plan, index, &row.layout.sprites, None);
            paint_decorations(window, plan, index, &row.layout, |line| {
                matches!(line, Line::Strikethrough)
            });
        }
        kitty.paint(window, KittyLayer::AboveText, plan.origin, plan.scale);
        if plan.preedit.is_none() {
            self.paint_cursor(window, plan, frame, focused);
        }
        if let Some(preedit) = plan.preedit.as_deref() {
            paint_preedit(window, plan, frame, preedit);
        }
        self.shared.drew();
    }

    fn paint_cursor(&self, window: &mut Window, plan: &Plan, frame: &Frame, focused: bool) {
        // a password prompt shows the lock whatever the cursor's style, visibility, blink or focus
        let cursor = Cursor {
            password_input: frame.cursor.password_input || plan.password_input,
            ..frame.cursor
        };
        let (style, col, row) = if let Some((col, row)) = lock_cursor(&cursor) {
            (None, col, row)
        } else {
            let Some((style, col, row)) = cursor_shape(&frame.cursor, focused, self.blink_on) else {
                return;
            };
            (Some(style), col, row)
        };
        let cells = frame.rows.get(usize::from(row)).map(|row| row.cells());
        let cell = cells.and_then(|cells| cells.get(usize::from(col)));
        let wide = cell.is_some_and(|cell| cell.width == CellWidth::Wide);
        let under = cell.filter(|_| !frame.rows[usize::from(row)].text(usize::from(col)).is_empty());
        let color = colors::cursor_color(under, &frame.colors, &plan.palette);
        let alpha = plan.palette.cursor_alpha;
        let Some(style) = style else {
            paint_lock(window, plan, (col, usize::from(row)), wide, hsla(color, alpha));
            return;
        };
        paint_cursor_shape(window, plan, style, (col, usize::from(row)), wide, hsla(color, alpha));
        if style == CursorStyle::Block
            && let Some(cached) = self.cache.row(usize::from(row))
        {
            let text = Some((
                col,
                hsla(colors::cursor_text(under, &frame.colors, &plan.palette), u8::MAX),
            ));
            if let Some(shaped) = cached.shaped.as_ref() {
                paint_glyphs(window, plan, usize::from(row), &shaped.glyphs, text);
            }
            paint_sprites(window, plan, usize::from(row), &cached.layout.sprites, text);
        }
    }
}

/// The owner sizes the grid to its `screen` (device pixels) and gives the pane that size, starting it the first time
/// (deferred out of prepaint). The padding the grid sits in.
fn lay_out(
    shared: &Rc<Shared>,
    screen: (u32, u32),
    scale: f32,
    metrics: &CellMetrics,
    explicit: Padding,
    cx: &mut App,
) -> Padding {
    let geometry = Geometry::new(screen, metrics, explicit, shared.config().padding.balance);
    if screen.0 == 0 || screen.1 == 0 {
        return geometry.padding;
    }
    let laid = Laid {
        grid: agterm_term::GridSize {
            cols: geometry.cols,
            rows: geometry.rows,
            cell_width: metrics.cell_width,
            cell_height: metrics.cell_height,
        },
        padding: geometry.padding,
    };
    if shared.lay_out(scale, laid) {
        let spawning = Rc::clone(shared);
        cx.defer(move |_| spawning.spawn());
    }
    geometry.padding
}

/// Logical pixels to whole device pixels (floored, as a surface's pixel size is).
fn device_from(logical: f32, scale: f32) -> u32 {
    let device = (logical * scale).floor().max(0.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a non-negative view size"
    )]
    let device = device as u32;
    device
}

/// Device pixels to logical pixels.
fn device_to(device: u32, scale: f32) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "pixel counts are far below 2^24")]
    let device = device as f32;
    device / scale
}

/// Shapes the rows the cache rebuilt (or whose font changed).
fn shape_missing(
    cache: &mut super::RowCache<ShapedRow, (u32, u32, u64)>,
    fonts: &Fonts,
    font_size: Pixels,
    text: &WindowTextSystem,
) {
    for row in cache.rows_mut() {
        if row.shaped.is_none() {
            row.shaped = Some(shape_row(&row.layout, fonts, font_size, text));
        }
    }
}

/// Shapes every run of `layout` and places its glyphs at their clusters' columns.
#[must_use]
pub fn shape_row(layout: &RowLayout, fonts: &Fonts, font_size: Pixels, text: &WindowTextSystem) -> ShapedRow {
    let mut glyphs = Vec::new();
    for run in &layout.runs {
        let color = hsla(run.style.color, run.style.alpha);
        let gpui_run = GpuiRun {
            len: run.text.len(),
            font: fonts.style(run.style.bold, run.style.italic).clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = text.layout_line(&run.text, font_size, &[gpui_run], None);
        // the first glyph of each cluster anchors the others, across the shaped runs fallback fonts split it into
        let mut anchors: Vec<Option<Pixels>> = vec![None; run.clusters.len()];
        for shaped in &line.runs {
            for glyph in &shaped.glyphs {
                let cluster = run.cluster_of(glyph.index);
                let anchor = *anchors[cluster].get_or_insert(glyph.position.x);
                glyphs.push(PlacedGlyph {
                    col: run.clusters[cluster].col,
                    x: glyph.position.x - anchor,
                    y: glyph.position.y,
                    font_id: shaped.font_id,
                    glyph_id: glyph.id,
                    emoji: glyph.is_emoji,
                    color,
                });
            }
        }
    }
    ShapedRow { glyphs }
}

/// Which edges of a row's backgrounds reach into the padding (`window-padding-color`).
#[derive(Debug, Clone, Copy, Default)]
struct Edges {
    /// Left and right.
    sides: bool,
    up: bool,
    down: bool,
}

/// `window-padding-color = extend`/`extend-always` for a frame: Ghostty extends every row sideways, and the first and
/// last rows up and down, except (`extend`) a row that would look wrong extended.
struct Extend {
    sideways: bool,
    up: bool,
    down: bool,
}

impl Extend {
    fn new(color: PaddingColor, frame: &Frame) -> Self {
        let edge = |row: Option<&Arc<Row>>| match color {
            PaddingColor::Background => false,
            PaddingColor::ExtendAlways => true,
            PaddingColor::Extend => row.is_some_and(|row| !never_extend(row, &frame.colors)),
        };
        Self {
            sideways: color != PaddingColor::Background,
            up: edge(frame.rows.first()),
            down: edge(frame.rows.last()),
        }
    }

    fn for_row(&self, index: usize, rows: usize) -> Edges {
        Edges {
            sides: self.sideways,
            up: self.up && index == 0,
            down: self.down && index + 1 == rows,
        }
    }
}

/// Ghostty's `neverExtendBg`: a row with a default background cell (it looks fine as it is) or a powerline glyph
/// (drawn to fit its cell). Ghostty also never extends a prompt row, which a frame does not mark.
fn never_extend(row: &Row, colors: &Colors) -> bool {
    row.cells().iter().enumerate().any(|(col, cell)| {
        let powerline = row
            .text(col)
            .chars()
            .next()
            .is_some_and(|ch| matches!(u32::from(ch), 0xE0B0..=0xE0C8 | 0xE0CA | 0xE0CC..=0xE0D2 | 0xE0D4));
        powerline || cell.background.is_none_or(|bg| bg == colors.background)
    })
}

fn paint_backgrounds(
    window: &mut Window,
    plan: &Plan,
    bounds: Bounds<Pixels>,
    row: usize,
    layout: &RowLayout,
    edges: Edges,
) {
    let cols = row_cols(plan);
    for span in &layout.backgrounds {
        let top_left = plan.cell(span.start, row);
        let bottom_right = plan.cell(span.start + span.len, row + 1);
        let left = if edges.sides && span.start == 0 {
            bounds.left()
        } else {
            top_left.x
        };
        let right = if edges.sides && span.start + span.len >= cols {
            bounds.right()
        } else {
            bottom_right.x
        };
        let top = if edges.up { bounds.top() } else { top_left.y };
        let bottom = if edges.down { bounds.bottom() } else { bottom_right.y };
        let area = Bounds::from_corners(point(left, top), point(right, bottom));
        window.paint_quad(fill(area, hsla(span.value, u8::MAX)));
    }
}

/// The frame's width in columns.
fn row_cols(plan: &Plan) -> u16 {
    plan.frame.as_ref().map_or(0, |frame| frame.cols)
}

fn paint_glyphs(window: &mut Window, plan: &Plan, row: usize, glyphs: &[PlacedGlyph], only: Option<(u16, Hsla)>) {
    let baseline = plan.px(f64::from(plan.metrics.baseline_from_top()));
    for glyph in glyphs {
        let color = match only {
            Some((col, color)) if glyph.col == col => color,
            Some(_) => continue,
            None => glyph.color,
        };
        let cell = plan.cell(glyph.col, row);
        let origin = point(cell.x + glyph.x, cell.y + baseline + glyph.y);
        let painted = if glyph.emoji {
            window.paint_emoji(origin, glyph.font_id, glyph.glyph_id, plan.font_size)
        } else {
            window.paint_glyph(origin, glyph.font_id, glyph.glyph_id, plan.font_size, color)
        };
        if let Err(err) = painted {
            tracing::debug!(%err, "a terminal glyph was not painted");
        }
    }
}

/// The atlas owner of the sprites' masks (`RenderAlphaMaskParams::owner`).
const SPRITE_OWNER: &str = "agterm.terminal.sprite";

/// A row's sprite cells, rasterized for the view's metrics (once per thread) and painted at whole device pixels in
/// their foreground color; with `only`, just the cell at that column, in that color (the text under a block cursor).
fn paint_sprites(window: &mut Window, plan: &Plan, row: usize, cells: &[SpriteCell], only: Option<(u16, Hsla)>) {
    for cell in cells {
        let color = match only {
            Some((col, color)) if cell.col == col => color,
            Some(_) => continue,
            None => hsla(cell.color, cell.alpha),
        };
        let Some(sprite) = sprite::sprite(cell.codepoint, &plan.metrics, u32::from(cell.cells)) else {
            continue;
        };
        let corner = plan.cell(cell.col, row);
        let origin = point(
            corner.x + plan.px(f64::from(sprite.mask.left)),
            corner.y + plan.px(f64::from(sprite.mask.top)),
        );
        let params = RenderAlphaMaskParams {
            owner: SPRITE_OWNER,
            id: sprite.id,
        };
        let mask_size = size(device_pixels(sprite.mask.width), device_pixels(sprite.mask.height));
        let painted = window.paint_alpha_mask(origin, params, color, || Some((mask_size, sprite.mask.alpha.clone())));
        if let Err(err) = painted {
            tracing::debug!(%err, "a terminal sprite was not painted");
        }
    }
}

fn device_pixels(value: u32) -> DevicePixels {
    DevicePixels(i32::try_from(value).unwrap_or(i32::MAX))
}

/// The lock Ghostty draws for the cursor at a password prompt (`Cursor::password_input`): the cell, when the cursor is
/// in the viewport. It shows whatever the cursor's style, visibility, blink or the view's focus.
pub(super) fn lock_cursor(cursor: &Cursor) -> Option<(u16, u16)> {
    let position = cursor.position?;
    if !cursor.password_input {
        return None;
    }
    let col = if position.wide_tail {
        position.column.saturating_sub(1)
    } else {
        position.column
    };
    Some((col, position.row))
}

/// Nerd Font's lock (`nf-fa-lock`), the glyph Ghostty draws: the embedded `Symbols Nerd Font Mono` has it.
const LOCK: &str = "\u{f023}";

/// The lock in the cursor's color, centered in the cursor's cell (two for a wide character).
fn paint_lock(window: &mut Window, plan: &Plan, (col, row): (u16, usize), wide: bool, color: Hsla) {
    let run = GpuiRun {
        len: LOCK.len(),
        font: plan.fonts.regular.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let text = window.text_system().clone();
    let line = text.layout_line(LOCK, plan.font_size, &[run], None);
    let cell = plan.cell(col, row);
    let width = plan.cell_width() * if wide { 2.0 } else { 1.0 };
    let left = cell.x + (width - line.width).max(px(0.0)) / 2.0;
    let baseline = plan.px(f64::from(plan.metrics.baseline_from_top()));
    for shaped in &line.runs {
        for glyph in &shaped.glyphs {
            let origin = point(left + glyph.position.x, cell.y + baseline + glyph.position.y);
            if let Err(err) = window.paint_glyph(origin, shaped.font_id, glyph.id, plan.font_size, color) {
                tracing::debug!(%err, "the lock cursor was not painted");
            }
        }
    }
}

fn paint_decorations(window: &mut Window, plan: &Plan, row: usize, layout: &RowLayout, which: impl Fn(Line) -> bool) {
    for span in layout.decorations.iter().filter(|span| which(span.value.line)) {
        for col in span.start..span.start + span.len {
            paint_decoration(window, plan, row, col, span.value);
        }
    }
}

/// One cell's decoration, as Ghostty's sprites draw it (device pixels; a sprite canvas reaches a quarter cell below).
fn paint_decoration(window: &mut Window, plan: &Plan, row: usize, col: u16, decoration: Decoration) {
    let m = &plan.metrics;
    let color = hsla(decoration.color, decoration.alpha);
    let below = f64::from(m.cell_height) + f64::from(m.cell_height / 4);
    let cell = plan.cell(col, row);
    let rect = |y: f64, height: u32, window: &mut Window| {
        let area = Bounds::new(
            point(cell.x, cell.y + plan.px(y)),
            size(plan.cell_width(), plan.px(f64::from(height))),
        );
        window.paint_quad(fill(area, color));
    };
    let thickness = f64::from(m.underline_thickness);
    let underline_y = |lines: f64| f64::from(m.underline_position).min(below - lines * thickness);
    match decoration.line {
        Line::Underline(Underline::None) => {}
        Line::Underline(Underline::Single) => rect(underline_y(1.0), m.underline_thickness, window),
        Line::Underline(Underline::Double) => {
            let y = underline_y(2.0);
            rect((y - thickness).max(0.0), m.underline_thickness, window);
            rect(y + thickness, m.underline_thickness, window);
        }
        Line::Underline(Underline::Dotted) => paint_dots(window, plan, cell, below, color),
        Line::Underline(Underline::Dashed) => paint_dashes(window, plan, cell, underline_y(1.0), color),
        Line::Underline(Underline::Curly) => paint_curl(window, plan, cell, below, color),
        Line::Strikethrough => rect(f64::from(m.strikethrough_position), m.strikethrough_thickness, window),
        Line::Overline => rect(
            f64::from(m.overline_position).max(-f64::from(m.cell_height / 4)),
            m.overline_thickness,
            window,
        ),
    }
}

/// `underline_dotted`: round dots spread evenly over the cell.
fn paint_dots(window: &mut Window, plan: &Plan, cell: Point<Pixels>, below: f64, color: Hsla) {
    let m = &plan.metrics;
    let width = f64::from(m.cell_width);
    let thickness = f64::from(m.underline_thickness);
    let radius = std::f64::consts::FRAC_1_SQRT_2 * thickness;
    let y = (f64::from(m.underline_position) + 0.5 * thickness).min(below - radius.ceil());
    let count = (width / (4.0 * radius))
        .ceil()
        .min((width / (3.0 * radius)).floor())
        .min((width / (2.0 * radius + 1.0)).floor())
        .max(1.0);
    let step = width / count;
    let mut x = step / 2.0;
    while x < width {
        let dot = Bounds::new(
            point(cell.x + plan.px(x - radius), cell.y + plan.px(y - radius)),
            size(plan.px(2.0 * radius), plan.px(2.0 * radius)),
        );
        window.paint_quad(fill(dot, color).corner_radii(plan.px(radius)));
        x += step;
    }
}

/// `underline_dashed`: every other third of the cell.
fn paint_dashes(window: &mut Window, plan: &Plan, cell: Point<Pixels>, y: f64, color: Hsla) {
    let m = &plan.metrics;
    let dash = m.cell_width / 3 + 1;
    let count = m.cell_width / dash + 1;
    for index in (0..count).step_by(2) {
        let x = index * dash;
        let width = dash.min(m.cell_width.saturating_sub(x));
        let area = Bounds::new(
            point(cell.x + plan.px(f64::from(x)), cell.y + plan.px(y)),
            size(plan.px(f64::from(width)), plan.px(f64::from(m.underline_thickness))),
        );
        window.paint_quad(fill(area, color));
    }
}

/// `underline_curly`: one wave per cell, two cubic curves stroked at the underline thickness.
fn paint_curl(window: &mut Window, plan: &Plan, cell: Point<Pixels>, below: f64, color: Hsla) {
    let m = &plan.metrics;
    let width = f64::from(m.cell_width);
    let thickness = f64::from(m.underline_thickness);
    let amplitude = width / std::f64::consts::PI;
    let top = f64::from(m.underline_position).min(below - amplitude - thickness);
    let bottom = top + amplitude;
    let center = width / 2.0;
    let at = |x: f64, y: f64| point(cell.x + plan.px(x), cell.y + plan.px(y));
    let mut path = PathBuilder::stroke(plan.px(thickness));
    path.move_to(at(0.0, bottom));
    path.cubic_bezier_to(at(center, top), at(center * 0.4, bottom), at(center * 0.6, top));
    path.cubic_bezier_to(
        at(width, bottom),
        at(center * 1.4, top),
        at(width - center * 0.4, bottom),
    );
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// The cursor's visual style and cell, or `None` when it is not drawn (Ghostty's `renderer/cursor.zig`): hidden by
/// the program, scrolled out, in the off phase of its blink; a hollow block in an unfocused view.
pub(super) fn cursor_shape(cursor: &Cursor, focused: bool, blink_on: bool) -> Option<(CursorStyle, u16, u16)> {
    let position = cursor.position?;
    if !cursor.visible {
        return None;
    }
    let col = if position.wide_tail {
        position.column.saturating_sub(1)
    } else {
        position.column
    };
    if !focused {
        return Some((CursorStyle::BlockHollow, col, position.row));
    }
    if cursor.blinking && !blink_on {
        return None;
    }
    Some((cursor.style, col, position.row))
}

/// Ghostty's cursor sprites (`cursor_rect`, `cursor_hollow_rect`, `cursor_bar`, `cursor_underline`), two cells wide
/// on a wide character.
fn paint_cursor_shape(
    window: &mut Window,
    plan: &Plan,
    style: CursorStyle,
    (col, row): (u16, usize),
    wide: bool,
    color: Hsla,
) {
    let m = &plan.metrics;
    let cell = plan.cell(col, row);
    let width = plan.cell_width() * if wide { 2.0 } else { 1.0 };
    let height = plan.cell_height();
    let thickness = plan.px(f64::from(m.cursor_thickness));
    let area = match style {
        CursorStyle::Block => Bounds::new(cell, size(width, height)),
        CursorStyle::BlockHollow => {
            let frame = outline(Bounds::new(cell, size(width, height)), color, BorderStyle::default());
            window.paint_quad(frame.border_widths(thickness));
            return;
        }
        CursorStyle::Bar => {
            let shift = plan.px(f64::from(m.cursor_thickness.div_ceil(2)));
            Bounds::new(point(cell.x - shift, cell.y), size(thickness, height))
        }
        CursorStyle::Underline => {
            let below = f64::from(m.cell_height) + f64::from(m.cell_height / 4);
            let y = f64::from(m.underline_position).min(below - f64::from(m.underline_thickness));
            Bounds::new(point(cell.x, cell.y + plan.px(y)), size(width, thickness))
        }
    };
    window.paint_quad(fill(area, color));
}

/// The input method's composition over the cursor's cell: the default colors, underlined.
fn paint_preedit(window: &mut Window, plan: &Plan, frame: &Frame, preedit: &str) {
    let Some(position) = frame.cursor.position else {
        return;
    };
    let colors = &frame.colors;
    let text = window.text_system().clone();
    let run = GpuiRun {
        len: preedit.len(),
        font: plan.fonts.regular.clone(),
        color: hsla(colors.foreground, u8::MAX),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = text.layout_line(preedit, plan.font_size, &[run], None);
    let cell = plan.cell(position.column, usize::from(position.row));
    let width = line.width.max(plan.cell_width());
    let area = Bounds::new(cell, size(width, plan.cell_height()));
    window.paint_quad(fill(area, hsla(colors.background, u8::MAX)));
    let baseline = plan.px(f64::from(plan.metrics.baseline_from_top()));
    for shaped in &line.runs {
        for glyph in &shaped.glyphs {
            let origin = point(cell.x + glyph.position.x, cell.y + baseline + glyph.position.y);
            let _ = if glyph.is_emoji {
                window.paint_emoji(origin, shaped.font_id, glyph.id, plan.font_size)
            } else {
                window.paint_glyph(
                    origin,
                    shaped.font_id,
                    glyph.id,
                    plan.font_size,
                    hsla(colors.foreground, u8::MAX),
                )
            };
        }
    }
    let underline = plan.px(f64::from(plan.metrics.underline_thickness));
    let y = cell.y + plan.px(f64::from(plan.metrics.underline_position));
    window.paint_quad(fill(
        Bounds::new(point(cell.x, y), size(width, underline)),
        hsla(colors.foreground, u8::MAX),
    ));
}
