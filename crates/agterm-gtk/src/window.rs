//! The GTK window of the frontmost open window: the sidebar beside the deck under a header bar (upstream's
//! `WindowContentView.swift` `splitRoot`: sidebar, draggable divider, terminal column), titled after the active
//! session, and reporting its full-screen and maximized state for `window.list`.
//!
//! One GTK window for now; more come with Task 14, and sessions of other windows get no terminal until then.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use agterm_core::model::{AppStore, Session, WindowLibrary};
use agterm_core::{Core, WindowState};
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

/// The window, its deck and its sidebar.
pub struct WindowUi {
    window: adw::ApplicationWindow,
    /// The model window shown; rebound to the frontmost open one once it closes.
    window_id: Option<Uuid>,
    paned: gtk::Paned,
    deck: Deck,
    sidebar: Sidebar,
    context: PaneContext,
    /// Set once the quit save ran, so the next close request goes through.
    closing: Rc<Cell<bool>>,
}

impl WindowUi {
    /// Builds the window for `window_id` (not presented yet); its handlers post through `poster`.
    #[must_use]
    pub fn build(app: &adw::Application, window_id: Option<Uuid>, context: PaneContext, poster: &Poster) -> Self {
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
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
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
            deck,
            sidebar,
            context,
            closing: Rc::new(Cell::new(false)),
        };
        ui.connect(poster);
        ui
    }

    fn connect(&self, poster: &Poster) {
        let posting = poster.clone();
        let closing = Rc::clone(&self.closing);
        self.window.connect_close_request(move |_| {
            if closing.get() || posting.upgrade().is_none() {
                return gtk::glib::Propagation::Proceed;
            }
            state::post(&posting, AppState::close_requested);
            gtk::glib::Propagation::Stop
        });
        for property in ["fullscreened", "maximized"] {
            let posting = poster.clone();
            self.window.connect_notify_local(Some(property), move |_, _| {
                state::post(&posting, AppState::window_state_changed);
            });
        }
        // a drag notifies per pixel: one task at a time reads the latest position
        let pending = Arc::new(AtomicBool::new(false));
        let posting = poster.clone();
        self.paned.connect_position_notify(move |_| {
            if pending.swap(true, Ordering::AcqRel) {
                return;
            }
            let pending = Arc::clone(&pending);
            state::post(&posting, move |state: &mut AppState| {
                pending.store(false, Ordering::Release);
                state.sidebar_resized();
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

    /// The model window shown.
    #[must_use]
    pub fn window_id(&self) -> Option<Uuid> {
        self.window_id
    }

    /// Closes the window once the quit save ran.
    pub fn close(&self) {
        self.closing.set(true);
        self.window.close();
    }

    /// What `window.list` reports for this window: its full-screen and maximized state. Wayland reads back no
    /// minimized state, so `minimized` stays false.
    #[must_use]
    pub fn window_state(&self) -> Option<(Uuid, WindowState)> {
        let fullscreen = self.window.is_fullscreen();
        let state = WindowState {
            fullscreen,
            // maximized, not full screen (`WindowState::zoomed`)
            zoomed: self.window.is_maximized() && !fullscreen,
            minimized: false,
        };
        self.window_id.map(|id| (id, state))
    }

    /// The window and the divider's position, read after the divider moved; `None` while the sidebar is hidden or
    /// the position was never set. A move the model made itself reads back its own width, which is a no-op.
    #[must_use]
    pub fn dragged_sidebar_width(&self) -> Option<(Uuid, f64)> {
        if !self.sidebar.widget().is_visible() || !self.paned.is_position_set() {
            return None;
        }
        self.window_id.map(|id| (id, f64::from(self.paned.position())))
    }

    /// Records a user's open or fold of a workspace row.
    pub fn set_workspace_expanded(&mut self, id: Uuid, expanded: bool) {
        self.sidebar.set_expanded(id, expanded);
    }

    /// Renders the model: the deck's pages, the sidebar's rows, visibility and width, and the title.
    pub fn sync(&mut self, core: &mut Core, poster: &Poster) {
        let library = core.library();
        if !self.window_id.is_some_and(|id| library.is_open(id)) {
            self.window_id = library.active_window_id();
        }
        self.deck.reconcile(core, self.window_id, &self.context, poster);
        let badges = core.settings().notification_badge_enabled.unwrap_or(true);
        if let Some(store) = self.window_id.and_then(|id| core.library_mut().store_mut(id)) {
            self.sidebar.refresh(store, badges);
            let position = divider_position(store.sidebar().width);
            if self.paned.position() != position {
                self.paned.set_position(position);
            }
        }
        self.window
            .set_title(Some(&window_title(core.library(), self.window_id)));
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
}
