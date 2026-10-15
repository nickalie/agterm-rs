//! One terminal: an `agterm-term` pane drawn by GPUI, the core's [`agterm_host::Surface`] over it, and the views that
//! show it (plan > Task 13; Technical Details > Frame, Surface).
//!
//! - [`Terminal`] (a GPUI entity) owns the pane's pump: one foreground task woken through a one-token channel by the
//!   pane's IO thread (`PaneConfig::wake`) and by the Surface. Each wake drains the pane's events (re-emitted as
//!   [`TerminalEvent::Pane`]), runs the queued work that needs the app (the clipboard, events), and notifies the views.
//! - [`Shared`] is the plain main-thread state the entity, its views and the [`TerminalSurface`] share: the pane once
//!   spawned, the live font size and cell metrics (computed synchronously through the app's text system, so the core
//!   reads the right font and cell size right after an action), the owner's scale factor and grid, the queued work.
//!   The Surface has no GPUI context (the core calls it inside main-loop tasks): what it cannot do without one is
//!   queued and the pump does it on the next foreground tick, after the core's task; binding actions are accepted for a
//!   realized terminal, as the GTK build's were.
//! - [`TerminalView`] (an entity per view) draws the pane's shared frame with its own row cache. The [`ViewRole::Owner`]
//!   view lays the grid out, starts the pane at its first nonzero size (deferred out of prepaint, as GTK creates its
//!   surface on a tick once the area has a size) and resizes it; a [`ViewRole::Viewer`] (the dashboard's second
//!   rendering) only draws, at its own font size, and never spawns, resizes or takes input.
//!
//! Teardown drops the pane (its child gets `SIGHUP`) and closes the pump, whatever views still hold the state.

pub mod background;
mod colors;
mod element;
mod font;
pub mod kitty;
mod metrics;
mod row_cache;
pub mod sprite;
mod surface;

#[cfg(test)]
mod kitty_tests;
#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agterm_core::ghostty_config::{self, GhosttyConfig};
use agterm_term::{
    ClipboardLocation, CursorDefaults, Frame, GridSize, Pane, PaneConfig, PaneError, PaneEvent, Paste, PasteSource,
    ScrollbackLimits, ThemeColors,
};
use gpui_kit::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Task, TextSystem, Window, div, px,
};

pub use colors::{CellPaint, Mark, Palette, Pick};
pub use element::{Layer as PaintLayer, PlacedGlyph, ShapedRow, TerminalElement};
pub use font::{DEFAULT_FAMILIES, DEFAULT_FAMILY, Fonts, SYMBOLS_FAMILY, register_embedded as register_embedded_fonts};
pub use metrics::{CellMetrics, DEFAULT_DPI, FaceMetrics, Geometry, Padding, points_to_px};
pub use row_cache::{Cluster, Decoration, Line, RowCache, RowLayout, RowMarks, Span, SpriteCell, TextRun, TextStyle};
pub use surface::TerminalSurface;

/// What a terminal draws and starts with, from the ghostty config.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalConfig {
    pub font: ghostty_config::Font,
    pub padding: ghostty_config::WindowPadding,
    pub palette: Arc<Palette>,
    pub theme: ThemeColors,
    pub cursor: CursorDefaults,
    pub scrollback: ScrollbackLimits,
    /// `background-image*`, drawn behind the grid.
    pub background_image: ghostty_config::BackgroundImage,
}

impl TerminalConfig {
    #[must_use]
    pub fn from_ghostty(config: &GhosttyConfig) -> Self {
        Self {
            font: config.font.clone(),
            padding: config.window_padding,
            palette: Arc::new(Palette::from_config(config)),
            theme: ThemeColors::from_config(&config.colors, &config.cursor),
            cursor: CursorDefaults::from_config(&config.cursor),
            scrollback: config.scrollback.into(),
            background_image: config.background_image.clone(),
        }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self::from_ghostty(&GhosttyConfig::default())
    }
}

/// Who a terminal is (GTK's `SurfaceSpec` identity part).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerminalSpec {
    /// Baked into the shell as `AGTERM_PANE_ID`; empty for a terminal with no pane identity.
    pub token: String,
    /// Attached to a live-session daemon.
    pub live: bool,
    /// The point size the terminal starts at, which `reset_font_size` returns to; `None` is the config's.
    pub font_size: Option<f32>,
}

/// Starts the pane (Task 18 passes `Pane::launch` with the session's launch); called once, with the size of the grid
/// the owner view first laid out and the wake that reaches this terminal's pump.
pub type Spawn = Box<dyn FnOnce(PaneConfig) -> Result<Pane, PaneError>>;

/// What a terminal tells its frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// The pane reported this, in stream order.
    Pane(PaneEvent),
    /// The pane started (GTK's surface "created").
    Spawned,
    /// The pane could not start.
    SpawnFailed(String),
    /// The live font size changed, and the cell with it (GTK's font-size and cell-size reports): read them from
    /// [`Shared::font_points`] and [`Shared::cell_size`].
    FontSize,
    /// A `start_search` binding: the frontend opens its search UI.
    StartSearch,
    /// An `end_search` binding ended the search.
    EndSearch,
    /// A paste was refused as unsafe (it could run a command): the frontend confirms and pastes it again.
    UnsafePaste(String),
}

/// Work that needs the app, queued by the Surface for the pump.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Effect {
    Paste(ClipboardLocation),
    Copy(String, ClipboardLocation),
    Emit(TerminalEvent),
}

/// Where a terminal is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Waiting for the owner view's first grid.
    Pending,
    /// The spawn is queued.
    Spawning,
    Live,
    Failed,
    TornDown,
}

/// The surface key of the next terminal (the `key` field of the `agterm::surface` log lines).
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// The size the owner view gave the pane last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Laid {
    grid: GridSize,
    padding: Padding,
}

/// The state a terminal, its views and its Surface share on the main thread (module docs).
pub struct Shared {
    key: u64,
    spec: TerminalSpec,
    config: RefCell<Rc<TerminalConfig>>,
    text: Arc<TextSystem>,
    fonts: RefCell<Option<Rc<Fonts>>>,
    /// Bumped when the fonts change, keying every view's shaping.
    font_generation: Cell<u64>,
    pane: RefCell<Option<Rc<Pane>>>,
    spawn: RefCell<Option<Spawn>>,
    status: Cell<Status>,
    /// The live font size in points.
    points: Cell<f32>,
    /// What `reset_font_size` returns to.
    original_points: f32,
    /// The owner view's scale factor, last seen.
    scale: Cell<f32>,
    metrics: Cell<Option<(u32, u32, u64, CellMetrics)>>,
    laid: Cell<Option<Laid>>,
    effects: RefCell<Vec<Effect>>,
    wake: async_channel::Sender<()>,
    /// The first frame was drawn (logged once).
    drew: Cell<bool>,
    /// The owner view's input-method composition, shown at the cursor (Task 14 drives it).
    preedit: RefCell<Option<String>>,
    /// The `background-image` every view draws, loaded once per terminal.
    background: RefCell<background::Loader>,
    /// The Kitty images as textures, converted off the main thread for every view.
    kitty: RefCell<kitty::KittyStore>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("key", &self.key)
            .field("status", &self.status.get())
            .finish_non_exhaustive()
    }
}

impl Shared {
    /// The surface key of the `agterm::surface` log lines.
    #[must_use]
    pub fn key(&self) -> u64 {
        self.key
    }

    #[must_use]
    pub fn spec(&self) -> &TerminalSpec {
        &self.spec
    }

    #[must_use]
    pub fn config(&self) -> Rc<TerminalConfig> {
        Rc::clone(&self.config.borrow())
    }

    /// The pane, once started and until torn down.
    #[must_use]
    pub fn pane(&self) -> Option<Rc<Pane>> {
        self.pane.borrow().clone()
    }

    /// The pane exists: started and not torn down (GTK's `is_realized`).
    #[must_use]
    pub fn is_realized(&self) -> bool {
        self.status.get() == Status::Live
    }

    #[must_use]
    pub fn is_torn_down(&self) -> bool {
        self.status.get() == Status::TornDown
    }

    /// The live font size in points.
    #[must_use]
    pub fn font_points(&self) -> f32 {
        self.points.get()
    }

    /// The size `reset_font_size` returns to.
    #[must_use]
    pub fn original_font_points(&self) -> f32 {
        self.original_points
    }

    /// The owner view's scale factor, as last laid out (1 before).
    #[must_use]
    pub fn scale(&self) -> f32 {
        self.scale.get()
    }

    /// The fonts of the config, resolved against the installed families once per config.
    #[must_use]
    pub fn fonts(&self) -> Rc<Fonts> {
        if let Some(fonts) = self.fonts.borrow().as_ref() {
            return Rc::clone(fonts);
        }
        let installed = self.text.all_font_names();
        let fonts = Rc::new(Fonts::resolve(&self.config.borrow().font, &|family| {
            installed.binary_search_by(|name| name.as_str().cmp(family)).is_ok()
        }));
        *self.fonts.borrow_mut() = Some(Rc::clone(&fonts));
        fonts
    }

    /// The generation of the fonts, bumped when they change.
    #[must_use]
    pub fn font_generation(&self) -> u64 {
        self.font_generation.get()
    }

    /// The cell metrics at `points` on a display of `scale`, cached for the last size asked.
    #[must_use]
    pub fn metrics_at(&self, points: f32, scale: f32) -> CellMetrics {
        let key = (points.to_bits(), scale.to_bits(), self.font_generation.get());
        if let Some((p, s, g, metrics)) = self.metrics.get()
            && (p, s, g) == key
        {
            return metrics;
        }
        let fonts = self.fonts();
        let font_id = self.text.resolve_font(&fonts.regular);
        let metrics = metrics::cell_metrics(&self.text, font_id, points, scale);
        self.metrics.set(Some((key.0, key.1, key.2, metrics)));
        metrics
    }

    /// The live cell metrics, at the owner's scale.
    #[must_use]
    pub fn metrics(&self) -> CellMetrics {
        self.metrics_at(self.points.get(), self.scale.get())
    }

    /// The live cell in logical pixels (GTK's `cell_size`: device pixels over the scale factor).
    #[must_use]
    pub fn cell_size(&self) -> (f64, f64) {
        let metrics = self.metrics();
        let scale = f64::from(self.scale.get().max(1.0));
        (
            f64::from(metrics.cell_width) / scale,
            f64::from(metrics.cell_height) / scale,
        )
    }

    /// The composition shown at the cursor.
    #[must_use]
    pub fn preedit(&self) -> Option<String> {
        self.preedit.borrow().clone()
    }

    /// Sets the composition shown at the cursor (the owner view's input method, Task 14).
    pub fn set_preedit(&self, preedit: Option<String>) {
        *self.preedit.borrow_mut() = preedit.filter(|text| !text.is_empty());
        self.poke();
    }

    /// Wakes the pump: the queued work runs and the views redraw.
    pub fn poke(&self) {
        let _ = self.wake.try_send(());
    }

    fn queue(&self, effect: Effect) {
        if self.is_torn_down() {
            return;
        }
        self.effects.borrow_mut().push(effect);
        self.poke();
    }

    /// Changes the live font size (clamped to Ghostty's 1–255 points): the metrics follow now, the views on the next
    /// frame. Whether it changed.
    pub fn set_font_points(&self, points: f32) -> bool {
        let points = points.clamp(1.0, 255.0);
        if (points - self.points.get()).abs() < f32::EPSILON {
            return false;
        }
        self.points.set(points);
        let _ = self.metrics();
        self.queue(Effect::Emit(TerminalEvent::FontSize));
        true
    }

    /// The owner view laid the grid out at `scale`: record it, resize a live pane, and report whether the pane waits
    /// for its first size (the caller defers [`Shared::spawn`]).
    fn lay_out(&self, scale: f32, laid: Laid) -> bool {
        self.scale.set(scale);
        if self.laid.get() == Some(laid) {
            return false;
        }
        self.laid.set(Some(laid));
        match self.status.get() {
            Status::Pending => {
                self.status.set(Status::Spawning);
                true
            }
            Status::Live => {
                if let Some(pane) = self.pane()
                    && let Err(err) = resize(&pane, laid)
                {
                    tracing::warn!(target: "agterm::surface", key = self.key, %err, "terminal resize failed");
                }
                false
            }
            Status::Spawning | Status::Failed | Status::TornDown => false,
        }
    }

    /// Starts the pane at the grid the owner laid out last; once.
    fn spawn(&self) {
        if self.status.get() != Status::Spawning {
            return;
        }
        let (Some(spawn), Some(laid)) = (self.spawn.borrow_mut().take(), self.laid.get()) else {
            return;
        };
        let config = self.config();
        let wake = self.wake.clone();
        let pane_config = PaneConfig {
            scrollback: config.scrollback,
            colors: config.theme.clone(),
            cursor: config.cursor,
            wake: Some(Arc::new(move || {
                let _ = wake.try_send(());
            })),
            ..PaneConfig::new(laid.grid)
        };
        match spawn(pane_config) {
            Ok(pane) => {
                pane.set_padding(engine_padding(laid.padding));
                *self.pane.borrow_mut() = Some(Rc::new(pane));
                self.status.set(Status::Live);
                tracing::debug!(target: "agterm::surface", key = self.key, scale = self.scale.get(), "terminal surface created");
                self.queue(Effect::Emit(TerminalEvent::Spawned));
            }
            Err(err) => {
                self.status.set(Status::Failed);
                tracing::error!(target: "agterm::surface", key = self.key, %err, "could not create the terminal surface");
                self.queue(Effect::Emit(TerminalEvent::SpawnFailed(err.to_string())));
            }
        }
    }

    /// Drops the pane (its child gets `SIGHUP`) and ends the pump; once.
    pub fn teardown(&self) {
        if self.status.replace(Status::TornDown) == Status::TornDown {
            return;
        }
        self.spawn.borrow_mut().take();
        self.effects.borrow_mut().clear();
        self.kitty.borrow_mut().clear();
        let pane = self.pane.borrow_mut().take();
        drop(pane);
        // one last pass lets the views drop what they drew, then the channel ends the pump
        self.poke();
        self.wake.close();
    }

    /// Logs the first frame drawn (what `ghostty_probe.py` waits for).
    fn drew(&self) {
        if !self.drew.replace(true) {
            tracing::debug!(target: "agterm::surface", key = self.key, "terminal drew its first frame");
        }
    }
}

fn engine_padding(padding: Padding) -> agterm_term::Padding {
    agterm_term::Padding {
        top: padding.top,
        bottom: padding.bottom,
        left: padding.left,
        right: padding.right,
    }
}

fn resize(pane: &Pane, laid: Laid) -> Result<(), PaneError> {
    pane.resize(laid.grid)?;
    pane.set_padding(engine_padding(laid.padding));
    Ok(())
}

/// A terminal: the pane's pump and the events it emits (module docs).
pub struct Terminal {
    shared: Rc<Shared>,
    _pump: Task<()>,
}

impl EventEmitter<TerminalEvent> for Terminal {}

impl Terminal {
    /// A terminal that starts its pane with `spawn` once its owner view has a size.
    pub fn new(spec: TerminalSpec, config: TerminalConfig, spawn: Spawn, cx: &mut Context<Self>) -> Self {
        let (wake, woken) = async_channel::bounded::<()>(1);
        let points = spec.font_size.unwrap_or(config.font.size).clamp(1.0, 255.0);
        let shared = Rc::new(Shared {
            key: NEXT_KEY.fetch_add(1, Ordering::Relaxed),
            spec,
            config: RefCell::new(Rc::new(config)),
            text: cx.text_system().clone(),
            fonts: RefCell::new(None),
            font_generation: Cell::new(0),
            pane: RefCell::new(None),
            spawn: RefCell::new(Some(spawn)),
            status: Cell::new(Status::Pending),
            points: Cell::new(points),
            original_points: points,
            scale: Cell::new(1.0),
            metrics: Cell::new(None),
            laid: Cell::new(None),
            effects: RefCell::new(Vec::new()),
            wake,
            drew: Cell::new(false),
            preedit: RefCell::new(None),
            background: RefCell::new(background::Loader::default()),
            kitty: RefCell::new(kitty::KittyStore::default()),
        });
        let pump = cx.spawn(async move |this, cx| {
            while woken.recv().await.is_ok() {
                if this.update(cx, Terminal::pump).is_err() {
                    break;
                }
            }
        });
        Self { shared, _pump: pump }
    }

    #[must_use]
    pub fn shared(&self) -> &Rc<Shared> {
        &self.shared
    }

    /// The core's Surface over this terminal.
    #[must_use]
    pub fn surface(&self) -> TerminalSurface {
        TerminalSurface::new(Rc::clone(&self.shared))
    }

    /// Takes a new config (a reload): the pane's theme, cursor and scrollback, the views' fonts and colors. The live
    /// font size is kept.
    pub fn set_config(&mut self, config: TerminalConfig, cx: &mut Context<Self>) {
        let shared = &self.shared;
        if let Some(pane) = shared.pane() {
            let applied = pane
                .set_colors(&config.theme)
                .and_then(|()| pane.set_cursor_defaults(config.cursor))
                .and_then(|()| pane.set_scrollback(config.scrollback));
            if let Err(err) = applied {
                tracing::warn!(target: "agterm::surface", key = shared.key, %err, "the terminal refused the config");
            }
        }
        let fonts_changed = shared.config.borrow().font != config.font;
        *shared.config.borrow_mut() = Rc::new(config);
        // the background image's file is looked at again (a watermark rendered anew into the same file)
        shared.background.borrow_mut().invalidate();
        if fonts_changed {
            shared.fonts.borrow_mut().take();
            shared.font_generation.set(shared.font_generation.get() + 1);
        }
        cx.notify();
    }

    /// One wake: the pane's events, the queued work, and a redraw.
    fn pump(&mut self, cx: &mut Context<Self>) {
        let shared = Rc::clone(&self.shared);
        if let Some(pane) = shared.pane() {
            for event in pane.take_events() {
                cx.emit(TerminalEvent::Pane(event));
            }
        }
        let effects = std::mem::take(&mut *shared.effects.borrow_mut());
        for effect in effects {
            run_effect(&shared, effect, cx);
        }
        cx.notify();
    }
}

fn run_effect(shared: &Shared, effect: Effect, cx: &mut Context<Terminal>) {
    match effect {
        Effect::Emit(event) => cx.emit(event),
        Effect::Copy(text, location) => {
            let item = ClipboardItem::new_string(text);
            match location {
                ClipboardLocation::Standard => cx.write_to_clipboard(item),
                // ghostty's "selection" clipboard is the primary selection on Linux
                ClipboardLocation::Selection | ClipboardLocation::Primary => cx.write_to_primary(item),
            }
        }
        Effect::Paste(location) => {
            let item = match location {
                ClipboardLocation::Standard => cx.read_from_clipboard(),
                ClipboardLocation::Selection | ClipboardLocation::Primary => cx.read_from_primary(),
            };
            let (Some(text), Some(pane)) = (item.and_then(|item| item.text()), shared.pane()) else {
                return;
            };
            match pane.paste(&text, PasteSource::Clipboard, location, false) {
                Ok(Paste::Unsafe) => cx.emit(TerminalEvent::UnsafePaste(text)),
                Ok(Paste::Written | Paste::Empty) => {}
                Err(err) => tracing::warn!(target: "agterm::surface", key = shared.key, %err, "paste failed"),
            }
        }
    }
}

/// Whether a view lays the pane out and takes its input, or only draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewRole {
    /// The pane's own view: lays the grid out, starts and resizes the pane, takes focus.
    Owner,
    /// A second rendering (the dashboard): draws the pane's frame at its own font size.
    Viewer,
}

/// Ghostty's cursor blink interval.
pub const BLINK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(600);

/// A view of a terminal (module docs).
pub struct TerminalView {
    terminal: Entity<Terminal>,
    shared: Rc<Shared>,
    role: ViewRole,
    focus: FocusHandle,
    cache: RowCache<ShapedRow, (u32, u32, u64)>,
    /// A viewer's own font size; `None` draws at the terminal's.
    font_override: Option<f32>,
    /// The hovered link: viewport row and inclusive columns (Task 14 sets it).
    link: Option<(u16, u16, u16)>,
    /// The cursor's blink phase; on while not blinking.
    blink_on: bool,
    blink: Option<Task<()>>,
    /// The frame drawn last.
    painted: Option<Arc<Frame>>,
    /// The Kitty images and the background image this view put into its window's atlas.
    images: kitty::ViewImages,
    /// The layers the last paint drew, in order, with how many Kitty placements each Kitty layer had.
    painted_layers: Vec<(element::Layer, usize)>,
    _observe: Subscription,
}

impl TerminalView {
    pub fn new(terminal: &Entity<Terminal>, role: ViewRole, cx: &mut Context<Self>) -> Self {
        let shared = Rc::clone(terminal.read(cx).shared());
        Self {
            terminal: terminal.clone(),
            shared,
            role,
            focus: cx.focus_handle(),
            cache: RowCache::default(),
            font_override: None,
            link: None,
            blink_on: true,
            blink: None,
            painted: None,
            images: kitty::ViewImages::default(),
            painted_layers: Vec::new(),
            _observe: cx.observe(terminal, |_, _, cx| cx.notify()),
        }
    }

    #[must_use]
    pub fn terminal(&self) -> &Entity<Terminal> {
        &self.terminal
    }

    #[must_use]
    pub fn shared(&self) -> &Rc<Shared> {
        &self.shared
    }

    #[must_use]
    pub fn role(&self) -> ViewRole {
        self.role
    }

    /// Draws at `points` instead of the terminal's size (a viewer: the dashboard's cells); `None` follows the terminal.
    pub fn set_font_override(&mut self, points: Option<f32>, cx: &mut Context<Self>) {
        self.font_override = points.map(|points| points.clamp(1.0, 255.0));
        cx.notify();
    }

    /// Underlines a hovered link: viewport `row`, columns `start..=end`; `None` clears it.
    pub fn set_hovered_link(&mut self, link: Option<(u16, u16, u16)>, cx: &mut Context<Self>) {
        if self.link != link {
            self.link = link;
            cx.notify();
        }
    }

    /// The frame this view drew last.
    #[must_use]
    pub fn painted(&self) -> Option<&Arc<Frame>> {
        self.painted.as_ref()
    }

    /// The rows the last paint had to rebuild (tests, profiling).
    #[must_use]
    pub fn rebuilt_rows(&self) -> &[usize] {
        &self.cache.rebuilt
    }

    /// The cached plan of viewport row `index`.
    #[must_use]
    pub fn row_layout(&self, index: usize) -> Option<&RowLayout> {
        self.cache.row(index).map(|row| &row.layout)
    }

    /// The glyphs placed for viewport row `index` at the last paint.
    #[must_use]
    pub fn row_glyphs(&self, index: usize) -> Option<&ShapedRow> {
        self.cache.row(index).and_then(|row| row.shaped.as_ref())
    }

    /// The cells of viewport row `index` the sprites drew at the last paint.
    #[must_use]
    pub fn row_sprites(&self, index: usize) -> Option<&[SpriteCell]> {
        self.cache.row(index).map(|row| row.layout.sprites.as_slice())
    }

    /// The font size this view draws at.
    #[must_use]
    pub fn font_points(&self) -> f32 {
        self.font_override
            .filter(|_| self.role == ViewRole::Viewer)
            .unwrap_or_else(|| self.shared.font_points())
    }

    /// Whether the cursor is in the visible phase of its blink.
    #[must_use]
    pub fn blink_on(&self) -> bool {
        self.blink_on
    }

    /// Runs the blink timer while the owner is focused and the cursor blinks; stops it (cursor shown) otherwise.
    fn sync_blink(&mut self, focused: bool, cx: &mut Context<Self>) {
        let blinking = focused
            && self.role == ViewRole::Owner
            && self
                .painted
                .as_ref()
                .is_some_and(|frame| frame.cursor.blinking && frame.cursor.visible);
        if !blinking {
            self.blink = None;
            self.blink_on = true;
            return;
        }
        if self.blink.is_some() {
            return;
        }
        let timer = cx.background_executor().clone();
        self.blink = Some(cx.spawn(async move |this, cx| {
            loop {
                timer.timer(BLINK_INTERVAL).await;
                let toggled = this.update(cx, |view, cx| {
                    view.blink_on = !view.blink_on;
                    cx.notify();
                });
                if toggled.is_err() {
                    break;
                }
            }
        }));
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.role == ViewRole::Owner && self.focus.is_focused(window);
        self.sync_blink(focused, cx);
        let element = TerminalElement::new(cx.entity(), focused);
        let root = div().size_full().min_w(px(0.)).min_h(px(0.));
        match self.role {
            ViewRole::Owner => root.track_focus(&self.focus).child(element),
            ViewRole::Viewer => root.child(element),
        }
    }
}
