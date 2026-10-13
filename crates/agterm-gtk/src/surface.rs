//! One terminal: a libghostty surface rendered in a `GtkGLArea`, with its keyboard, input method, pointer, scroll and
//! focus wiring. Behavior follows upstream's `GhosttySurfaceView*.swift` and `libghostty.md`; the `GtkGLArea` hosting
//! is the Linux glue (the patched OpenGL platform draws on this thread into the area's current context).
//!
//! Lifecycle: the libghostty surface is created once the realized area first has a nonzero size (upstream creates
//! only at a nonzero size) and freed exactly once, by [`TerminalSurface::teardown`], which unrealizing the area calls
//! with the context current. The embedding API cannot move a renderer to a new GL context, so a live surface's area
//! must stay realized: containers keep it mounted and hide it, never re-parent it.
//!
//! Creation does not wait for a draw. A `GtkGLArea` emits `resize` only from its snapshot, and GTK snapshots no
//! widget at opacity 0, so a hidden deck page (upstream's eager deck mounts every session and spawns its shell at
//! startup) would never get its surface that way. The realized area instead runs a one-shot frame-clock tick that
//! creates the surface at the area's allocated size as soon as it has one; `resize` then only pushes sizes (and
//! creates, should it come first).
//!
//! Ownership: the area's `destroy` handler holds the one long-lived strong reference to the surface's state, and the
//! state holds the area only weakly, so a container dropping the area disposes it, `destroy` releases the state and
//! the key leaves the registry. A [`TerminalSurface`] handle holds the area strongly while it lives, which keeps a
//! surface that is not in a container yet alive.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use agterm_core::keystrokes::{self, SUBMIT_GAP};
use agterm_core::link_policy::ClickOrigin;
use agterm_core::model::{SearchReport, StatusKeystroke};
use agterm_core::persist::settings::LinkOpenMode;
use agterm_ghostty::input::{KeyAction, KeyEvent, MouseState, ScrollMods};
use agterm_ghostty::{
    ClipboardContent, MouseShape, PendingRequest, PointTag, Selection, Surface, SurfaceKey, SurfaceOptions,
};
use agterm_host::TextScope;
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::ghostty_app;
use crate::input::{self, PressedKeys};

/// What the terminal runs. `None` fields take the config's values.
#[derive(Debug, Clone, Default)]
pub struct SurfaceSpec {
    pub working_directory: Option<PathBuf>,
    /// Runs through `/bin/sh -c` instead of the configured command.
    pub command: Option<String>,
    pub env: Vec<(String, String)>,
    pub wait_after_command: bool,
    /// The point size the terminal starts at; `None` is the config's.
    pub font_size: Option<f32>,
}

/// Where the input method is inside a key event (ghostty's GTK runtime tracks the same: input methods differ in the
/// order they emit commit and preedit-end).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum KeyPhase {
    #[default]
    Outside,
    Composing,
    NotComposing,
}

#[derive(Debug, Default)]
struct KeyState {
    phase: KeyPhase,
    composing: bool,
    /// Text the input method committed during the current key event, sent with the key.
    committed: String,
    pressed: PressedKeys,
}

#[derive(Debug, Default)]
struct PointerState {
    shape: Option<MouseShape>,
    hidden: bool,
}

type ExitHandler = Box<dyn FnOnce()>;
type ReportHandler = Box<dyn Fn(&str)>;
type SearchHandler = Box<dyn Fn(&SearchReport)>;
type KeystrokeHandler = Box<dyn Fn(StatusKeystroke)>;
type FocusHandler = Box<dyn Fn()>;
type NotificationHandler = Box<dyn Fn(&str, &str)>;
type FontSizeHandler = Box<dyn Fn(f64)>;

/// How far apart two font sizes may be and still count as the same report (upstream's half-point tolerance).
const FONT_REPORT_TOLERANCE: f64 = 0.5;

/// What a cell-size report does under the dashboard's font override (upstream's `reportFontSize`): whether the live
/// `size` is reported as the pane's own, and the pending restore left. Nothing is reported while an override is on (its
/// size is transient); the first report after the override cleared is the revert to `pending`, dropped when it
/// matches, so restoring the grid's font never pins a session to it.
#[must_use]
pub fn font_report(override_size: Option<f64>, pending: Option<f64>, size: f64) -> (bool, Option<f64>) {
    if override_size.is_some() {
        return (false, pending);
    }
    let revert = pending.is_some_and(|pending| (size - pending).abs() <= FONT_REPORT_TOLERANCE);
    (!revert, None)
}

/// The restore a cleared override arms: the size the terminal reverts to, when that is a real change from the
/// override's (an equal size emits no cell-size report, and a pending restore would swallow a later zoom).
#[must_use]
pub fn pending_restore(applied: f64, revert_to: f64) -> Option<f64> {
    ((applied - revert_to).abs() > FONT_REPORT_TOLERANCE).then_some(revert_to)
}

/// The libghostty binding action that puts a terminal back at `before` after a config broadcast left it at `live`
/// (the appearance flip keeping zoom): none when the size survived, which a size the user stepped does. The action,
/// unlike a `font-size` config line, leaves the size `reset_font_size` returns to alone.
#[must_use]
pub fn font_size_restore_action(live: Option<f64>, before: f64) -> Option<String> {
    let kept = live.is_some_and(|live| (live - before).abs() < 0.01);
    (!kept).then(|| format!("set_font_size:{before}"))
}

struct Inner {
    key: SurfaceKey,
    /// Weak: the area owns this state through its `destroy` handler, so a strong reference here would be a cycle.
    area: glib::WeakRef<gtk::GLArea>,
    im: gtk::IMMulticontext,
    spec: SurfaceSpec,
    ghostty: RefCell<Option<Surface>>,
    torn_down: Cell<bool>,
    keys: RefCell<KeyState>,
    pointer: RefCell<PointerState>,
    pwd: RefCell<Option<String>>,
    on_title: RefCell<Vec<ReportHandler>>,
    on_pwd: RefCell<Vec<ReportHandler>>,
    on_search: RefCell<Vec<SearchHandler>>,
    on_open_url: RefCell<Vec<ReportHandler>>,
    on_keystroke: RefCell<Vec<KeystrokeHandler>>,
    on_focus_in: RefCell<Vec<FocusHandler>>,
    on_created: RefCell<Vec<FocusHandler>>,
    on_notification: RefCell<Vec<NotificationHandler>>,
    on_font_size: RefCell<Vec<FontSizeHandler>>,
    on_exit: RefCell<Option<ExitHandler>>,
    /// The input method is being reset to end a composition this side already committed or dropped: what it emits
    /// meanwhile is ignored (upstream's `committingComposition`).
    resetting_im: Cell<bool>,
    exit_handled: Cell<bool>,
    /// The first frame was drawn (logged once).
    drew: Cell<bool>,
    /// Whether the deck shows this terminal (the selected session's page); a hidden one is occluded.
    deck_visible: Cell<bool>,
    /// The pending creation tick, while the area is realized and the surface not created yet.
    creation_tick: RefCell<Option<gtk::TickCallbackId>>,
    /// This terminal's own config overlay (a session background), without the zoom-keeping font size, which is read
    /// when it applies; `None` until one is set, so a terminal that never had one keeps the app config.
    config_overlay: RefCell<Option<String>>,
    /// The dashboard's transient font size while the terminal sits in a grid cell (upstream's
    /// `dashboardFontOverride`): its cell-size reports are not the pane's.
    font_override: Cell<Option<f64>>,
    /// The size a cleared override reverts to, whose report is dropped (upstream's `pendingFontRestore`).
    pending_font_restore: Cell<Option<f64>>,
    /// The start size is the terminal's for its whole life (a HUD, measured at it; upstream's `restoreHudFontSize`):
    /// every config re-apply restates it.
    keeps_start_font_size: Cell<bool>,
}

thread_local! {
    static SURFACES: RefCell<HashMap<SurfaceKey, Weak<Inner>>> = RefCell::new(HashMap::new());
    static NEXT_KEY: Cell<usize> = const { Cell::new(1) };
}

/// The live surface libghostty knows by `key`, if it still exists and is not torn down.
pub fn lookup(key: SurfaceKey) -> Option<TerminalSurface> {
    let weak = SURFACES.with(|surfaces| surfaces.borrow().get(&key).cloned())?;
    TerminalSurface::upgrade(&weak).filter(|surface| !surface.inner.torn_down.get())
}

/// Tear down every live surface, ending their processes: the quit path, which leaves the widgets to the process exit.
pub fn teardown_all() {
    let live: Vec<Weak<Inner>> = SURFACES.with(|surfaces| surfaces.borrow().values().cloned().collect());
    for surface in live.iter().filter_map(TerminalSurface::upgrade) {
        surface.teardown();
    }
}

/// Every live terminal that is not torn down: the session panes, the overlays and HUDs, the quick terminal.
pub fn all() -> Vec<TerminalSurface> {
    let live: Vec<Weak<Inner>> = SURFACES.with(|surfaces| surfaces.borrow().values().cloned().collect());
    live.iter()
        .filter_map(TerminalSurface::upgrade)
        .filter(|surface| !surface.inner.torn_down.get())
        .collect()
}

/// How many surfaces the registry holds (each one's area is alive).
#[cfg(test)]
fn registered() -> usize {
    SURFACES.with(|surfaces| surfaces.borrow().len())
}

fn next_key() -> SurfaceKey {
    NEXT_KEY.with(|next| {
        let value = next.get();
        next.set(value.saturating_add(1));
        SurfaceKey::new(NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MAX))
    })
}

/// A terminal widget. Cheap to clone; the area owns the surface, and its `destroy` releases it (module docs).
#[derive(Clone)]
pub struct TerminalSurface {
    inner: Rc<Inner>,
    area: gtk::GLArea,
}

impl TerminalSurface {
    #[must_use]
    pub fn new(spec: SurfaceSpec) -> Self {
        let area = gtk::GLArea::builder()
            .allowed_apis(gdk::GLAPI::GL)
            .has_depth_buffer(false)
            .has_stencil_buffer(false)
            .focusable(true)
            .focus_on_click(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        let inner = Rc::new(Inner {
            key: next_key(),
            area: area.downgrade(),
            im: gtk::IMMulticontext::new(),
            spec,
            ghostty: RefCell::new(None),
            torn_down: Cell::new(false),
            keys: RefCell::new(KeyState::default()),
            pointer: RefCell::new(PointerState::default()),
            pwd: RefCell::new(None),
            on_title: RefCell::new(Vec::new()),
            on_pwd: RefCell::new(Vec::new()),
            on_search: RefCell::new(Vec::new()),
            on_open_url: RefCell::new(Vec::new()),
            on_keystroke: RefCell::new(Vec::new()),
            on_focus_in: RefCell::new(Vec::new()),
            on_created: RefCell::new(Vec::new()),
            on_notification: RefCell::new(Vec::new()),
            on_font_size: RefCell::new(Vec::new()),
            on_exit: RefCell::new(None),
            resetting_im: Cell::new(false),
            exit_handled: Cell::new(false),
            drew: Cell::new(false),
            deck_visible: Cell::new(true),
            creation_tick: RefCell::new(None),
            config_overlay: RefCell::new(None),
            font_override: Cell::new(None),
            pending_font_restore: Cell::new(None),
            keeps_start_font_size: Cell::new(false),
        });
        SURFACES.with(|surfaces| surfaces.borrow_mut().insert(inner.key, Rc::downgrade(&inner)));
        let surface = Self { inner, area };
        surface.connect_area();
        surface.connect_keyboard();
        surface.connect_pointer();
        surface.connect_focus();
        // the widget owns the state from here: the handler's reference goes when dispose drops the area's handlers
        let owner = Rc::clone(&surface.inner);
        surface.area.connect_destroy(move |area| {
            let surface = Self {
                inner: Rc::clone(&owner),
                area: area.clone(),
            };
            surface.teardown();
            SURFACES.with(|surfaces| surfaces.borrow_mut().remove(&surface.inner.key));
        });
        surface
    }

    /// The widget to place in a container.
    #[must_use]
    pub fn widget(&self) -> &gtk::GLArea {
        &self.area
    }

    /// Adds a handler called with each title a program sets (or the user's static one), empty ones included.
    pub fn connect_title(&self, handler: impl Fn(&str) + 'static) {
        self.inner.on_title.borrow_mut().push(Box::new(handler));
    }

    /// Adds a handler called with each working directory the shell reports (OSC 7).
    pub fn connect_pwd(&self, handler: impl Fn(&str) + 'static) {
        self.inner.on_pwd.borrow_mut().push(Box::new(handler));
    }

    /// Adds a handler called with each report libghostty makes on this terminal's search (start, end, match count,
    /// selected match).
    pub fn connect_search(&self, handler: impl Fn(&SearchReport) + 'static) {
        self.inner.on_search.borrow_mut().push(Box::new(handler));
    }

    /// Adds a handler called with each link clicked in the terminal (`GHOSTTY_ACTION_OPEN_URL`), which then owns
    /// opening it. A terminal with none opens links as a sessionless one (see [`TerminalSurface::open_url`]).
    pub fn connect_open_url(&self, handler: impl Fn(&str) + 'static) {
        self.inner.on_open_url.borrow_mut().push(Box::new(handler));
    }

    /// Adds a handler called with each physical key press or repeat the terminal receives, classified for the status
    /// glyph, before the input method or libghostty sees it (upstream's `onUserInputClearsStatus` from `keyDown`).
    /// Bare modifier keys are not reported.
    pub fn connect_keystroke(&self, handler: impl Fn(StatusKeystroke) + 'static) {
        self.inner.on_keystroke.borrow_mut().push(Box::new(handler));
    }

    /// Adds a handler called each time the area gains keyboard focus, whoever moved it there.
    pub fn connect_focus_in(&self, handler: impl Fn() + 'static) {
        self.inner.on_focus_in.borrow_mut().push(Box::new(handler));
    }

    /// Adds a handler called once the libghostty surface exists (it then has a grid and a cell).
    pub fn connect_created(&self, handler: impl Fn() + 'static) {
        self.inner.on_created.borrow_mut().push(Box::new(handler));
    }

    /// Adds a handler called with the title and body of each desktop notification the terminal's program posts
    /// (OSC 9/777, `GHOSTTY_ACTION_DESKTOP_NOTIFICATION`).
    pub fn connect_notification(&self, handler: impl Fn(&str, &str) + 'static) {
        self.inner.on_notification.borrow_mut().push(Box::new(handler));
    }

    /// Hands a desktop notification to the notification handlers.
    pub fn desktop_notification(&self, title: &str, body: &str) {
        for handler in self.inner.on_notification.borrow().iter() {
            handler(title, body);
        }
    }

    /// Adds a handler called with the terminal's live font size after a cell-size change (a font key or command, a DPI
    /// change), except while the dashboard overrides it and for the revert when that ends (upstream's
    /// `onFontSizeChange`).
    pub fn connect_font_size(&self, handler: impl Fn(f64) + 'static) {
        self.inner.on_font_size.borrow_mut().push(Box::new(handler));
    }

    /// libghostty reported a new cell size (`GHOSTTY_ACTION_CELL_SIZE`): a trigger only, the live font size is read and
    /// reported off the callback, as upstream hops to the main queue.
    pub fn cell_size_changed(&self) {
        let weak = self.weak();
        glib::idle_add_local_once(move || {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.report_font_size();
            }
        });
    }

    fn report_font_size(&self) {
        let Some(size) = self.font_size() else {
            return;
        };
        let (report, pending) = font_report(
            self.inner.font_override.get(),
            self.inner.pending_font_restore.get(),
            size,
        );
        self.inner.pending_font_restore.set(pending);
        if report {
            for handler in self.inner.on_font_size.borrow().iter() {
                handler(size);
            }
        }
    }

    /// The dashboard sets this terminal's font to its transient `size`: reports stop being the pane's.
    pub fn set_font_override(&self, size: f64) {
        self.inner.font_override.set(Some(size));
        self.inner.pending_font_restore.set(None);
    }

    /// The dashboard gave the terminal back its own size `revert_to`: the report of that revert is dropped.
    pub fn clear_font_override(&self, revert_to: f64) {
        if let Some(applied) = self.inner.font_override.take() {
            self.inner.pending_font_restore.set(pending_restore(applied, revert_to));
        }
    }

    /// Called once when the terminal's process ends.
    pub fn connect_exit(&self, handler: impl FnOnce() + 'static) {
        *self.inner.on_exit.borrow_mut() = Some(Box::new(handler));
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.inner)
    }

    /// A handle again, while both the state and its area are alive.
    fn upgrade(weak: &Weak<Inner>) -> Option<Self> {
        let inner = weak.upgrade()?;
        let area = inner.area.upgrade()?;
        Some(Self { inner, area })
    }

    /// Run `f` on the libghostty surface, if it exists. The borrow is shared, so callbacks libghostty makes during
    /// `f` may use the surface too.
    fn with_surface<R>(&self, f: impl FnOnce(&Surface) -> R) -> Option<R> {
        self.inner.ghostty.borrow().as_ref().map(f)
    }

    fn connect_area(&self) {
        let area = &self.area;
        let weak = self.weak();
        area.connect_realize(move |area| {
            let Some(surface) = Self::upgrade(&weak) else { return };
            area.make_current();
            if let Some(err) = area.error() {
                tracing::error!(%err, "the terminal's GtkGLArea has no OpenGL context");
            }
            surface.inner.im.set_client_widget(Some(area));
            surface.arm_creation_tick();
        });
        let weak = self.weak();
        area.connect_unrealize(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.teardown();
            }
        });
        let weak = self.weak();
        area.connect_resize(move |_, width, height| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.resized(width, height);
            }
        });
        let weak = self.weak();
        area.connect_render(move |_, _| {
            if let Some(surface) = Self::upgrade(&weak)
                && surface.with_surface(Surface::draw).is_some()
                && !surface.inner.drew.replace(true)
            {
                tracing::debug!(key = surface.inner.key.get(), "terminal drew its first frame");
            }
            glib::Propagation::Stop
        });
        let weak = self.weak();
        area.connect_map(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                let visible = surface.inner.deck_visible.get();
                surface.with_surface(|s| s.set_occlusion(visible));
            }
        });
        let weak = self.weak();
        area.connect_unmap(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.with_surface(|s| s.set_occlusion(false));
            }
        });
    }

    /// Creates the surface on the first frame-clock tick at which the realized area has a nonzero allocation,
    /// whether or not it is drawn (module docs). One tick per frame until then; none once created or torn down.
    fn arm_creation_tick(&self) {
        if self.inner.torn_down.get() || self.inner.ghostty.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let id = self.area.add_tick_callback(move |area, _| {
            let Some(surface) = Self::upgrade(&weak) else {
                return glib::ControlFlow::Break;
            };
            if surface.inner.torn_down.get() || surface.inner.ghostty.borrow().is_some() {
                surface.inner.creation_tick.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            let scale = area.scale_factor();
            let (width, height) = (area.width() * scale, area.height() * scale);
            if width <= 0 || height <= 0 {
                return glib::ControlFlow::Continue;
            }
            surface.inner.creation_tick.borrow_mut().take();
            surface.resized(width, height);
            glib::ControlFlow::Break
        });
        if let Some(previous) = self.inner.creation_tick.replace(Some(id)) {
            previous.remove();
        }
    }

    /// Shows or hides the terminal as a deck page: only the shown one is focusable and visible to its programs
    /// (occlusion, upstream's `deckOnScreen`); a hidden one keeps its surface and shell running.
    pub fn set_deck_visible(&self, visible: bool) {
        self.inner.deck_visible.set(visible);
        self.area.set_focusable(visible);
        self.area.set_can_target(visible);
        if self.area.is_mapped() {
            self.with_surface(|s| s.set_occlusion(visible));
        }
        if !visible {
            self.release_gpu_resources();
        }
    }

    /// Frees a hidden terminal's GPU resources (render targets, atlas textures, buffers). The OpenGL renderer draws
    /// on this thread, so occlusion alone frees nothing (libghostty releases on occlusion only where it draws on its
    /// own thread); the objects die in whatever context is current, hence the area's. A hidden page is never
    /// snapshotted, so nothing draws until it shows again, and that draw rebuilds everything.
    fn release_gpu_resources(&self) {
        if !self.area.is_realized() {
            return;
        }
        self.area.make_current();
        if self.area.error().is_some() {
            return;
        }
        self.with_surface(Surface::release_gpu_resources);
    }

    /// Whether the deck shows this terminal.
    #[must_use]
    pub fn is_deck_visible(&self) -> bool {
        self.inner.deck_visible.get()
    }

    /// The area's drawable changed size (or scale): create the surface the first time, else push the new size.
    fn resized(&self, width: i32, height: i32) {
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return;
        };
        if width == 0 || height == 0 {
            return;
        }
        let scale = f64::from(self.area.scale_factor());
        if self.inner.ghostty.borrow().is_none() {
            self.create(scale);
        }
        self.with_surface(|surface| {
            surface.set_content_scale(scale, scale);
            surface.set_size(width, height);
            // a size push that keeps the grid draws nothing new (libghostty.md)
            surface.refresh();
        });
    }

    fn create(&self, scale: f64) {
        if self.inner.torn_down.get() {
            return;
        }
        let area = &self.area;
        area.make_current();
        if area.error().is_some() {
            return;
        }
        let spec = &self.inner.spec;
        let options = SurfaceOptions {
            scale_factor: scale,
            working_directory: spec.working_directory.as_deref(),
            command: spec.command.as_deref(),
            env: &spec.env,
            wait_after_command: spec.wait_after_command,
            font_size: spec.font_size,
            ..SurfaceOptions::default()
        };
        match ghostty_app::create_surface(self.inner.key, &options) {
            Ok(surface) => {
                tracing::debug!(key = self.inner.key.get(), scale, "terminal surface created");
                surface.set_focus(area.has_focus());
                surface.set_occlusion(self.inner.deck_visible.get() && area.is_mapped());
                *self.inner.ghostty.borrow_mut() = Some(surface);
                if let Some(title) = ghostty_app::static_title() {
                    self.apply_title(&title);
                }
                self.apply_config_overlay();
                if !self.inner.deck_visible.get() {
                    self.release_gpu_resources();
                }
                for handler in self.inner.on_created.borrow().iter() {
                    handler();
                }
            }
            Err(err) => tracing::error!(%err, "could not create the terminal surface"),
        }
    }

    /// Free the libghostty surface and end its process, once; later calls do nothing.
    pub fn teardown(&self) {
        if self.inner.torn_down.replace(true) {
            return;
        }
        // libghostty frees its GL objects in the area's context; an area never realized has no context and no surface
        if self.area.is_realized() {
            self.area.make_current();
        }
        let Ok(mut slot) = self.inner.ghostty.try_borrow_mut() else {
            tracing::error!("terminal surface torn down while in use; it leaks");
            return;
        };
        drop(slot.take());
        if let Some(tick) = self.inner.creation_tick.borrow_mut().take() {
            tick.remove();
        }
        self.inner.im.set_client_widget(None::<&gtk::Widget>);
    }

    /// Whether the libghostty surface exists: created at the realized area's first nonzero size and not torn down
    /// since.
    pub fn is_realized(&self) -> bool {
        !self.inner.torn_down.get() && self.inner.ghostty.try_borrow().is_ok_and(|surface| surface.is_some())
    }

    /// The key libghostty knows this terminal by, which [`lookup`] resolves back to it.
    #[must_use]
    pub fn key(&self) -> SurfaceKey {
        self.inner.key
    }

    /// Moves keyboard focus to the terminal; whether it took it.
    pub fn grab_focus(&self) -> bool {
        self.area.grab_focus()
    }

    pub fn queue_render(&self) {
        self.area.queue_render();
    }

    pub fn apply_title(&self, title: &str) {
        for handler in self.inner.on_title.borrow().iter() {
            handler(title);
        }
    }

    pub fn apply_pwd(&self, pwd: String) {
        for handler in self.inner.on_pwd.borrow().iter() {
            handler(&pwd);
        }
        *self.inner.pwd.borrow_mut() = Some(pwd);
    }

    pub fn report_search(&self, report: &SearchReport) {
        for handler in self.inner.on_search.borrow().iter() {
            handler(report);
        }
    }

    /// A link clicked in the terminal goes to the open-url handlers, or, with none connected, through the link policy
    /// as a click from a terminal with no session (upstream's `.quick` origin: the browser, whatever the mode).
    pub fn open_url(&self, url: &str) {
        let handlers = self.inner.on_open_url.borrow();
        if handlers.is_empty() {
            ghostty_app::follow_link(url, LinkOpenMode::Browser, ClickOrigin::Quick);
        }
        for handler in handlers.iter() {
            handler(url);
        }
    }

    pub fn set_mouse_shape(&self, shape: MouseShape) {
        self.inner.pointer.borrow_mut().shape = Some(shape);
        self.apply_cursor();
    }

    pub fn set_mouse_hidden(&self, hidden: bool) {
        self.inner.pointer.borrow_mut().hidden = hidden;
        self.apply_cursor();
    }

    fn apply_cursor(&self) {
        let pointer = self.inner.pointer.borrow();
        let name = if pointer.hidden {
            "none"
        } else {
            pointer.shape.unwrap_or(MouseShape::Text).css_name()
        };
        self.area.set_cursor_from_name(Some(name));
    }

    /// The child exited: a command surface that should vanish closes now and suppresses libghostty's prompt (`true`);
    /// any other keeps the prompt, and its close arrives through the close callback.
    pub fn child_exited(&self) -> bool {
        let spec = &self.inner.spec;
        if spec.command.is_none() || spec.wait_after_command {
            return false;
        }
        let weak = self.weak();
        glib::idle_add_local_once(move || {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.handle_process_exit();
            }
        });
        true
    }

    /// The process ended: run the exit handler once.
    pub fn handle_process_exit(&self) {
        if self.inner.exit_handled.replace(true) {
            return;
        }
        let handler = self.inner.on_exit.borrow_mut().take();
        if let Some(handler) = handler {
            handler();
        }
    }

    /// Complete a clipboard request this surface owns.
    pub fn complete_clipboard_request(
        &self,
        pending: PendingRequest,
        contents: &[ClipboardContent],
        available: &[&str],
        confirmed: bool,
    ) {
        let refused = self.with_surface(|s| s.complete_clipboard_request(pending, contents, available, confirmed));
        if matches!(refused, Some(Err(_))) {
            tracing::error!("clipboard completion for another surface dropped");
        }
    }

    /// Deny a clipboard request this surface owns.
    pub fn deny_clipboard_request(&self, pending: PendingRequest) {
        let refused = self.with_surface(|s| s.deny_clipboard_request(pending));
        if matches!(refused, Some(Err(_))) {
            tracing::error!("clipboard denial for another surface dropped");
        }
    }

    /// Types `text` into the terminal as keystrokes through the keyboard's path (upstream's `inject(text:)`), never as
    /// a paste: `ghostty_surface_text`'s bracketed-paste wrapping would keep a trailing newline from submitting. Each
    /// run is one key press carrying the text, each line ending a Return keypress, and the final Return after text
    /// waits [`SUBMIT_GAP`] (a blocking sleep: a deferred Return could be overtaken by another injection or a
    /// keystroke). A live composition is committed first. False, sending nothing, when the surface does not exist.
    pub fn type_text(&self, text: &str) -> bool {
        if !self.is_realized() {
            return false;
        }
        // typing under a live composition would leave it to re-commit on the user's next key, after the injected text
        self.commit_or_discard_composition();
        let paced = keystrokes::paced(text);
        for segment in &paced.head {
            self.send_keys(&input::segment_keys(segment));
        }
        if paced.paced_return {
            std::thread::sleep(SUBMIT_GAP);
            self.send_keys(&input::return_keys());
        }
        true
    }

    fn send_keys(&self, keys: &[KeyEvent]) {
        self.with_surface(|s| {
            for key in keys {
                let _ = s.send_key(key);
            }
        });
    }

    /// Ends a live input-method composition before input that does not come through it lands (upstream's
    /// `commitOrDiscardComposition`): its text goes in as typed, an empty one is dropped, and the input method is
    /// reset so it does not commit the same text again. A no-op unless this terminal is composing.
    fn commit_or_discard_composition(&self) {
        if !std::mem::take(&mut self.inner.keys.borrow_mut().composing) {
            return;
        }
        let (preedit, _, _) = self.inner.im.preedit_string();
        self.with_surface(|s| s.preedit(None));
        if !preedit.is_empty() {
            self.send_keys(&[input::text_key(&preedit)]);
        }
        self.inner.resetting_im.set(true);
        self.inner.im.reset();
        self.inner.resetting_im.set(false);
    }

    /// The terminal's text for `scope` (upstream's `readScreenText` without `lines`, which the core applies): the
    /// viewport, or the screen with its scrollback, trailing blank rows included; empty for a blank screen. `None`
    /// when the surface does not exist or the read failed.
    pub fn read_text(&self, scope: TextScope) -> Option<String> {
        let tag = match scope {
            TextScope::Screen => PointTag::Viewport,
            TextScope::All => PointTag::Screen,
        };
        self.with_surface(|s| s.read_text(Selection::whole(tag)))
            .flatten()
            .map(|read| read.text)
    }

    /// The selection's text; `None` with no or an empty selection, or no surface.
    pub fn read_selection(&self) -> Option<String> {
        self.with_surface(Surface::read_selection)
            .flatten()
            .map(|read| read.text)
            .filter(|text| !text.is_empty())
    }

    /// The cursor's zero-based column (upstream's `readCursorColumn`, see [`agterm_ghostty::solve_cursor_column`]).
    pub fn cursor_column(&self) -> Option<u32> {
        self.with_surface(Surface::cursor_column).flatten()
    }

    /// Runs a libghostty binding action (upstream's `performBindingAction`); true when the surface exists, whatever
    /// libghostty made of the action.
    pub fn binding_action(&self, action: &str) -> bool {
        self.with_surface(|s| {
            let _ = s.binding_action(action);
        })
        .is_some()
    }

    /// The terminal's live font size in points.
    pub fn font_size(&self) -> Option<f64> {
        self.with_surface(Surface::font_size).flatten()
    }

    /// The cell the terminal draws, width and height in logical pixels (points).
    pub fn cell_size(&self) -> Option<(f64, f64)> {
        let scale = f64::from(self.area.scale_factor().max(1));
        let size = self.with_surface(Surface::size)?;
        (size.cell_width_px > 0 && size.cell_height_px > 0).then(|| {
            (
                f64::from(size.cell_width_px) / scale,
                f64::from(size.cell_height_px) / scale,
            )
        })
    }

    /// Reports the pointer at (`x`, `y`) in the terminal's own coordinates to a terminal that takes no pointer events
    /// itself (a passive HUD panel, upstream's `passivePointer`); a negative point takes it away.
    pub fn passive_pointer(&self, x: f64, y: f64, mods: agterm_ghostty::input::Mods) {
        self.with_surface(|s| s.mouse_pos(x, y, mods));
    }

    /// Forwards a primary-button press or release at (`x`, `y`) to a terminal that takes no clicks itself, without
    /// moving focus (upstream's `passiveClick`): libghostty resolves a link under it and opens it.
    pub fn passive_click(&self, state: MouseState, x: f64, y: f64, mods: agterm_ghostty::input::Mods) {
        self.with_surface(|s| {
            s.mouse_pos(x, y, mods);
            let _ = s.mouse_button(state, agterm_ghostty::input::MouseButton::Left, mods);
        });
    }

    /// The pid of the terminal's foreground process.
    pub fn foreground_pid(&self) -> Option<u32> {
        self.with_surface(Surface::foreground_pid).flatten()
    }

    /// Sets this terminal's own config overlay (upstream's per-surface `applyWatermarkFromSession`): the app config
    /// with `overlay` loaded last, plus a `font-size` line keeping the terminal's live zoom, which a config update
    /// would otherwise reset. `""` returns to the app config. Applied now when the surface exists, else once it is
    /// created; an unchanged overlay does nothing.
    pub fn set_config_overlay(&self, overlay: &str) {
        if self.inner.config_overlay.borrow().as_deref() == Some(overlay) {
            return;
        }
        *self.inner.config_overlay.borrow_mut() = Some(overlay.to_owned());
        self.apply_config_overlay();
    }

    /// Pins the terminal's start size (its spec's `font_size`) for its whole life: every config re-apply restates it,
    /// as a HUD's panel was measured at it (upstream's `currentEffectiveFontSize` for a HUD).
    pub fn keep_start_font_size(&self) {
        self.inner.keeps_start_font_size.set(true);
    }

    /// The pinned start size, when [`Self::keep_start_font_size`] set one.
    fn pinned_font_size(&self) -> Option<f64> {
        self.inner
            .keeps_start_font_size
            .get()
            .then(|| self.inner.spec.font_size.map(f64::from))
            .flatten()
    }

    /// The first half of a config reload, run on every terminal BEFORE the app takes the new config (upstream's
    /// `syncColorScheme` ahead of `ghostty_app_update_config`): the terminal takes the color scheme, as libghostty
    /// resolves a dual theme against it when the app re-feeds every surface, and its live font size is read while it
    /// still holds it, as that re-feed resets a size the user did not step (a restored session's). The size, `None`
    /// for a terminal not created yet.
    pub fn prepare_reload(&self, scheme: agterm_ghostty::ColorScheme) -> Option<f64> {
        if !self.is_realized() {
            return None;
        }
        if self.area.is_realized() {
            self.area.make_current();
        }
        self.with_surface(|surface| {
            surface.set_color_scheme(scheme);
            surface.font_size()
        })
        .flatten()
    }

    /// The app config was rebuilt and the app re-fed it to every terminal (a theme change, a settings change, an
    /// appearance flip; upstream's `GhosttyApp.reloadConfig` and `reapplySessionConfigIfNeeded`), after
    /// [`Self::prepare_reload`] read `size_before`: the terminal takes its own overlay rebuilt over the new files, or
    /// `config` itself. With `keep_zoom` it restates `size_before`; otherwise an overlay restates the size libghostty
    /// left (it keeps only a size the user stepped). A pinned start size wins over both. A terminal not created yet
    /// takes the new config when it is.
    ///
    /// Without an overlay or a pinned size the terminal takes `config` and keeps `size_before` through libghostty's
    /// `set_font_size` action ([`font_size_restore_action`]), never a `font-size` config line: libghostty would make
    /// that line the size `font.reset` returns to. The action marks the size as stepped, so a restored session's
    /// size then lives like a zoom the user stepped.
    pub fn reload_config(&self, config: &agterm_ghostty::Config, size_before: Option<f64>, keep_zoom: bool) {
        if !self.is_realized() {
            return;
        }
        if self.area.is_realized() {
            self.area.make_current();
        }
        let pinned = self.pinned_font_size();
        if self.inner.config_overlay.borrow().is_some() {
            if let Some(live) = self.with_surface(Surface::font_size) {
                self.push_config_overlay(pinned.or_else(|| size_before.filter(|_| keep_zoom)).or(live));
            }
            return;
        }
        if let Some(size) = pinned {
            let pinned =
                ghostty_app::config_with_overlay(&agterm_core::watermark::overlay_text(None, None, Some(size), 1.0));
            self.with_surface(|surface| surface.update_config(pinned.as_ref().unwrap_or(config)));
            return;
        }
        self.with_surface(|surface| {
            surface.update_config(config);
            let restore = size_before
                .filter(|_| keep_zoom)
                .and_then(|before| font_size_restore_action(surface.font_size(), before));
            if let Some(action) = restore
                && !surface.binding_action(&action)
            {
                tracing::warn!(%action, "could not restore the font size after a config reload");
            }
        });
    }

    /// Pushes the stored overlay to the surface, if both exist, at the pinned start size or the live one.
    fn apply_config_overlay(&self) {
        if let Some(live) = self.with_surface(Surface::font_size) {
            self.push_config_overlay(self.pinned_font_size().or(live));
        }
    }

    /// Pushes the stored overlay with a `font-size` line for `font_size`; libghostty clones the config, so it is freed
    /// here.
    fn push_config_overlay(&self, font_size: Option<f64>) {
        let Some(overlay) = self.inner.config_overlay.borrow().clone() else {
            return;
        };
        let mut text = overlay;
        // with no background the overlay is the font-size line alone
        text.push_str(&agterm_core::watermark::overlay_text(None, None, font_size, 1.0));
        let Some(config) = ghostty_app::config_with_overlay(&text) else {
            return;
        };
        // a config change may touch the renderer, whose objects live in the area's context
        if self.area.is_realized() {
            self.area.make_current();
        }
        self.with_surface(|surface| surface.update_config(&config));
    }

    fn connect_focus(&self) {
        let focus = gtk::EventControllerFocus::new();
        let weak = self.weak();
        focus.connect_enter(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.focus_changed(true);
            }
        });
        let weak = self.weak();
        focus.connect_leave(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.focus_changed(false);
            }
        });
        self.area.add_controller(focus);
    }

    fn focus_changed(&self, focused: bool) {
        self.with_surface(|s| s.set_focus(focused));
        if focused {
            self.inner.im.focus_in();
            for handler in self.inner.on_focus_in.borrow().iter() {
                handler();
            }
        } else {
            self.inner.im.focus_out();
            // the releases of keys held now go to whatever takes focus
            self.inner.keys.borrow_mut().pressed.clear();
        }
    }

    fn connect_keyboard(&self) {
        let keys = gtk::EventControllerKey::new();
        let weak = self.weak();
        keys.connect_key_pressed(move |controller, keyval, keycode, state| {
            let handled = Self::upgrade(&weak)
                .is_some_and(|surface| surface.key_event(controller, KeyAction::Press, keyval, keycode, state));
            glib::Propagation::from(handled)
        });
        let weak = self.weak();
        keys.connect_key_released(move |controller, keyval, keycode, state| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.key_event(controller, KeyAction::Release, keyval, keycode, state);
            }
        });
        self.area.add_controller(keys);
        self.connect_input_method();
    }

    fn connect_input_method(&self) {
        let im = &self.inner.im;
        let weak = self.weak();
        im.connect_preedit_start(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                let mut keys = surface.inner.keys.borrow_mut();
                keys.composing = true;
                keys.committed.clear();
            }
        });
        let weak = self.weak();
        im.connect_preedit_changed(move |im| {
            if let Some(surface) = Self::upgrade(&weak)
                && !surface.inner.resetting_im.get()
            {
                // fcitx5-hangul can commit and keep composing without a new preedit-start
                surface.inner.keys.borrow_mut().composing = true;
                let (text, _, _) = im.preedit_string();
                surface.with_surface(|s| s.preedit(Some(text.as_str())));
            }
        });
        let weak = self.weak();
        im.connect_preedit_end(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.inner.keys.borrow_mut().composing = false;
                surface.with_surface(|s| s.preedit(None));
            }
        });
        let weak = self.weak();
        im.connect_commit(move |_, text| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.im_commit(text);
            }
        });
    }

    /// Text the input method committed: inside a plain key event it rides with the key (so libghostty knows which key
    /// made it); a composition's result or text from elsewhere (an on-screen keyboard) goes in on its own.
    fn im_commit(&self, text: &str) {
        // a reset ending a composition this side already handled may commit it once more
        if self.inner.resetting_im.get() {
            return;
        }
        {
            let mut keys = self.inner.keys.borrow_mut();
            if keys.phase == KeyPhase::NotComposing {
                keys.committed.push_str(text);
                return;
            }
            // a commit ends the composition
            keys.composing = false;
        }
        // well-behaved input methods end the preedit themselves, some (ibus 1.5.29) do not
        self.with_surface(|s| s.preedit(None));
        self.send_keys(&[input::text_key(text)]);
    }

    /// Pass the event through the input method; `true` when it consumed the key (ghostty's GTK runtime's rules).
    fn filtered_by_input_method(&self, event: &gdk::Event) -> bool {
        {
            let mut keys = self.inner.keys.borrow_mut();
            keys.phase = if keys.composing {
                KeyPhase::Composing
            } else {
                KeyPhase::NotComposing
            };
        }
        if let Some(point) = self.with_surface(Surface::ime_point) {
            #[expect(clippy::cast_possible_truncation, reason = "widget coordinates fit an i32 by far")]
            let rect = gdk::Rectangle::new(point.x as i32, point.y as i32, 1, 1);
            self.inner.im.set_cursor_location(&rect);
        }
        let handled = self.inner.im.filter_keypress(event);
        let mut keys = self.inner.keys.borrow_mut();
        let was_composing = keys.phase == KeyPhase::Composing;
        keys.phase = KeyPhase::Outside;
        // composing, or finishing a composition (its commit already went in), or a key the method swallowed whole
        handled && (keys.composing || was_composing || keys.committed.is_empty())
    }

    fn key_event(
        &self,
        controller: &gtk::EventControllerKey,
        action: KeyAction,
        keyval: gdk::Key,
        keycode: u32,
        state: gdk::ModifierType,
    ) -> bool {
        let Some(event) = controller.current_event() else {
            return false;
        };
        // before the input method: a release it swallows must still lift the key
        let action = self.inner.keys.borrow_mut().pressed.track(action, keycode);
        let key_event = event.downcast_ref::<gdk::KeyEvent>();
        if action != KeyAction::Release
            && let Some(key_event) = key_event
        {
            self.report_keystroke(key_event, keyval, keycode, state);
        }
        if self.filtered_by_input_method(&event) {
            self.inner.keys.borrow_mut().committed.clear();
            return true;
        }
        let (text, composing) = {
            let mut keys = self.inner.keys.borrow_mut();
            let committed = std::mem::take(&mut keys.committed);
            let text = if committed.is_empty() {
                input::keyval_text(keyval)
            } else {
                Some(committed)
            };
            (text, keys.composing)
        };
        let num_lock = event.device().is_some_and(|device| device.is_num_locked());
        let consumed = key_event.map_or(gdk::ModifierType::empty(), |e| {
            e.consumed_modifiers() & gdk::MODIFIER_MASK
        });
        let key = KeyEvent {
            action,
            mods: input::event_mods(state, keyval, action, num_lock),
            consumed_mods: input::translate_mods(consumed),
            keycode: key_event.map_or(keycode, |e| input::remapped_keycode(keycode, keyval, e.is_modifier())),
            text,
            unshifted_codepoint: key_event.map_or(0, |e| unshifted_codepoint(&self.area, e, keycode)),
            composing,
        };
        let used = self.with_surface(|s| s.send_key(&key)).unwrap_or(false);
        if used && action != KeyAction::Release && composing {
            self.inner.im.reset();
            self.with_surface(|s| s.preedit(None));
        }
        used
    }

    /// Every press reaches the status clear, whatever consumes it next (upstream classifies first thing in
    /// `keyDown`); the pane-scoped decision is the model's.
    fn report_keystroke(&self, event: &gdk::KeyEvent, keyval: gdk::Key, keycode: u32, state: gdk::ModifierType) {
        let is_modifier = event.is_modifier();
        let keycode = input::remapped_keycode(keycode, keyval, is_modifier);
        if let Some(keystroke) = input::status_keystroke(keycode, keyval, state, is_modifier) {
            for handler in self.inner.on_keystroke.borrow().iter() {
                handler(keystroke);
            }
        }
    }

    fn connect_pointer(&self) {
        let click = gtk::GestureClick::builder().button(0).build();
        let weak = self.weak();
        click.connect_pressed(move |gesture, _, x, y| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.click(gesture, MouseState::Press, x, y);
            }
        });
        let weak = self.weak();
        click.connect_released(move |gesture, _, x, y| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.click(gesture, MouseState::Release, x, y);
            }
        });
        self.area.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        let weak = self.weak();
        motion.connect_motion(move |controller, x, y| {
            if let Some(surface) = Self::upgrade(&weak) {
                let mods = input::translate_mods(controller.current_event_state());
                surface.with_surface(|s| s.mouse_pos(x, y, mods));
            }
        });
        let weak = self.weak();
        motion.connect_enter(move |controller, x, y| {
            if let Some(surface) = Self::upgrade(&weak) {
                let mods = input::translate_mods(controller.current_event_state());
                surface.with_surface(|s| s.mouse_pos(x, y, mods));
            }
        });
        let weak = self.weak();
        motion.connect_leave(move |controller| {
            let state = controller.current_event_state();
            // mid-drag the position stays where the selection is going
            if input::any_button_held(state) {
                return;
            }
            if let Some(surface) = Self::upgrade(&weak) {
                surface.with_surface(|s| s.mouse_pos(-1.0, -1.0, input::translate_mods(state)));
            }
        });
        self.area.add_controller(motion);
        self.connect_scroll();
    }

    fn click(&self, gesture: &gtk::GestureClick, state: MouseState, x: f64, y: f64) {
        let Some(button) = input::mouse_button(gesture.current_button()) else {
            return;
        };
        // only the left button moves focus; right and middle reach libghostty's bindings without it (upstream)
        if state == MouseState::Press && button == agterm_ghostty::input::MouseButton::Left {
            self.grab_focus();
        }
        let mods = input::translate_mods(gesture.current_event_state());
        self.with_surface(|s| {
            s.mouse_pos(x, y, mods);
            // the terminal has no context menu, so whether libghostty used the click does not matter
            let _ = s.mouse_button(state, button, mods);
        });
    }

    fn connect_scroll(&self) {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let weak = self.weak();
        scroll.connect_scroll(move |controller, dx, dy| {
            let Some(surface) = Self::upgrade(&weak) else {
                return glib::Propagation::Proceed;
            };
            let precision = controller.unit() == gdk::ScrollUnit::Surface;
            let scale = f64::from(surface.area.scale_factor());
            let (x, y) = input::scroll_delta(dx, dy, scale, precision);
            surface.with_surface(|s| s.mouse_scroll(x, y, ScrollMods { precision }));
            glib::Propagation::Stop
        });
        self.area.add_controller(scroll);
    }
}

/// The key's character with no modifier in the active layout (level 0 of the event's group), for the Kitty protocol
/// and unicode bindings; 0 when it has none.
fn unshifted_codepoint(widget: &impl IsA<gtk::Widget>, event: &gdk::KeyEvent, keycode: u32) -> u32 {
    let layout = event.layout();
    widget
        .display()
        .map_keycode(keycode)
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|(key, _)| key.group() == i32::try_from(layout).unwrap_or(-1) && key.level() == 0)
        })
        .and_then(|(_, keyval)| keyval.to_unicode())
        .map_or(0, u32::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_under_the_dashboard_override_is_not_the_panes() {
        assert_eq!(font_report(Some(8.0), None, 8.0), (false, None));
        // the pending restore waits for the override to clear
        assert_eq!(font_report(Some(8.0), Some(13.0), 8.0), (false, Some(13.0)));
        assert_eq!(font_report(None, None, 14.0), (true, None));
    }

    #[test]
    fn the_revert_report_is_dropped_once_and_a_later_zoom_persists() {
        assert_eq!(font_report(None, Some(13.0), 13.0), (false, None));
        assert_eq!(font_report(None, Some(13.0), 13.4), (false, None));
        // a report that is not the revert clears the restore and counts
        assert_eq!(font_report(None, Some(13.0), 15.0), (true, None));
    }

    #[test]
    fn a_cleared_override_arms_a_restore_only_for_a_real_change() {
        assert_eq!(pending_restore(8.0, 13.0), Some(13.0));
        assert_eq!(pending_restore(13.2, 13.0), None);
    }

    #[test]
    fn the_area_owns_the_surface_without_a_cycle() {
        crate::gtk_test::on_gtk_thread(the_area_owns_the_surface);
    }

    fn the_area_owns_the_surface() {
        // a handle keeps a surface that is in no container yet
        let unplaced = TerminalSurface::new(SurfaceSpec::default());
        let key = unplaced.inner.key;
        let state = unplaced.weak();
        assert!(lookup(key).is_some());
        drop(unplaced);
        assert!(lookup(key).is_none(), "the last handle freed the unplaced surface");
        assert!(state.upgrade().is_none());
        assert_eq!(registered(), 0);

        // once placed, the container owns it, and dropping the area frees everything
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let terminal = TerminalSurface::new(SurfaceSpec::default());
        let key = terminal.inner.key;
        let state = terminal.weak();
        container.append(terminal.widget());
        drop(terminal);
        let area = lookup(key).map(|surface| surface.widget().clone());
        assert!(area.is_some(), "the container keeps the surface");
        assert_eq!(registered(), 1);
        if let Some(area) = area {
            container.remove(&area);
        }
        assert!(lookup(key).is_none());
        assert_eq!(registered(), 0, "destroy took the key out of the registry");
        assert!(state.upgrade().is_none(), "nothing holds the surface's state");

        let placed = TerminalSurface::new(SurfaceSpec::default());
        let key = placed.inner.key;
        container.append(placed.widget());
        teardown_all();
        assert!(placed.inner.torn_down.get());
        assert!(lookup(key).is_none(), "a torn-down surface is not live");
    }

    #[test]
    fn an_appearance_flip_restores_only_a_size_the_broadcast_reset() {
        // a stepped size survived the broadcast: nothing to restate, so font.reset still returns to the config's size
        assert_eq!(font_size_restore_action(Some(15.0), 15.0), None);
        // a restored session's size the broadcast reset goes back through the action, not a config line
        assert_eq!(
            font_size_restore_action(Some(14.0), 15.0).as_deref(),
            Some("set_font_size:15")
        );
        assert_eq!(
            font_size_restore_action(Some(14.0), 13.5).as_deref(),
            Some("set_font_size:13.5")
        );
        assert_eq!(
            font_size_restore_action(None, 16.0).as_deref(),
            Some("set_font_size:16")
        );
    }

    #[test]
    fn only_a_pinned_start_size_survives_every_reload() {
        crate::gtk_test::on_gtk_thread(a_pinned_start_size_survives_every_reload);
    }

    fn a_pinned_start_size_survives_every_reload() {
        let spec = SurfaceSpec {
            font_size: Some(20.0),
            ..SurfaceSpec::default()
        };
        let restored = TerminalSurface::new(spec.clone());
        assert_eq!(restored.pinned_font_size(), None, "a session's start size is a zoom");
        let hud = TerminalSurface::new(spec);
        hud.keep_start_font_size();
        assert_eq!(hud.pinned_font_size(), Some(20.0));
        let no_size = TerminalSurface::new(SurfaceSpec::default());
        no_size.keep_start_font_size();
        assert_eq!(no_size.pinned_font_size(), None, "no start size, nothing pinned");
        // a terminal libghostty has not created yet has no size to keep and takes the new config once it is
        assert_eq!(restored.prepare_reload(agterm_ghostty::ColorScheme::Dark), None);
    }
}
