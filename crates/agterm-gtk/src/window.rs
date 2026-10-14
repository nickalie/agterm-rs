//! One GTK window over one model window's store: the sidebar beside the deck under a header bar (upstream's
//! `WindowContentView.swift` `splitRoot`: sidebar, draggable divider, terminal column), titled after the active
//! session, reporting its on-screen state for `window.list`, its activation as frontmost (upstream's
//! `WindowAccessor.reportFrontmost`) and its close (upstream's window `willClose`), and applying the window commands'
//! requests. [`crate::windows`] owns the set of them.
//!
//! The header bar is the window's [`Titlebar`]: its title, its buttons and popovers, stripped to an exit under a
//! terminal zoom or the open dashboard (upstream's `dashboardTitlebar`), which is titled "Dashboard" and, like a
//! terminal zoom, fills the window with the sidebar stepped aside.
//!
//! Over the window's content sit its two modal layers, added once: the GUI ask ([`crate::ask_dialog::WindowAsk`]) and
//! the native picker ([`crate::picker::WindowPicker`]), drawn from the window's modal slot on every sync. While either
//! is up the title bar's controls stand down, no terminal is given the keyboard, and the layer that
//! came up takes it. Beside them: the scrollback search bar ([`crate::search::SearchBar`]), the command palette while
//! the window draws it ([`crate::palette::WindowPalette`]), and the Ctrl-Tab switcher's overlay
//! ([`crate::session_switcher::WindowSwitcher`]).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adw::prelude::*;
use agterm_core::model::{AppStore, PendingAsk, Session, WindowLibrary};
// the title with no session selected (upstream's `WindowTitleSync` fallback), the header bar's too
use agterm_core::titlebar::UNTITLED;
use agterm_core::{Core, WindowRequest, WindowSize, WindowState};
use gtk::gdk;
use uuid::Uuid;

use crate::ask_dialog::{AnchorTarget, Look, WindowAsk};
use crate::deck::{Deck, PaneContext};
use crate::palette::{PaletteView, WindowPalette};
use crate::picker::WindowPicker;
use crate::search::SearchBar;
use crate::session_switcher::WindowSwitcher;
use crate::sidebar::Sidebar;
use crate::state::{self, AppState, Poster};
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

/// A sidebar width in the model as the divider's position.
#[expect(
    clippy::cast_possible_truncation,
    reason = "sidebar widths are clamped to a few hundred points"
)]
fn divider_position(width: f64) -> i32 {
    width.round() as i32
}

/// A size in points as GTK takes it, saturating.
fn gtk_size(points: i64) -> i32 {
    i32::try_from(points).unwrap_or(i32::MAX)
}

/// Whether the display reads a window's minimized state back: X11 does; Wayland's xdg-shell has no such state for a
/// client.
fn reads_minimized(display: &gdk::Display) -> bool {
    display.type_().name().starts_with("GdkX11")
}

/// Full screen and maximized as last painted.
///
/// GTK flips its flags when the compositor's configure arrives, a frame before the window commits that state, and
/// weston drops an unmaximize or unfullscreen that arrives before the client committed the state it just configured.
/// So `window.list` reports these only once a frame in that state is painted (and committed): a caller that waits for
/// the new state before toggling back is not dropped (upstream likewise reports once the transition settled).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Painted {
    fullscreen: bool,
    maximized: bool,
}

impl Painted {
    fn of(window: &adw::ApplicationWindow) -> Self {
        Self {
            fullscreen: window.is_fullscreen(),
            maximized: window.is_maximized(),
        }
    }
}

/// How long a minimize or restore asked of an X11 window is reported as done before the window manager confirms it
/// (upstream's settle poll budget, 30 × 0.05 s); after it the state read back is reported again.
const MINIMIZE_SETTLE: Duration = Duration::from_millis(1500);

/// The minimized state to report, given the one `read` back and the one a request is `pending` on, and whether the
/// request is still pending. The window manager confirms an iconify a round trip after it is asked, so until the read
/// back catches up the requested state is reported: a quick `off` or `toggle` then acts on the window's new state.
fn reported_minimized(read: Option<bool>, pending: Option<bool>) -> (Option<bool>, bool) {
    match pending {
        Some(desired) if read.is_some() && read != Some(desired) => (Some(desired), true),
        _ => (read, false),
    }
}

/// The window, its deck and its sidebar.
pub struct WindowUi {
    window: adw::ApplicationWindow,
    /// The model window shown.
    window_id: Uuid,
    paned: gtk::Paned,
    titlebar: Titlebar,
    deck: Deck,
    sidebar: Sidebar,
    /// The GUI ask over the window's content.
    ask: WindowAsk,
    /// The native picker over the window's content.
    picker: WindowPicker,
    /// The command palette, while this window draws it.
    palette: WindowPalette,
    /// The scrollback search bar.
    search: SearchBar,
    /// The Ctrl-Tab switcher.
    switcher: WindowSwitcher,
    context: PaneContext,
    /// Set once the model closed the window, so its close request goes through.
    closing: Rc<Cell<bool>>,
    /// The minimized state last asked of an X11 window and the request's number, until the window manager confirms it
    /// or [`MINIMIZE_SETTLE`] passes ([`reported_minimized`]).
    pending_minimized: Rc<Cell<Option<(bool, u64)>>>,
    /// Numbers the minimize requests, so only the latest one's settle timer clears the pending state.
    minimize_requests: Cell<u64>,
    /// Full screen and maximized as last painted ([`Painted`]).
    painted: Rc<Cell<Painted>>,
    poster: Poster,
}

impl WindowUi {
    /// Builds the window for `window_id` (not presented yet); its handlers post through `poster`.
    #[must_use]
    pub fn build(
        app: &adw::Application,
        window_id: Uuid,
        context: PaneContext,
        menu: &crate::app_menu::AppMenu,
        poster: &Poster,
    ) -> Self {
        let deck = Deck::new();
        let sidebar = Sidebar::new(window_id, poster);
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(sidebar.widget())
            .end_child(deck.widget())
            .resize_start_child(false)
            .shrink_start_child(false)
            .resize_end_child(true)
            .shrink_end_child(false)
            .wide_handle(false)
            .build();
        sidebar
            .widget()
            .set_size_request(divider_position(agterm_core::model::SIDEBAR_WIDTH_MIN), -1);
        let ask_layer = WindowAsk::new();
        let pick_layer = WindowPicker::new();
        let palette = WindowPalette::new();
        let search = SearchBar::new(window_id, poster);
        let switcher = WindowSwitcher::new(window_id, poster);
        let modal_host = gtk::Overlay::new();
        modal_host.set_child(Some(&paned));
        modal_host.add_overlay(search.widget());
        modal_host.add_overlay(ask_layer.widget());
        modal_host.add_overlay(pick_layer.widget());
        modal_host.add_overlay(palette.widget());
        modal_host.add_overlay(&switcher.widget());
        let toolbar = adw::ToolbarView::new();
        let titlebar = Titlebar::new(window_id, &toolbar, menu.model().upcast_ref(), poster);
        toolbar.set_content(Some(&modal_host));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(UNTITLED)
            .default_width(960)
            .default_height(640)
            .content(&toolbar)
            .build();
        window.insert_action_group("menu", Some(menu.group()));
        // ahead of every key monitor: a HUD link hears each modifier change, and consumes none
        crate::hud::connect_link_modifiers(&window);
        // ahead of the keymap's monitor: Ctrl+Tab is the switcher's
        switcher.install(&window);
        crate::keymap_dispatch::install(&window, crate::keymap_dispatch::KeyOrigin::Window(window_id));
        let ui = Self {
            window,
            window_id,
            paned,
            titlebar,
            deck,
            sidebar,
            ask: ask_layer,
            picker: pick_layer,
            palette,
            search,
            switcher,
            context,
            closing: Rc::new(Cell::new(false)),
            pending_minimized: Rc::new(Cell::new(None)),
            minimize_requests: Cell::new(0),
            painted: Rc::new(Cell::new(Painted::default())),
            poster: poster.clone(),
        };
        ui.connect(poster);
        ui
    }

    fn connect(&self, poster: &Poster) {
        let id = self.window_id;
        let posting = poster.clone();
        let closing = Rc::clone(&self.closing);
        self.window.connect_close_request(move |_| {
            if closing.get() || posting.upgrade().is_none() {
                return gtk::glib::Propagation::Proceed;
            }
            state::post(&posting, move |state: &mut AppState| state.close_requested(id));
            gtk::glib::Propagation::Stop
        });
        let posting = poster.clone();
        self.window.connect_is_active_notify(move |window| {
            if window.is_active() {
                state::post(&posting, move |state: &mut AppState| state.report_frontmost(id));
            }
        });
        // full screen and maximized are reported once painted (`Painted`): a change draws a frame
        for property in ["fullscreened", "maximized"] {
            self.window
                .connect_notify_local(Some(property), |window, _| window.queue_draw());
        }
        // the toplevel's state (minimized on X11), its monitor and its frame clock are known once it is realized
        let posting = poster.clone();
        let painted = Rc::clone(&self.painted);
        self.window.connect_realize(move |window| {
            let Some(surface) = window.surface() else { return };
            let changed = posting.clone();
            surface.connect_notify_local(Some("state"), move |_, _| {
                state::post(&changed, move |state: &mut AppState| state.window_state_changed(id));
            });
            let clock = surface.frame_clock();
            let weak = window.downgrade();
            let painted = Rc::clone(&painted);
            let posting = posting.clone();
            clock.connect_after_paint(move |_| {
                let Some(window) = weak.upgrade() else { return };
                let now = Painted::of(&window);
                if painted.replace(now) != now {
                    state::post(&posting, move |state: &mut AppState| state.window_state_changed(id));
                }
            });
        });
        self.connect_divider(poster);
        self.deck.chrome().connect(id, poster);
    }

    /// A drag notifies per pixel: one task at a time reads the latest position.
    fn connect_divider(&self, poster: &Poster) {
        let id = self.window_id;
        let pending = Arc::new(AtomicBool::new(false));
        let posting = poster.clone();
        self.paned.connect_position_notify(move |_| {
            if pending.swap(true, Ordering::AcqRel) {
                return;
            }
            let pending = Arc::clone(&pending);
            state::post(&posting, move |state: &mut AppState| {
                pending.store(false, Ordering::Release);
                state.sidebar_resized(id);
            });
        });
    }

    /// Shows the window and focuses its picker, GUI ask or palette while one is up, else the shown terminal (the
    /// dashboard's grid while it is open).
    pub fn present(&self) {
        self.window.present();
        if !self.picker.grab_focus() && !self.ask.grab_focus() && !self.palette.grab_focus() {
            self.deck.grab_focus();
        }
    }

    /// The window's sidebar.
    #[must_use]
    pub fn sidebar(&self) -> &Sidebar {
        &self.sidebar
    }

    /// The GTK window.
    #[must_use]
    pub fn gtk_window(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    /// Closes the window for good: the model closed it, and its terminals with it.
    pub fn close(&self) {
        self.closing.set(true);
        self.window.close();
    }

    /// Applies a window command's request (upstream's `WindowRegistry` actions). GTK has no client positioning, so a
    /// move (which this host never claims) is only logged.
    pub fn apply(&self, request: WindowRequest) {
        let window = &self.window;
        match request {
            WindowRequest::Raise(_) => self.present(),
            WindowRequest::Resize { size, .. } => {
                // GTK keeps a default size aside while the window is maximized; leaving that state makes the window
                // take the size now, as `setFrame` resizes a zoomed `NSWindow` at once. Full screen is never left here:
                // the core sends no resize for a window it reads in full screen, and one racing that read-back is only
                // kept aside for when the window leaves full screen
                if window.is_maximized() {
                    window.unmaximize();
                }
                window.set_default_size(gtk_size(size.width), gtk_size(size.height));
            }
            WindowRequest::ToggleZoom(_) => {
                if window.is_maximized() {
                    window.unmaximize();
                } else {
                    window.maximize();
                }
            }
            WindowRequest::ToggleFullscreen(_) => {
                if window.is_fullscreen() {
                    window.unfullscreen();
                } else {
                    window.fullscreen();
                }
            }
            WindowRequest::Minimize { minimized, .. } => {
                if minimized {
                    window.minimize();
                } else {
                    window.unminimize();
                }
                self.expect_minimized(minimized);
            }
            WindowRequest::Move { .. } => tracing::warn!("window.move reached a host that cannot place windows"),
        }
    }

    /// On a display that reads the minimized state back, reports `minimized` until the window manager confirms it or
    /// [`MINIMIZE_SETTLE`] passes, then reports the state read back again.
    fn expect_minimized(&self, minimized: bool) {
        let display = gtk::prelude::WidgetExt::display(&self.window);
        if !reads_minimized(&display) {
            return;
        }
        let request = self.minimize_requests.get().wrapping_add(1);
        self.minimize_requests.set(request);
        self.pending_minimized.set(Some((minimized, request)));
        let pending = Rc::clone(&self.pending_minimized);
        let posting = self.poster.clone();
        let id = self.window_id;
        gtk::glib::timeout_add_local_once(MINIMIZE_SETTLE, move || {
            if pending.get().is_some_and(|(_, number)| number == request) {
                pending.set(None);
                state::post(&posting, move |state: &mut AppState| state.window_state_changed(id));
            }
        });
    }

    /// What `window.list` and the window commands read of this window: full screen and maximized as last painted
    /// ([`Painted`]), minimized where the display reads it back, and the size of the monitor it is on.
    #[must_use]
    pub fn window_state(&self) -> WindowState {
        let Painted { fullscreen, maximized } = self.painted.get();
        let surface = self.window.surface();
        let display = gtk::prelude::WidgetExt::display(&self.window);
        let read = surface
            .as_ref()
            .filter(|_| reads_minimized(&display))
            .and_then(|surface| surface.downcast_ref::<gdk::Toplevel>().map(gdk::Toplevel::state))
            .map(|state| state.contains(gdk::ToplevelState::MINIMIZED));
        let pending = self.pending_minimized.get().map(|(desired, _)| desired);
        let (minimized, still_pending) = reported_minimized(read, pending);
        if !still_pending {
            self.pending_minimized.set(None);
        }
        let monitor = surface
            .as_ref()
            .and_then(|surface| display.monitor_at_surface(surface))
            .map(|monitor| monitor.geometry())
            .map(|geometry| WindowSize {
                width: i64::from(geometry.width()),
                height: i64::from(geometry.height()),
            });
        WindowState {
            fullscreen,
            // maximized, not full screen (`WindowState::zoomed`)
            zoomed: maximized && !fullscreen,
            minimized,
            display: monitor,
        }
    }

    /// The divider's position, read after the divider moved; `None` while the sidebar is hidden or the position was
    /// never set. A move the model made itself reads back its own width, which is a no-op.
    #[must_use]
    pub fn dragged_sidebar_width(&self) -> Option<f64> {
        if !self.sidebar.widget().is_visible() || !self.paned.is_position_set() {
            return None;
        }
        Some(f64::from(self.paned.position()))
    }

    /// Opens or folds a workspace row in the sidebar's outline (view state; the caller persists a user toggle).
    pub fn set_workspace_expanded(&mut self, id: Uuid, expanded: bool) {
        self.sidebar.set_expanded(id, expanded);
    }

    /// Puts a fresh terminal in place of a pane of this window's deck (see [`crate::deck::Deck::replace_pane`]).
    pub fn replace_pane(
        &mut self,
        core: &mut Core,
        replacement: &agterm_core::PaneReplacement,
        poster: &Poster,
    ) -> bool {
        self.deck
            .replace_pane(core, self.window_id, replacement, &self.context, poster)
    }

    /// Renders the model: the deck's pages, the sidebar's rows, visibility and width, and the title.
    pub fn sync(&mut self, core: &mut Core, poster: &Poster) {
        let id = self.window_id;
        let zoomed = core.zoom_target(id);
        let dashboard = core.dashboard(id).is_some();
        self.deck.reconcile(core, Some(id), zoomed, &self.context, poster);
        for (session, token) in self.deck.take_given_focus() {
            state::post(poster, move |state: &mut AppState| state.pane_focused(session, &token));
        }
        let settings = core.settings().clone();
        if let Some(store) = core.library_mut().store_mut(id) {
            self.sidebar.refresh(store, &settings);
            // a zoomed terminal or the dashboard fills the window: the sidebar steps aside without its visibility
            // changing
            if zoomed.is_some() || dashboard {
                self.sidebar.widget().set_visible(false);
            }
            let position = divider_position(store.sidebar().width);
            if self.paned.position() != position {
                self.paned.set_position(position);
            }
        }
        let title = if dashboard {
            dashboard_title(core.library(), id)
        } else {
            window_title(core.library(), Some(id))
        };
        let cover = if dashboard {
            Cover::Dashboard(title.clone())
        } else if zoomed.is_some() {
            Cover::Zoom
        } else {
            Cover::None
        };
        self.window.set_title(Some(&title));
        let inset = terminal_area_inset(core, id);
        let modal = self.sync_modals(core, poster, inset);
        self.sync_transients(core, poster, inset, modal);
        self.titlebar.sync(core, &cover, modal);
    }

    /// Draws the window's modal slot: its picker or GUI ask, a new one taking the keyboard. Whether one is up.
    fn sync_modals(&mut self, core: &Core, poster: &Poster, inset: f64) -> bool {
        let id = self.window_id;
        let slot = core.modal_slot(id);
        let pick = slot.and_then(agterm_core::modal::PickController::pending);
        let ask = slot.and_then(agterm_core::modal::PickController::pending_ask);
        let target = ask.map(|ask| self.anchor_target(core, ask));
        let look = Look::current(core);
        let new_ask = self.ask.sync(ask, target.flatten().as_ref(), &look, id, poster);
        let new_pick = self.picker.sync(pick, inset, id, poster);
        if new_pick {
            self.picker.grab_focus();
        } else if new_ask {
            self.ask.grab_focus();
        }
        self.picker.is_open() || self.ask.is_open()
    }

    /// Draws what is not the modal slot: the palette (taking the keyboard when it comes up), the search bar (likewise,
    /// unless a modal holds it) and the switcher.
    fn sync_transients(&mut self, core: &Core, poster: &Poster, inset: f64, modal: bool) {
        let id = self.window_id;
        if self.palette.sync(PaletteView::of(core, id), inset, id, poster) {
            self.palette.grab_focus();
        }
        if self.search.sync(core) && !modal && !core.palette_holds(id) {
            self.search.grab_focus();
        }
        self.switcher.sync(core, inset);
    }

    /// What GUI ask `ask` covers (upstream's `askAnchorFrame`): the terminal area without an anchor, else its
    /// session's page or that page's pane; `None` while the page or pane is gone (the core cancels the ask then).
    fn anchor_target(&self, core: &Core, ask: &PendingAsk) -> Option<AnchorTarget> {
        let Some(anchor) = &ask.anchor else {
            return Some(AnchorTarget::Area(self.deck.widget().clone().upcast()));
        };
        let stack = self.deck.page_stack(anchor.session)?;
        let pane = match anchor.pane_identity {
            None => None,
            Some(identity) => {
                let session = core.library().store(self.window_id)?.session(anchor.session)?;
                Some(crate::split::Slot::of_overlay(
                    session.pane_role_for_identity(identity)?,
                ))
            }
        };
        Some(AnchorTarget::Page { stack, pane })
    }
}

/// Where the terminal area starts in window `window`'s content: past the sidebar and its divider while the sidebar
/// shows, else at the edge (upstream's `terminalAreaInset`; a zoom or the dashboard covers the sidebar).
fn terminal_area_inset(core: &Core, window: Uuid) -> f64 {
    if core.zoom_target(window).is_some() || core.dashboard(window).is_some() {
        return 0.0;
    }
    core.library()
        .store(window)
        .map(AppStore::sidebar)
        .filter(|sidebar| sidebar.visible)
        .map_or(0.0, |sidebar| sidebar.width + 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_is_the_active_session_then_the_custom_window_name() {
        let mut library = WindowLibrary::seeded("/home/u");
        let window = library.active_window_id().unwrap();
        assert_eq!(window_title(&library, Some(window)), "u");
        library.rename_window(window, "work");
        assert_eq!(window_title(&library, Some(window)), "u — work");
        library.store_mut(window).unwrap().select_session(None);
        assert_eq!(window_title(&library, Some(window)), "Agterm — work");
        assert_eq!(window_title(&library, None), "Agterm");
    }

    #[test]
    fn the_dashboard_title_names_a_custom_window() {
        let mut library = WindowLibrary::seeded("/home/u");
        let window = library.active_window_id().unwrap();
        assert_eq!(dashboard_title(&library, window), "Dashboard");
        library.rename_window(window, "work");
        assert_eq!(dashboard_title(&library, window), "Dashboard — work");
    }

    #[test]
    fn widths_round_to_whole_points() {
        assert_eq!(divider_position(220.0), 220);
        assert_eq!(divider_position(180.6), 181);
    }

    #[test]
    fn a_pending_minimize_is_reported_until_the_read_back_confirms_it() {
        assert_eq!(
            reported_minimized(Some(false), Some(true)),
            (Some(true), true),
            "iconify not confirmed yet"
        );
        assert_eq!(
            reported_minimized(Some(true), Some(false)),
            (Some(false), true),
            "restore not confirmed yet"
        );
        assert_eq!(
            reported_minimized(Some(true), Some(true)),
            (Some(true), false),
            "confirmed"
        );
        assert_eq!(reported_minimized(Some(false), None), (Some(false), false));
        assert_eq!(
            reported_minimized(None, Some(true)),
            (None, false),
            "no read-back (Wayland)"
        );
    }

    #[test]
    fn sizes_saturate_into_gtks_range() {
        assert_eq!(gtk_size(900), 900);
        assert_eq!(gtk_size(i64::MAX), i32::MAX);
    }
}
