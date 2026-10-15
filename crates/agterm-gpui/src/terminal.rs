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
//!
//! Input (plan > Task 14) is the owner view's: keys reach it from the app's keystroke interceptor
//! (`crate::keymap_dispatch`, after the window's keymap) and go through ghostty's binds to the encoder
//! (`keyboard.rs`, `input.rs`), the input method composes at the cursor, the mouse selects, reports or opens links
//! (`mouse.rs`, `links.rs`), and the pump answers a program's clipboard access under `clipboard-read`/`-write` and
//! the prompt (`clipboard.rs`).

pub mod background;
mod clipboard;
mod colors;
mod element;
mod font;
pub mod input;
mod keyboard;
mod keycodes;
pub mod kitty;
pub mod links;
mod metrics;
mod mouse;
mod row_cache;
pub mod sprite;
mod surface;

#[cfg(test)]
mod input_tests;
#[cfg(test)]
mod kitty_tests;
#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agterm_core::ghostty_config::binds::Keybinds;
use agterm_core::ghostty_config::{self, CopyOnSelect, GhosttyConfig};
use agterm_core::keymap::SurfaceOwner;
use agterm_term::{
    ClipboardLocation, CursorDefaults, FormatRegion, Frame, GridSize, Pane, PaneConfig, PaneError, PaneEvent,
    PasteSource, ScrollbackLimits, TextFormat, ThemeColors,
};
use gpui_kit::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ModifiersChangedEvent, ParentElement as _, Render, Styled as _, Subscription, Task, TextSystem, Window, div, px,
};

pub use clipboard::{ClipboardPrompts, prompt_text};
pub use colors::{CellPaint, Mark, Palette, Pick};
pub use element::{Layer as PaintLayer, PlacedGlyph, ShapedRow, TerminalElement};
pub use font::{DEFAULT_FAMILIES, DEFAULT_FAMILY, Fonts, SYMBOLS_FAMILY, register_embedded as register_embedded_fonts};
pub use keyboard::{key_down, typing_surface};
pub use links::{Link, LinkKind, follow_link};
pub use metrics::{CellMetrics, DEFAULT_DPI, FaceMetrics, Geometry, Padding, points_to_px};
pub use mouse::{cursor_style, scroll_multiplier};
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
    /// ghostty's `keybind`s.
    pub keybinds: SharedKeybinds,
    /// `selection-clear-on-typing`.
    pub selection_clear_on_typing: bool,
    pub mouse: ghostty_config::Mouse,
    pub link: ghostty_config::Link,
    pub clipboard: ghostty_config::Clipboard,
    /// `background-image*`, drawn behind the grid.
    pub background_image: ghostty_config::BackgroundImage,
    /// `clipboard-trim-trailing-spaces`: copies drop trailing whitespace.
    pub copy_trim: bool,
    /// `selection-clear-on-copy`.
    pub clear_selection_on_copy: bool,
    /// `copy-on-select`: where a selection made is copied ([`Shared::copy_on_select`]).
    pub copy_on_select: CopyOnSelect,
    /// `selection-word-chars`: the word boundaries of word selections; `None` is Ghostty's default set.
    pub word_chars: Option<Vec<char>>,
}

/// The keybinds a config loaded, shared by every terminal of it (equal when they are the same load).
#[derive(Debug, Clone)]
pub struct SharedKeybinds(Arc<Keybinds>);

impl std::ops::Deref for SharedKeybinds {
    type Target = Keybinds;

    fn deref(&self) -> &Keybinds {
        &self.0
    }
}

impl PartialEq for SharedKeybinds {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
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
            keybinds: SharedKeybinds(Arc::new(config.keybind.clone())),
            selection_clear_on_typing: config.selection.clear_on_typing,
            mouse: config.mouse.clone(),
            link: config.link,
            clipboard: config.clipboard.clone(),
            background_image: config.background_image.clone(),
            copy_trim: config.clipboard.trim_trailing_spaces,
            clear_selection_on_copy: config.selection.clear_on_copy,
            copy_on_select: config.copy_on_select,
            word_chars: config.selection.word_chars.clone(),
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

/// Starts the pane (the deck passes `Pane::launch` with the pane's launch); called once, with the size of the grid
/// the owner view first laid out and the wake that reaches this terminal's pump.
pub type Spawn = Box<dyn FnOnce(PaneConfig) -> Result<Pane, PaneError>>;

/// What a gated terminal calls once, when its owner view first has a size: it asks for its launch, which comes later
/// through [`Shared::launch_with`] (a session pane waits for the core's seed and its paced launch turn).
pub type AskLaunch = Box<dyn FnOnce()>;

/// What a key press on a terminal that holds its keys does instead of reaching the program ([`Shared::hold_keys`]).
pub type KeyHold = Rc<dyn Fn()>;

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
    /// A `start_search` binding (an empty needle) or `search_selection` (the selection as the needle): the frontend
    /// opens its search UI with the needle.
    StartSearch(String),
    /// An `end_search` binding ended the search.
    EndSearch,
    /// A link was clicked, or `write_*_file:open` asked for its file: the frontend opens it through the link
    /// policy ([`follow_link`] is the default route), as libghostty's `OPEN_URL` asked.
    OpenUrl(String),
    /// `toggle_readonly` turned read-only mode on or off: the frontend shows it.
    Readonly(bool),
    /// `focus-follows-mouse`: the pointer moved over this terminal's owner view, which does not hold the keyboard,
    /// in the active window, with no button held. The frontend focuses the pane (Ghostty's `moveFocus`; the deck owns
    /// pane focus); emitted once until the view's focus changes or the pointer leaves it.
    FocusFollowsMouse,
    /// A key press or repeat reached the terminal's keyboard, classified for the status glyph (the GTK build's
    /// `connect_keystroke`, upstream's `onUserInputClearsStatus` from `keyDown`): before its hold, ghostty's binds or
    /// the program see it.
    Keystroke(agterm_core::model::StatusKeystroke),
}

/// Work that needs the app, queued by the Surface for the pump.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Effect {
    Paste(ClipboardLocation),
    Copy(String, ClipboardLocation),
    /// A copy with an HTML representation (`copy_to_clipboard:html` alone, `:mixed` with its plain text).
    CopyHtml {
        text: Option<String>,
        html: String,
    },
    Emit(TerminalEvent),
    /// A gated terminal's launch arrived after its owner laid it out: start it now.
    Spawn,
}

/// Where a terminal is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Waiting for the owner view's first grid.
    Pending,
    /// Laid out, a gated terminal waits for its launch ([`Shared::launch_with`]).
    Waiting,
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
    /// What `reset_font_size` returns to: the size the terminal started at, then the config's after a reload.
    original_points: Cell<f32>,
    /// A font size action changed the size (`reset_font_size` clears it): a config reload keeps the live size then,
    /// and takes the configured one otherwise (Ghostty's `font_size_adjusted`).
    font_adjusted: Cell<bool>,
    /// The owner view's scale factor, last seen.
    scale: Cell<f32>,
    metrics: Cell<Option<(u32, u32, u64, CellMetrics)>>,
    laid: Cell<Option<Laid>>,
    effects: RefCell<Vec<Effect>>,
    wake: async_channel::Sender<()>,
    /// The first frame was drawn (logged once).
    drew: Cell<bool>,
    /// The owner view's input-method composition, shown at the cursor.
    preedit: RefCell<Option<String>>,
    /// Whose terminal this is, for a custom command's typing surface (the frontend sets it).
    owner: RefCell<Option<SurfaceOwner>>,
    /// The `background-image` every view draws, loaded once per terminal.
    background: RefCell<background::Loader>,
    /// The Kitty images as textures, converted off the main thread for every view.
    kitty: RefCell<kitty::KittyStore>,
    /// Ghostty's read-only mode (`toggle_readonly`): the user's input does not reach the program.
    readonly: Cell<bool>,
    /// The URL of the link under the pointer (`copy_url_to_clipboard`; Task 14's hover sets it).
    hovered_url: RefCell<Option<String>>,
    /// A gated terminal's ask for its launch, until it is made.
    ask: RefCell<Option<AskLaunch>>,
    /// The terminal is a client of its pane's live-session daemon: from the spec, or from the launch it started with.
    live: Cell<bool>,
    /// While set, a key press runs this instead of reaching the program (a lead cover, a held exit).
    key_hold: RefCell<Option<KeyHold>>,
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
        self.original_points.get()
    }

    /// A font size action set `points`; `reset` (`reset_font_size`) gives a config reload the size back.
    fn adjust_font_points(&self, points: f32, reset: bool) {
        self.set_font_points(points);
        self.font_adjusted.set(!reset);
    }

    /// A config reload of font size `configured`: it is what `reset_font_size` returns to from now on, and the live size
    /// unless a font size action changed it (Ghostty's `changeConfig`). Whether the live size changed.
    fn follow_config_points(&self, configured: f32) -> bool {
        let configured = configured.clamp(1.0, 255.0);
        self.original_points.set(configured);
        if self.font_adjusted.get() || (configured - self.points.get()).abs() < f32::EPSILON {
            return false;
        }
        self.points.set(configured);
        true
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

    /// Whose terminal this is.
    #[must_use]
    pub fn owner(&self) -> Option<SurfaceOwner> {
        self.owner.borrow().clone()
    }

    /// Sets whose terminal this is (a session pane's, an overlay's).
    pub fn set_owner(&self, owner: Option<SurfaceOwner>) {
        *self.owner.borrow_mut() = owner;
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

    /// Read-only (`toggle_readonly`): the user's input paths (keys, typing, pastes, mouse reports) leave the program
    /// alone; the terminal's own replies still go.
    #[must_use]
    pub fn is_readonly(&self) -> bool {
        self.readonly.get()
    }

    pub fn set_readonly(&self, readonly: bool) {
        self.readonly.set(readonly);
    }

    /// The URL of the hovered link, which `copy_url_to_clipboard` copies.
    #[must_use]
    pub fn hovered_url(&self) -> Option<String> {
        self.hovered_url.borrow().clone()
    }

    /// Sets or clears the hovered link's URL (Task 14's hover tracking).
    pub fn set_hovered_url(&self, url: Option<String>) {
        *self.hovered_url.borrow_mut() = url;
    }

    /// A selection was made (`select_all`, and the mouse's selection in Task 14): Ghostty's `setSelectionAndCopy`
    /// copies it per `copy-on-select`: not at all, to the selection clipboard, or to it and the system clipboard; the
    /// text as `copy_to_clipboard` formats it (`clipboard-trim-trailing-spaces`; the plain part of Ghostty's mixed
    /// copy, GPUI's clipboard taking text only). An empty selection copies nothing.
    pub fn copy_on_select(&self) {
        let config = self.config();
        let locations: &[ClipboardLocation] = match config.copy_on_select {
            CopyOnSelect::False => return,
            CopyOnSelect::True => &[ClipboardLocation::Selection],
            CopyOnSelect::Clipboard => &[ClipboardLocation::Standard, ClipboardLocation::Selection],
        };
        let Some(pane) = self.pane() else {
            return;
        };
        let Some(text) = pane
            .format(FormatRegion::Selection, TextFormat::Plain, config.copy_trim)
            .filter(|text| !text.is_empty())
        else {
            return;
        };
        for location in locations {
            self.queue(Effect::Copy(text.clone(), *location));
        }
    }

    /// Whether the terminal is a client of its pane's live-session daemon.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.live.get()
    }

    /// Hands a gated terminal its launch: it starts at once when its owner already laid it out (on the pump's next
    /// tick), else at its first layout. Ignored once the terminal started, failed or was torn down, and for a second
    /// launch. `live`: the launch attaches to a live-session daemon.
    pub fn launch_with(&self, spawn: Spawn, live: bool) {
        if !matches!(self.status.get(), Status::Pending | Status::Waiting) || self.spawn.borrow().is_some() {
            return;
        }
        self.live.set(live);
        self.ask.borrow_mut().take();
        *self.spawn.borrow_mut() = Some(spawn);
        if self.status.get() == Status::Waiting {
            self.status.set(Status::Spawning);
            self.queue(Effect::Spawn);
        }
    }

    /// Whether the terminal waits for its launch (gated, laid out, not handed one yet).
    #[must_use]
    pub fn is_waiting(&self) -> bool {
        self.status.get() == Status::Waiting
    }

    /// While `hold` is set a key press runs it instead of reaching the program; `None` gives the keys back.
    pub fn hold_keys(&self, hold: Option<KeyHold>) {
        *self.key_hold.borrow_mut() = hold;
    }

    /// Whether the terminal holds its keys.
    #[must_use]
    pub fn holds_keys(&self) -> bool {
        self.key_hold.borrow().is_some()
    }

    /// A key press reached the terminal: while it holds its keys, the hold runs and the key is consumed (`true`); the
    /// keyboard path sends the key to the program only otherwise.
    pub fn held_key(&self) -> bool {
        let hold = self.key_hold.borrow().clone();
        match hold {
            Some(hold) => {
                hold();
                true
            }
            None => false,
        }
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
        let cell = self.cell_size();
        self.scale.set(scale);
        if self.cell_size() != cell {
            // a display of another scale: the cell changed as a font change changes it (libghostty's `CELL_SIZE`)
            self.queue(Effect::Emit(TerminalEvent::FontSize));
        }
        if self.laid.get() == Some(laid) {
            return false;
        }
        self.laid.set(Some(laid));
        match self.status.get() {
            Status::Pending if self.spawn.borrow().is_some() => {
                self.status.set(Status::Spawning);
                true
            }
            Status::Pending => {
                // gated: ask for the launch once, which may come back later
                self.status.set(Status::Waiting);
                let ask = self.ask.borrow_mut().take();
                if let Some(ask) = ask {
                    ask();
                }
                false
            }
            Status::Live => {
                if let Some(pane) = self.pane()
                    && let Err(err) = resize(&pane, laid)
                {
                    tracing::warn!(target: "agterm::surface", key = self.key, %err, "terminal resize failed");
                }
                false
            }
            Status::Waiting | Status::Spawning | Status::Failed | Status::TornDown => false,
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
                // the pane's reporting setting starts from the config; `toggle_mouse_reporting` flips it from there
                pane.set_mouse_reporting(config.mouse.reporting);
                pane.set_word_boundaries(config.word_chars.as_deref());
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
        self.ask.borrow_mut().take();
        self.key_hold.borrow_mut().take();
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
        Self::build(spec, config, Some(spawn), None, cx)
    }

    /// A terminal that calls `ask` once its owner view has a size, and starts its pane only once
    /// [`Shared::launch_with`] hands it a launch (the GTK build's gated surface).
    pub fn gated(spec: TerminalSpec, config: TerminalConfig, ask: AskLaunch, cx: &mut Context<Self>) -> Self {
        Self::build(spec, config, None, Some(ask), cx)
    }

    fn build(
        spec: TerminalSpec,
        config: TerminalConfig,
        spawn: Option<Spawn>,
        ask: Option<AskLaunch>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (wake, woken) = async_channel::bounded::<()>(1);
        let live = spec.live;
        let points = spec.font_size.unwrap_or(config.font.size).clamp(1.0, 255.0);
        let shared = Rc::new(Shared {
            key: NEXT_KEY.fetch_add(1, Ordering::Relaxed),
            spec,
            config: RefCell::new(Rc::new(config)),
            text: cx.text_system().clone(),
            fonts: RefCell::new(None),
            font_generation: Cell::new(0),
            pane: RefCell::new(None),
            spawn: RefCell::new(spawn),
            status: Cell::new(Status::Pending),
            points: Cell::new(points),
            original_points: Cell::new(points),
            font_adjusted: Cell::new(false),
            scale: Cell::new(1.0),
            metrics: Cell::new(None),
            laid: Cell::new(None),
            effects: RefCell::new(Vec::new()),
            wake,
            drew: Cell::new(false),
            preedit: RefCell::new(None),
            owner: RefCell::new(None),
            background: RefCell::new(background::Loader::default()),
            kitty: RefCell::new(kitty::KittyStore::default()),
            readonly: Cell::new(false),
            hovered_url: RefCell::new(None),
            ask: RefCell::new(ask),
            live: Cell::new(live),
            key_hold: RefCell::new(None),
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

    /// Takes a new config of the same app config (its own overlay changed: a background watermark): the pane's theme,
    /// cursor and scrollback, the views' fonts and colors. The live font size and the size a reset returns to are
    /// kept, as the GTK build's overlay restates the live size.
    pub fn set_config(&mut self, config: TerminalConfig, cx: &mut Context<Self>) {
        self.apply_config(config, false, cx);
    }

    /// Takes a reloaded app config as [`Self::set_config`] does, and its font size too: what a reset returns to from
    /// now on, and the live size unless a font size action changed it. A restored or replacement terminal's start size
    /// was not stepped, so it takes the configured one, as libghostty's re-feed does in the GTK build.
    pub fn reload_config(&mut self, config: TerminalConfig, cx: &mut Context<Self>) {
        self.apply_config(config, true, cx);
    }

    fn apply_config(&mut self, config: TerminalConfig, reload: bool, cx: &mut Context<Self>) {
        let shared = &self.shared;
        if let Some(pane) = shared.pane() {
            pane.set_mouse_reporting(config.mouse.reporting);
            pane.set_word_boundaries(config.word_chars.as_deref());
            let applied = pane
                .set_colors(&config.theme)
                .and_then(|()| pane.set_cursor_defaults(config.cursor))
                .and_then(|()| pane.set_scrollback(config.scrollback));
            if let Err(err) = applied {
                tracing::warn!(target: "agterm::surface", key = shared.key, %err, "the terminal refused the config");
            }
        }
        let fonts_changed = shared.config.borrow().font != config.font;
        let cell = shared.metrics_at(shared.font_points(), shared.scale());
        let resized = reload && shared.follow_config_points(config.font.size);
        *shared.config.borrow_mut() = Rc::new(config);
        // the background image's file is looked at again (a watermark rendered anew into the same file)
        shared.background.borrow_mut().invalidate();
        if fonts_changed {
            shared.fonts.borrow_mut().take();
            shared.font_generation.set(shared.font_generation.get() + 1);
        }
        // the cell follows the fonts and the size: report it as a font size change does
        if (fonts_changed || resized) && shared.metrics_at(shared.font_points(), shared.scale()) != cell {
            cx.emit(TerminalEvent::FontSize);
        }
        cx.notify();
    }

    /// One wake: the pane's events, the queued work, and a redraw.
    fn pump(&mut self, cx: &mut Context<Self>) {
        let shared = Rc::clone(&self.shared);
        if let Some(pane) = shared.pane() {
            for event in pane.take_events() {
                if !clipboard::handle(&shared, &event, cx) {
                    cx.emit(TerminalEvent::Pane(event));
                }
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
        Effect::Spawn => shared.spawn(),
        Effect::CopyHtml { text, html } => copy_html(text, html, cx),
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
            if shared.is_readonly() {
                return;
            }
            // pasted as confirmed, as upstream and the GTK build complete libghostty's unsafe-paste confirmation
            if let Err(err) = pane.paste(&text, PasteSource::Clipboard, location, true) {
                tracing::warn!(target: "agterm::surface", key = shared.key, %err, "paste failed");
            }
        }
    }
}

/// Writes the clipboard with `html` offered as `text/html`, beside `text` or alone (Ghostty's `copy_to_clipboard:mixed`
/// and `:html`): GPUI's Linux clipboard offers it through our `gpui-pre-linux` patch (`with_clipboard_html`); a
/// platform without one gets the text (or the markup) as plain text.
fn copy_html(text: Option<String>, html: String, cx: &mut App) {
    let with_text = text.is_some();
    let item = ClipboardItem::new_string(text.unwrap_or_else(|| html.clone()));
    #[cfg(target_os = "linux")]
    {
        let offer = gpui_linux::ClipboardHtml { html, with_text };
        gpui_linux::with_clipboard_html(offer, || cx.write_to_clipboard(item));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (html, with_text);
        cx.write_to_clipboard(item);
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
    /// The hovered link's cells: viewport row and inclusive columns, per row.
    link: Vec<(u16, u16, u16)>,
    /// The keyboard's state (owner only).
    keys: keyboard::KeyState,
    /// The pointer's state (owner only).
    pointer: mouse::PointerState,
    /// The cursor's blink phase; on while not blinking.
    blink_on: bool,
    blink: Option<Task<()>>,
    /// The frame drawn last.
    painted: Option<Arc<Frame>>,
    /// The view is on screen. A hidden one (a deck page or pane not shown) still lays its grid out, so its pane
    /// starts and follows the size, but captures no frame and redraws for nothing its terminal does.
    shown: bool,
    /// The Kitty images and the background image this view put into its window's atlas.
    images: kitty::ViewImages,
    /// The layers the last paint drew, in order, with how many Kitty placements each Kitty layer had.
    painted_layers: Vec<(element::Layer, usize)>,
    _observe: Subscription,
}

impl TerminalView {
    pub fn new(terminal: &Entity<Terminal>, role: ViewRole, cx: &mut Context<Self>) -> Self {
        let shared = Rc::clone(terminal.read(cx).shared());
        let focus = cx.focus_handle();
        if role == ViewRole::Owner {
            crate::keymap_dispatch::init(cx);
            keyboard::register(&focus, cx.weak_entity(), &shared);
        }
        Self {
            terminal: terminal.clone(),
            shared,
            role,
            focus,
            cache: RowCache::default(),
            font_override: None,
            link: Vec::new(),
            keys: keyboard::KeyState::default(),
            pointer: mouse::PointerState::default(),
            blink_on: true,
            blink: None,
            painted: None,
            shown: true,
            _observe: cx.observe(terminal, |view: &mut Self, _, cx| {
                if view.shown {
                    cx.notify();
                }
            }),
            images: kitty::ViewImages::default(),
            painted_layers: Vec::new(),
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

    /// Shows or hides the view (module docs of `TerminalView::shown`); a view shown again redraws.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.shown != shown {
            self.shown = shown;
            cx.notify();
        }
    }

    /// Whether the view is on screen.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// Draws at `points` instead of the terminal's size (a viewer: the dashboard's cells); `None` follows the terminal.
    pub fn set_font_override(&mut self, points: Option<f32>, cx: &mut Context<Self>) {
        self.font_override = points.map(|points| points.clamp(1.0, 255.0));
        cx.notify();
    }

    /// Underlines a hovered link that may run over several rows: (viewport row, first column, last column) each.
    pub fn set_hovered_links(&mut self, cells: Vec<(u16, u16, u16)>, cx: &mut Context<Self>) {
        if self.link != cells {
            self.link = cells;
            cx.notify();
        }
    }

    /// The live link under the pointer.
    #[must_use]
    pub fn hovered_link(&self) -> Option<&Link> {
        self.pointer.link.as_ref()
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

    /// Runs the blink timer while the owner is focused and the cursor blinks; stops it (cursor shown) otherwise. Called
    /// by prepaint once the frame it draws is current.
    pub(crate) fn sync_blink(&mut self, focused: bool, cx: &mut Context<Self>) {
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
        let element = TerminalElement::new(cx.entity(), focused);
        let root = div().size_full().min_w(px(0.)).min_h(px(0.));
        match self.role {
            ViewRole::Owner => {
                self.watch_input(window, cx);
                root.track_focus(&self.focus)
                    .on_key_up(cx.listener(Self::key_up))
                    .on_modifiers_changed(cx.listener(|view, event: &ModifiersChangedEvent, _, cx| {
                        view.modifiers_changed(event.modifiers, cx);
                    }))
                    .child(element)
            }
            ViewRole::Viewer => root.child(element),
        }
    }
}
