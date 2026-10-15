//! One window over one model window's store (upstream's `WindowContentView.swift`; ported from the GTK build's
//! `window.rs`): the deck under the window's chrome, titled after the active session, reporting its on-screen state for
//! `window.list`, its activation as frontmost (upstream's `WindowAccessor.reportFrontmost`) and its close (upstream's
//! window `willClose`), and applying the window commands' requests. [`crate::windows`] owns the set of them.
//!
//! The window's content is a column: the title bar (`titlebar.rs`), then the sidebar's place (Task 19) beside the
//! deck, all on the chrome's background (`chrome.rs`); a window the client decorates gets resize handles along its
//! free edges. Over it sit the window's modal layers ([`Layer`]), in the GTK build's order: the scrollback search bar, the
//! GUI ask, the native picker, the command palette and the Ctrl-Tab switcher; each later task attaches its view to its
//! layer ([`WindowView::set_layer`]), drawn over the whole window while attached.

use std::cell::Cell;
use std::rc::Rc;

use agterm_core::model::{AppStore, Session, WindowLibrary};
use agterm_core::titlebar::UNTITLED;
use agterm_core::{Core, WindowRequest, WindowSize, WindowState};
use gpui_kit::{
    AnyView, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Window, div, px, size,
};
use uuid::Uuid;

use crate::deck::Deck;
use crate::state::{AppState, Poster, post};
use crate::titlebar::{Cover, Titlebar};

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
    layers: Vec<(Layer, AnyView)>,
    title: String,
    /// Set once the model closed the window, so its close request goes through.
    closing: Rc<Cell<bool>>,
    painted: Rc<Cell<Painted>>,
    poster: Poster,
    _subscriptions: Vec<Subscription>,
}

impl WindowView {
    /// The root of model window `id`, over `deck`; its handlers post through `poster`.
    pub fn new(id: Uuid, deck: Entity<Deck>, poster: Poster, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let closing = Rc::new(Cell::new(false));
        let (posting, closed) = (poster.clone(), Rc::clone(&closing));
        window.on_window_should_close(cx, move |_, _| {
            if closed.get() || posting.upgrade().is_none() {
                return true;
            }
            post(&posting, move |state: &mut AppState| state.close_requested(id));
            false
        });
        let posting = poster.clone();
        let activation = cx.observe_window_activation(window, move |_, window, _| {
            if window.is_window_active() {
                post(&posting, move |state: &mut AppState| {
                    state.set_key_window(Some(id));
                    state.report_frontmost(id);
                });
            } else {
                post(&posting, move |state: &mut AppState| state.key_window_lost(id));
            }
        });
        let bounds = cx.observe_window_bounds(window, |_, _, cx| cx.notify());
        let titlebar = cx.new(|_| Titlebar::new(id, poster.clone()));
        Self {
            id,
            deck,
            titlebar,
            layers: Vec::new(),
            title: UNTITLED.to_owned(),
            closing,
            painted: Rc::new(Cell::new(Painted::default())),
            poster,
            _subscriptions: vec![activation, bounds],
        }
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

    #[must_use]
    pub fn titlebar(&self) -> &Entity<Titlebar> {
        &self.titlebar
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
        self.closing.set(true);
        window.remove_window();
    }

    /// Raises the window and gives the keyboard to the shown terminal.
    pub fn present(&self, window: &mut Window, cx: &mut App) {
        window.activate_window();
        self.focus_terminal(window, cx);
    }

    /// Gives the keyboard to the shown terminal while the window has it: a new window's first focus, which the window
    /// manager gives the window itself (a new window maps without an activation request of its own).
    pub fn focus_terminal(&self, window: &mut Window, cx: &mut App) {
        if let Some(handle) = self.deck.read(cx).focus_target(cx) {
            window.focus(&handle, cx);
        }
    }

    /// What `window.list` and the window commands read of this window: full screen and maximized as last painted,
    /// and the size of the display it is on.
    #[must_use]
    pub fn window_state(&self, window: &Window, cx: &App) -> WindowState {
        self.painted.get().state(display_size(window, cx))
    }

    /// Renders the model: the deck's pages and the title. Whether a modal holds the keyboard is the model's: the
    /// window's picker or GUI ask, or its palette.
    pub fn sync(&mut self, core: &mut Core, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id;
        let modal = core.modal_pending(id) || core.palette_holds(id);
        let given = self.deck.update(cx, |deck, cx| {
            deck.reconcile(core, modal, window, cx);
            deck.take_given_focus()
        });
        for (session, token) in given {
            post(&self.poster, move |state: &mut AppState| {
                state.pane_focused(session, &token);
            });
        }
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
            .update(cx, |bar, cx| bar.sync(core, cover, modal, colors, cx));
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        cx.notify();
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
        let layers = self
            .layers
            .iter()
            .map(|(_, view)| div().absolute().inset_0().child(view.clone()));
        div()
            .id("window")
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
                    .child(div().flex_1().min_w(px(0.)).h_full().child(self.deck.clone())),
            )
            .children(layers)
            .child(crate::chrome::resize_edges(window))
    }
}

/// Opens the window of model window `id` with `app_id` as its Wayland app id / X11 class; its handle, `None` (logged)
/// when the platform could not open one.
pub fn open(
    id: Uuid,
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
    let opened = cx.open_window(options, move |window, cx| {
        let deck = cx.new(|_| Deck::new(id, env));
        cx.new(|cx| WindowView::new(id, deck, poster, window, cx))
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
