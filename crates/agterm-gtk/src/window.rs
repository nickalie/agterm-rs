//! One GTK window over one model window's store: the sidebar beside the deck under a header bar (upstream's
//! `WindowContentView.swift` `splitRoot`: sidebar, draggable divider, terminal column), titled after the active
//! session, reporting its on-screen state for `window.list`, its activation as frontmost (upstream's
//! `WindowAccessor.reportFrontmost`) and its close (upstream's window `willClose`), and applying the window commands'
//! requests. [`crate::windows`] owns the set of them.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adw::prelude::*;
use agterm_core::model::{AppStore, Session, WindowLibrary};
use agterm_core::{Core, WindowRequest, WindowSize, WindowState};
use gtk::gdk;
use uuid::Uuid;

use crate::deck::{Deck, PaneContext};
use crate::sidebar::Sidebar;
use crate::state::{self, AppState, Poster};

/// The title with no session selected (upstream's `WindowTitleSync` fallback).
const UNTITLED: &str = "Agterm";

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
    /// The header bar's exit-zoom button, shown while the window is zoomed to one terminal.
    exit_zoom: gtk::Button,
    deck: Deck,
    sidebar: Sidebar,
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
    pub fn build(app: &adw::Application, window_id: Uuid, context: PaneContext, poster: &Poster) -> Self {
        let deck = Deck::new();
        let sidebar = Sidebar::new(poster);
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
        let exit_zoom = gtk::Button::builder()
            .icon_name("view-restore-symbolic")
            .tooltip_text("Exit Terminal Zoom")
            .visible(false)
            .build();
        let header = adw::HeaderBar::new();
        header.pack_end(&exit_zoom);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&paned));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(UNTITLED)
            .default_width(960)
            .default_height(640)
            .content(&toolbar)
            .build();
        let ui = Self {
            window,
            window_id,
            paned,
            exit_zoom,
            deck,
            sidebar,
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
        let posting = poster.clone();
        self.exit_zoom.connect_clicked(move |_| {
            state::post(&posting, move |state: &mut AppState| state.exit_zoom(id));
        });
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

    /// Shows the window and focuses the shown terminal.
    pub fn present(&self) {
        self.window.present();
        if let Some(terminal) = self.deck.shown_terminal() {
            terminal.grab_focus();
        }
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

    /// Records a user's open or fold of a workspace row.
    pub fn set_workspace_expanded(&mut self, id: Uuid, expanded: bool) {
        self.sidebar.set_expanded(id, expanded);
    }

    /// Renders the model: the deck's pages, the sidebar's rows, visibility and width, and the title.
    pub fn sync(&mut self, core: &mut Core, poster: &Poster) {
        let id = self.window_id;
        let zoomed = core.zoom_target(id);
        self.deck.reconcile(core, Some(id), zoomed, &self.context, poster);
        let badges = core.settings().notification_badge_enabled.unwrap_or(true);
        if let Some(store) = core.library_mut().store_mut(id) {
            self.sidebar.refresh(store, badges);
            // a zoomed terminal fills the window: the sidebar steps aside without its visibility changing
            if zoomed.is_some() {
                self.sidebar.widget().set_visible(false);
            }
            let position = divider_position(store.sidebar().width);
            if self.paned.position() != position {
                self.paned.set_position(position);
            }
        }
        self.window.set_title(Some(&window_title(core.library(), Some(id))));
        self.exit_zoom.set_visible(zoomed.is_some());
    }
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
