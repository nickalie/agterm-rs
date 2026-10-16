//! One window over one model window's store (upstream's `WindowContentView.swift`; ported from the GTK build's
//! `window.rs`): the deck under the window's chrome, titled after the active session, reporting its on-screen state for
//! `window.list`, its activation as frontmost (upstream's `WindowAccessor.reportFrontmost`) and its close (upstream's
//! window `willClose`), and applying the window commands' requests. [`crate::windows`] owns the set of them.
//!
//! The window's content is a column: the title bar (`titlebar.rs`), then the sidebar's place (Task 19) beside the
//! deck, all on the chrome's background (`chrome.rs`); a window the client decorates gets resize handles along its
//! free edges. Over it sit the window's modal layers ([`Layer`]), in the GTK build's order: the scrollback search bar, the
//! GUI ask, the native picker, the command palette, the Ctrl-Tab switcher and the title bar's popovers; each attaches its
//! view to its layer ([`WindowView::set_layer`]), drawn over the whole window while attached. The window's alerts
//! ([`crate::modal`]) come above everything.
//!
//! The GUI ask ([`crate::ask_dialog`]) is the window's own: one comes up with the model's modal slot, takes the
//! keyboard, and is drawn over its anchor while the rest of the window takes no clicks. While a GUI ask, a picker, the
//! palette, an alert or a popover holds the keyboard the deck moves none; once it closes the window's sync gives the
//! keyboard back to the shown terminal (an alert or a popover closing posts that sync).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use agterm_core::model::{AppStore, Session, WindowLibrary};
use agterm_core::titlebar::UNTITLED;
use agterm_core::{Core, WindowRequest, WindowSize, WindowState};
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, KeyUpEvent,
    ParentElement as _, Render, Styled as _, Subscription, Window, div, px, size,
};
use uuid::Uuid;

use crate::ask_dialog::{AskDialog, AskOwner};
use crate::deck::{AnchorTarget, Deck};
use crate::keymap_dispatch::KeyOrigin;
use crate::popovers::Popovers;
use crate::sidebar::Sidebar;
use crate::state::{AppState, FirstFocus, Poster, post};
use crate::titlebar::{Cover, Titlebar, TitlebarEvent};

/// The window title: the active session's display name, then the window's custom name when it has one (upstream's
/// `WindowTitleSync.title`).
#[must_use]
pub fn window_title(library: &WindowLibrary, window: Option<Uuid>) -> String {
    let session = window
        .and_then(|id| library.store(id))
        .and_then(AppStore::active_session)
        .map_or_else(|| UNTITLED.to_owned(), Session::display_name);
    match window.and_then(|id| library.custom_window_name(id)) {
        Some(name) => format!("{session} — {name}"),
        None => session,
    }
}

/// The title of an open dashboard: "Dashboard", then the window's custom name when it has one (upstream's
/// `dashboardWindowTitle`).
#[must_use]
pub fn dashboard_title(library: &WindowLibrary, window: Uuid) -> String {
    match library.custom_window_name(window) {
        Some(name) => format!("Dashboard — {name}"),
        None => "Dashboard".to_owned(),
    }
}

/// Where the terminal area starts in window `window`'s content: past the sidebar and its divider while the sidebar
/// shows, else at the edge (upstream's `terminalAreaInset`; a zoom or the dashboard covers the sidebar).
#[must_use]
pub fn terminal_area_inset(core: &Core, window: Uuid) -> f64 {
    if core.zoom_target(window).is_some() || core.dashboard(window).is_some() {
        return 0.0;
    }
    core.library()
        .store(window)
        .map(AppStore::sidebar)
        .filter(|sidebar| sidebar.visible)
        .map_or(0.0, |sidebar| sidebar.width + 1.0)
}

/// A modal layer over the window's content, back to front (the GTK build's `modal_host` overlays).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Search,
    Ask,
    Picker,
    Palette,
    Switcher,
    Popover,
}

/// The window's GUI ask: its dialog and what it covers (`None`: nowhere on screen; the panel waits).
struct WindowAsk {
    dialog: Entity<AskDialog>,
    target: Option<AnchorTarget>,
}

/// Full screen and maximized as last painted: `window.list` reports a state once a frame in it is drawn, so a caller
/// that waits for the new state before toggling back is not dropped by a compositor that ignores a request arriving
/// before the client committed the state it configured (the GTK build's `Painted`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Painted {
    pub fullscreen: bool,
    pub maximized: bool,
}

impl Painted {
    fn of(window: &Window) -> Self {
        Self {
            fullscreen: window.is_fullscreen(),
            maximized: window.is_maximized(),
        }
    }

    /// The model's state for this, on a display of `display`.
    #[must_use]
    pub fn state(self, display: Option<WindowSize>) -> WindowState {
        WindowState {
            fullscreen: self.fullscreen,
            // maximized, not full screen (`WindowState::zoomed`)
            zoomed: self.maximized && !self.fullscreen,
            // neither Wayland nor GPUI reads a minimized state back
            minimized: None,
            display,
        }
    }
}

/// The size of the display `window` is on, in logical pixels.
fn display_size(window: &Window, cx: &App) -> Option<WindowSize> {
    let bounds = window.display(cx)?.bounds();
    let side = |value: gpui_kit::Pixels| {
        let value = f32::from(value).round();
        #[expect(clippy::cast_possible_truncation, reason = "a display side fits an i64")]
        let value = value as i64;
        value
    };
    Some(WindowSize {
        width: side(bounds.size.width),
        height: side(bounds.size.height),
    })
}

/// The window's root view.
pub struct WindowView {
    id: Uuid,
    deck: Entity<Deck>,
    titlebar: Entity<Titlebar>,
    sidebar: Entity<Sidebar>,
    popovers: Entity<Popovers>,
    ask: Option<WindowAsk>,
    layers: Vec<(Layer, AnyView)>,
    title: String,
    /// Set once the model closed the window, so its close request goes through.
    closing: Rc<Cell<bool>>,
    painted: Rc<Cell<Painted>>,
    /// What the window's first activation knows of its build, until that activation or [`FIRST_FOCUS_GRACE`] after
    /// its first frame.
    first_focus: Rc<Cell<Option<FirstFocus>>>,
    poster: Poster,
    _subscriptions: Vec<Subscription>,
}

impl WindowView {
    /// The root of model window `id`, over `deck`, built when the core's frontmost takes stood at `frontmost_takes`;
    /// its handlers post through `poster`.
    pub fn new(
        id: Uuid,
        deck: Entity<Deck>,
        poster: Poster,
        frontmost_takes: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let closing = Rc::new(Cell::new(false));
        let (posting, closed) = (poster.clone(), Rc::clone(&closing));
        window.on_window_should_close(cx, move |_, _| {
            if closed.get() || posting.upgrade().is_none() {
                return true;
            }
            post(&posting, move |state: &mut AppState| state.close_requested(id));
            false
        });
        crate::keymap_dispatch::register_window(window.window_handle(), KeyOrigin::Window(id));
        let posting = poster.clone();
        let first_focus = Rc::new(Cell::new(Some(FirstFocus::Built { frontmost_takes })));
        let (fresh, expiring) = (Rc::clone(&first_focus), Rc::clone(&first_focus));
        window.on_next_frame(move |_, cx| {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(FIRST_FOCUS_GRACE).await;
                expiring.set(None);
            })
            .detach();
        });
        let activation = cx.observe_window_activation(window, move |_, window, cx| {
            if !window.is_window_active() {
                check_keyboard_left(cx);
            }
            if window.is_window_active() {
                let first = fresh.take();
                post(&posting, move |state: &mut AppState| state.window_activated(id, first));
            } else {
                post(&posting, move |state: &mut AppState| state.key_window_lost(id));
            }
        });
        let bounds = cx.observe_window_bounds(window, |_, _, cx| cx.notify());
        let titlebar = cx.new(|_| Titlebar::new(id, poster.clone()));
        let sidebar = cx.new(|_| Sidebar::new(id, poster.clone()));
        let popovers = cx.new(|cx| Popovers::new(id, poster.clone(), cx));
        let opening = cx.subscribe_in(&titlebar, window, |view, _, event: &TitlebarEvent, window, cx| {
            let TitlebarEvent::Popover { kind, anchor } = *event;
            view.popovers
                .update(cx, |popovers, cx| popovers.toggle(kind, anchor, window, cx));
            view.sync_popover_layer(cx);
        });
        let popped = cx.observe(&popovers, |view, _, cx| view.sync_popover_layer(cx));
        // an alert opening or closing (anywhere: the registry is the app's) is the window's to re-sync, so the
        // keyboard goes back to the shown terminal once the last one is answered
        let posting = poster.clone();
        let alerts = cx.observe_global::<crate::modal::Alerts>(move |_, _| post(&posting, |_| {}));
        Self {
            id,
            deck,
            titlebar,
            sidebar,
            popovers,
            ask: None,
            layers: Vec::new(),
            title: UNTITLED.to_owned(),
            closing,
            painted: Rc::new(Cell::new(Painted::default())),
            first_focus,
            poster,
            _subscriptions: vec![activation, bounds, opening, popped, alerts],
        }
    }

    /// The window's popovers.
    #[must_use]
    pub fn popovers(&self) -> &Entity<Popovers> {
        &self.popovers
    }

    /// The window's GUI ask dialog while one is up.
    #[must_use]
    pub fn ask_dialog(&self) -> Option<&Entity<AskDialog>> {
        self.ask.as_ref().map(|ask| &ask.dialog)
    }

    /// Attaches the popovers while one is open; a popover that closed posts the sync that hands the keyboard back.
    fn sync_popover_layer(&mut self, cx: &mut Context<Self>) {
        let open = self.popovers.read(cx).is_open();
        if open == self.has_layer(Layer::Popover) {
            return;
        }
        let view = open.then(|| AnyView::from(self.popovers.clone()));
        self.set_layer(Layer::Popover, view, cx);
        if !open {
            post(&self.poster, |_| {});
        }
    }

    /// Brings the GUI ask in line with the model's modal slot: a dialog per ask (a new one takes the keyboard), what
    /// it covers, and none once it ends.
    fn sync_ask(&mut self, core: &Core, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id;
        let pending = core
            .modal_slot(id)
            .and_then(agterm_core::modal::PickController::pending_ask);
        let Some(pending) = pending else {
            if self.ask.take().is_some() {
                self.set_layer(Layer::Ask, None, cx);
            }
            return;
        };
        let target = crate::deck::anchor_target(core, id, pending);
        let look = crate::ask_dialog::Look::current(core, &self.deck.read(cx).env().config());
        if let Some(ask) = self.ask.as_mut().filter(|ask| ask.dialog.read(cx).id() == pending.id) {
            ask.target = target;
            ask.dialog.update(cx, |dialog, cx| dialog.set_look(look, cx));
            return;
        }
        let poster = self.poster.clone();
        let dialog = cx.new(|cx| AskDialog::new(pending, AskOwner::Window(id), look, poster, cx));
        let focus = dialog.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        self.set_layer(Layer::Ask, Some(dialog.clone().into()), cx);
        self.ask = Some(WindowAsk { dialog, target });
    }

    /// The GUI ask over its anchor, the rest of the window taking no clicks.
    fn render_ask(&self, view: &AnyView, cx: &App) -> AnyElement {
        let anchor = self
            .ask
            .as_ref()
            .and_then(|ask| ask.target)
            .and_then(|target| self.deck.read(cx).anchor_bounds(target));
        div()
            .id("gui-ask-layer")
            .absolute()
            .inset_0()
            .occlude()
            .children(anchor.map(|bounds| {
                div()
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .child(view.clone())
            }))
            .into_any_element()
    }

    /// The model window shown.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    #[must_use]
    pub fn deck(&self) -> &Entity<Deck> {
        &self.deck
    }

    /// The window's title bar.
    #[must_use]
    pub fn titlebar(&self) -> &Entity<Titlebar> {
        &self.titlebar
    }

    /// The window's sidebar.
    #[must_use]
    pub fn sidebar(&self) -> &Entity<Sidebar> {
        &self.sidebar
    }

    /// The title set last.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Attaches `view` to modal layer `layer` (`None` takes it down).
    pub fn set_layer(&mut self, layer: Layer, view: Option<AnyView>, cx: &mut Context<Self>) {
        self.layers.retain(|(known, _)| *known != layer);
        if let Some(view) = view {
            self.layers.push((layer, view));
            self.layers.sort_by_key(|(known, _)| *known);
        }
        cx.notify();
    }

    /// Whether a modal layer is attached.
    #[must_use]
    pub fn has_layer(&self, layer: Layer) -> bool {
        self.layers.iter().any(|(known, _)| *known == layer)
    }

    /// Closes the window for good: the model closed it, and its terminals with it.
    pub fn close(&self, window: &mut Window) {
        crate::keymap_dispatch::unregister_window(window.window_handle());
        self.closing.set(true);
        window.remove_window();
    }

    /// Raises the window and gives the keyboard to the shown terminal.
    pub fn present(&self, window: &mut Window, cx: &mut App) {
        window.activate_window();
        self.focus_terminal(window, cx);
    }

    /// Gives the keyboard to what should hold it while the window has it: the window's alert, GUI ask or popover while
    /// one is up, else the shown terminal (a new window's first focus, which the window manager gives the window itself:
    /// a new window maps without an activation request of its own).
    pub fn focus_terminal(&self, window: &mut Window, cx: &mut App) {
        if crate::modal::alert_open(window.window_handle(), cx) {
            crate::modal::refocus_alert(window, cx);
            return;
        }
        let held = self
            .ask
            .as_ref()
            .map(|ask| ask.dialog.read(cx).focus_handle().clone())
            .or_else(|| {
                let popovers = self.popovers.read(cx);
                popovers.is_open().then(|| popovers.focus_handle().clone())
            });
        if let Some(handle) = held.or_else(|| self.deck.read(cx).focus_target(cx)) {
            window.focus(&handle, cx);
        }
    }

    /// What `window.list` and the window commands read of this window: full screen and maximized as last painted,
    /// and the size of the display it is on.
    #[must_use]
    pub fn window_state(&self, window: &Window, cx: &App) -> WindowState {
        self.painted.get().state(display_size(window, cx))
    }

    /// Renders the model: the deck's pages, the sidebar, the title, the GUI ask and the popovers' rows. A modal holds
    /// the keyboard while the model's modal slot (the window's picker or GUI ask) or its palette is up, or one of the
    /// window's alerts or popovers; while the sidebar's row menu holds it the deck moves no focus. Whether the core
    /// needs a settle for the focus the deck gave.
    #[must_use = "a focus report the core took needs a settle"]
    pub fn sync(&mut self, core: &mut Core, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let id = self.id;
        let dialog = core.modal_pending(id) || core.palette_holds(id);
        let alert = crate::modal::alert_open(window.window_handle(), cx);
        self.sync_ask(core, window, cx);
        self.popovers.update(cx, |popovers, cx| popovers.sync(core, cx));
        self.sync_popover_layer(cx);
        let modal = dialog || alert || self.popovers.read(cx).is_open();
        let sidebar = self.sidebar.read(cx);
        let (held, owner) = (sidebar.menu_open(), sidebar.keyboard_owner(cx));
        let focused = self.deck.update(cx, |deck, cx| {
            deck.hold_keyboard(held, owner);
            deck.reconcile(core, modal, window, cx)
        });
        if alert {
            // a menu closing after its item opened the alert may have taken the keyboard back
            crate::modal::refocus_alert(window, cx);
        }
        self.sidebar.update(cx, |sidebar, cx| sidebar.sync(core, cx));
        let dashboard = core.dashboard(id).is_some();
        let title = if dashboard {
            dashboard_title(core.library(), id)
        } else {
            window_title(core.library(), Some(id))
        };
        let cover = if dashboard {
            Cover::Dashboard(title.clone())
        } else if core.zoom_target(id).is_some() {
            Cover::Zoom
        } else {
            Cover::None
        };
        let colors = crate::chrome::current(cx);
        self.titlebar
            .update(cx, |bar, cx| bar.sync(core, cover, dialog || alert, colors, cx));
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        cx.notify();
        focused
    }

    /// Applies a window command's request (upstream's `WindowRegistry` actions). GPUI has no client positioning on
    /// Linux, so a move (which this host never claims) is only logged.
    pub fn apply(&self, request: WindowRequest, window: &mut Window, cx: &mut App) {
        match request {
            WindowRequest::Raise(_) => self.present(window, cx),
            WindowRequest::Resize { size: requested, .. } => {
                // leaving the maximized state makes the window take the size now, as `setFrame` resizes a zoomed
                // `NSWindow` at once
                if window.is_maximized() && !window.is_fullscreen() {
                    window.zoom_window();
                }
                #[expect(clippy::cast_precision_loss, reason = "a window side fits an f32")]
                let (width, height) = (requested.width as f32, requested.height as f32);
                window.resize(size(px(width), px(height)));
            }
            WindowRequest::ToggleZoom(_) => window.zoom_window(),
            WindowRequest::ToggleFullscreen(_) => window.toggle_fullscreen(),
            WindowRequest::Minimize { minimized, .. } => {
                if minimized {
                    // a new window parked by its own command leaves frontmost where that command handed it
                    if self.first_focus.get().is_some() {
                        self.first_focus.set(Some(FirstFocus::Parked));
                    }
                    window.minimize_window();
                } else {
                    window.activate_window();
                }
            }
            WindowRequest::Move { .. } => tracing::warn!("window.move reached a host that cannot place windows"),
        }
    }

    /// Reports the painted state once a frame in a new one is drawn.
    fn track_painted(&self, window: &mut Window) {
        let now = Painted::of(window);
        if self.painted.get() == now {
            return;
        }
        let (painted, poster, id) = (Rc::clone(&self.painted), self.poster.clone(), self.id);
        window.on_next_frame(move |window, cx| {
            let now = Painted::of(window);
            if painted.replace(now) != now {
                let state = now.state(display_size(window, cx));
                post(&poster, move |app: &mut AppState| app.window_state_changed(id, state));
            }
        });
    }
}

impl Render for WindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.track_painted(window);
        let background = crate::chrome::color(crate::chrome::current(cx).background, 1.0);
        let layers: Vec<AnyElement> = self
            .layers
            .iter()
            .map(|(layer, view)| match layer {
                Layer::Ask => self.render_ask(view, cx),
                _ => div().absolute().inset_0().child(view.clone()).into_any_element(),
            })
            .collect();
        let alert = crate::modal::alert_layer(window.window_handle(), cx);
        div()
            .id("window")
            .capture_key_up(|event: &KeyUpEvent, window, cx| {
                // a release of a press the keymap took is the keymap's: no terminal reports it
                if crate::keymap_dispatch::key_up(event, window, cx) {
                    cx.stop_propagation();
                }
            })
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .child(self.titlebar.clone())
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_row()
                    .child(self.sidebar.clone())
                    .child(div().flex_1().min_w(px(0.)).h_full().child(self.deck.clone())),
            )
            .children(layers)
            .children(alert)
            .child(crate::chrome::resize_edges(window))
    }
}

/// How long after a window loses the keyboard the monitor checks whether another agterm window took it (the GTK
/// build's `LEAVE_CHECK_DELAY`): the next window's activation comes after this one's deactivation.
const LEAVE_CHECK_DELAY: Duration = Duration::from_millis(150);

/// How long after its first frame a new window's first activation still counts as the focus the window manager grants
/// on mapping it ([`AppState::window_activated`]): that one follows the map at once, while a window left unfocused then
/// is activated later only by the user, whose activation always counts, whatever commands took frontmost before.
const FIRST_FOCUS_GRACE: Duration = Duration::from_secs(1);

/// A window lost the keyboard: once the next activation had its chance, the key monitor forgets its consumed keys
/// when no agterm window holds the keyboard.
fn check_keyboard_left(cx: &mut Context<WindowView>) {
    cx.spawn(async move |_, cx| {
        cx.background_executor().timer(LEAVE_CHECK_DELAY).await;
        cx.update(|cx| {
            if !crate::keymap_dispatch::holds_keyboard(cx.active_window()) {
                crate::keymap_dispatch::keyboard_left();
            }
        });
    })
    .detach();
}

/// Opens the window of model window `id` with `app_id` as its Wayland app id / X11 class, when the core's frontmost
/// takes stand at `frontmost_takes`; its handle, `None` (logged) when the platform could not open one.
pub fn open(
    id: Uuid,
    frontmost_takes: u64,
    app_id: &str,
    env: &Rc<crate::deck::DeckEnv>,
    cx: &mut App,
) -> Option<gpui_kit::WindowHandle<WindowView>> {
    let options = gpui_kit::WindowOptions {
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some(UNTITLED.into()),
            ..gpui_kit::TitlebarOptions::default()
        }),
        window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds::centered(
            None,
            size(px(960.), px(640.)),
            cx,
        ))),
        app_id: Some(app_id.to_owned()),
        // the title bar is the app's own (`titlebar.rs`): the window manager's decorations only where the compositor
        // insists (GPUI falls back to them on X11 without a compositing manager, and a Wayland compositor may refuse)
        window_decorations: Some(gpui_kit::WindowDecorations::Client),
        ..gpui_kit::WindowOptions::default()
    };
    let env = Rc::clone(env);
    let poster = env.poster.clone();
    crate::status_glyph::watch_motion(cx);
    let opened = cx.open_window(options, move |window, cx| {
        let deck = cx.new(|_| Deck::new(id, env));
        cx.new(|cx| WindowView::new(id, deck, poster, frontmost_takes, window, cx))
    });
    match opened {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::error!(%error, window = %id, "the window did not open");
            None
        }
    }
}

#[cfg(test)]
mod tests;
