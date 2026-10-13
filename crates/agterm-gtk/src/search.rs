//! The scrollback search bar (upstream's `Views/TerminalSearchBar.swift`, as `WindowContentView.searchBarLayer` hosts
//! it): a compact panel at the top right of the window's content, shown while the active session's search is open and
//! no zoom fills the window.
//!
//! A search field, the match counter (`2 of 7`, `no matches`), and previous, next and close buttons. Typing sends the
//! needle to the pane the search is pinned to; Return steps to the next match, Shift+Return to the previous one, Escape,
//! Find's chord or the close button ends the search, whose end report gives the terminal the keyboard back
//! ([`agterm_core::Core::search_bar_close`]). The bar takes the keyboard when it comes up.
//!
//! The field is the needle's writer while the user types: the model's needle is pushed into it only once every needle
//! the field sent has landed, so a settle in between never rewinds what is being typed; a needle set elsewhere (a
//! `session.search --text`, the seed of a new search) shows once the field is idle.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agterm_core::Core;
use agterm_core::model::{AppStore, SearchState};
use gtk::prelude::*;
use gtk::{gdk, glib};
use uuid::Uuid;

use crate::modal;
use crate::state::{self, AppState, Poster};

/// The field's width (upstream's 150-point field).
const FIELD_WIDTH: i32 = 150;

/// What the bar asks of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchInput {
    /// The field changed to this needle.
    Needle(String),
    /// Step to the next match (`true`) or the previous one.
    Step(bool),
    /// End the search.
    Close,
}

/// The session whose search the bar of window `window` shows, and that search: the active session's open search,
/// while no zoom fills the window.
#[must_use]
pub fn shown_search(core: &Core, window: Uuid) -> Option<(Uuid, &SearchState)> {
    if core.zoom_target(window).is_some() {
        return None;
    }
    let session = core.library().store(window).and_then(AppStore::active_session)?;
    session.search.active.then(|| (session.id(), &session.search))
}

/// What a key in the field does beyond editing it: Shift+Return steps back, Escape closes.
#[must_use]
pub fn field_key(keyval: gdk::Key, state: gdk::ModifierType) -> Option<SearchInput> {
    match keyval {
        gdk::Key::Return | gdk::Key::KP_Enter if state.contains(gdk::ModifierType::SHIFT_MASK) => {
            Some(SearchInput::Step(false))
        }
        gdk::Key::Escape => Some(SearchInput::Close),
        _ => None,
    }
}

/// What the bar's handlers share.
struct Shared {
    window: Uuid,
    poster: Poster,
    /// Set while the bar writes the field itself, so the change is not sent back.
    syncing: Cell<bool>,
    /// The needles the field sent, and how many of them landed.
    sent: Cell<u64>,
    landed: Arc<AtomicU64>,
}

impl Shared {
    fn send(&self, input: SearchInput) {
        let window = self.window;
        let landed = matches!(input, SearchInput::Needle(_)).then(|| {
            let number = self.sent.get() + 1;
            self.sent.set(number);
            (Arc::clone(&self.landed), number)
        });
        state::post(&self.poster, move |state: &mut AppState| {
            // landed before the settle renders it, so that render already counts it
            if let Some((landed, number)) = landed {
                landed.store(number, Ordering::Release);
            }
            state.search_bar(window, input);
        });
    }

    /// Whether every needle the field sent has landed in the model.
    fn idle(&self) -> bool {
        self.landed.load(Ordering::Acquire) == self.sent.get()
    }
}

/// One window's search bar.
pub struct SearchBar {
    root: gtk::Box,
    entry: gtk::Entry,
    counter: gtk::Label,
    shared: Rc<Shared>,
    /// The session the bar shows the search of.
    shown: Cell<Option<Uuid>>,
}

impl SearchBar {
    /// A hidden bar for window `window`, posting through `poster`.
    #[must_use]
    pub fn new(window: Uuid, poster: &Poster) -> Self {
        modal::install_css();
        let entry = gtk::Entry::builder()
            .placeholder_text("Find")
            .primary_icon_name("system-search-symbolic")
            .width_request(FIELD_WIDTH)
            .build();
        let counter = gtk::Label::new(None);
        counter.add_css_class("dim-label");
        counter.add_css_class("caption");
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .margin_top(8)
            .margin_end(8)
            .visible(false)
            .build();
        root.add_css_class(modal::PANEL_CLASS);
        root.add_css_class("agterm-search-bar");
        root.append(&entry);
        root.append(&counter);
        let shared = Rc::new(Shared {
            window,
            poster: poster.clone(),
            syncing: Cell::new(false),
            sent: Cell::new(0),
            landed: Arc::new(AtomicU64::new(0)),
        });
        for (icon, tip, input) in [
            ("go-up-symbolic", "Previous match", SearchInput::Step(false)),
            ("go-down-symbolic", "Next match", SearchInput::Step(true)),
            ("window-close-symbolic", "Close search", SearchInput::Close),
        ] {
            root.append(&button(icon, tip, input, &shared));
        }
        connect_field(&entry, &shared);
        Self {
            root,
            entry,
            counter,
            shared,
            shown: Cell::new(None),
        }
    }

    #[must_use]
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Gives the field the keyboard while the bar shows; whether it took it.
    pub fn grab_focus(&self) -> bool {
        self.root.is_visible() && self.entry.grab_focus()
    }

    /// Brings the bar in line with the window's active search; whether it came up for a session just now, which the
    /// caller gives the keyboard.
    pub fn sync(&self, core: &Core) -> bool {
        let Some((session, search)) = shown_search(core, self.shared.window) else {
            self.shown.set(None);
            self.root.set_visible(false);
            return false;
        };
        let fresh = self.shown.replace(Some(session)) != Some(session);
        if (fresh || self.shared.idle()) && self.entry.text() != search.needle {
            self.shared.syncing.set(true);
            self.entry.set_text(&search.needle);
            self.entry.set_position(-1);
            self.shared.syncing.set(false);
        }
        self.counter.set_label(&search.display_text());
        self.root.set_visible(true);
        fresh
    }
}

/// A flat glyph button sending `input`; it never takes the keyboard from the field.
fn button(icon: &str, tip: &str, input: SearchInput, shared: &Rc<Shared>) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tip)
        .focus_on_click(false)
        .build();
    button.add_css_class("flat");
    let shared = Rc::clone(shared);
    button.connect_clicked(move |_| shared.send(input.clone()));
    button
}

/// Typing sends the needle, Return the next step, Shift+Return the previous one, Escape the close.
fn connect_field(entry: &gtk::Entry, shared: &Rc<Shared>) {
    let changes = Rc::clone(shared);
    entry.connect_changed(move |entry| {
        if !changes.syncing.get() {
            changes.send(SearchInput::Needle(entry.text().to_string()));
        }
    });
    let activations = Rc::clone(shared);
    entry.connect_activate(move |_| activations.send(SearchInput::Step(true)));
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let presses = Rc::clone(shared);
    keys.connect_key_pressed(move |controller, keyval, keycode, state| {
        // Find's own chord closes the bar from its field, as the menu item's toggle does
        let find = crate::keymap_dispatch::menu_action(controller, keyval, keycode, state)
            == Some(agterm_core::keymap::BuiltinAction::ToggleSearch);
        match field_key(keyval, state).or(find.then_some(SearchInput::Close)) {
            Some(input) => {
                presses.send(input);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    });
    entry.add_controller(keys);
}

impl AppState {
    /// Window `window`'s search bar asked for `input` (upstream's `updateSearchNeedle`, `navigateSearch`, `endSearch`).
    pub fn search_bar(&mut self, window: Uuid, input: SearchInput) {
        match input {
            SearchInput::Needle(needle) => self.core.search_bar_needle(window, &needle),
            SearchInput::Step(next) => self.core.search_bar_step(window, next),
            SearchInput::Close => self.core.search_bar_close(window),
        }
        self.settle();
    }
}

#[cfg(test)]
mod tests {
    use agterm_core::model::{SearchReport, StatusPane};

    use super::*;

    #[test]
    fn shift_return_steps_back_and_escape_closes() {
        let shift = gdk::ModifierType::SHIFT_MASK;
        let none = gdk::ModifierType::empty();
        assert_eq!(field_key(gdk::Key::Return, shift), Some(SearchInput::Step(false)));
        assert_eq!(
            field_key(gdk::Key::Return, none),
            None,
            "Return activates: the next match"
        );
        assert_eq!(field_key(gdk::Key::Escape, none), Some(SearchInput::Close));
        assert_eq!(field_key(gdk::Key::a, none), None);
    }

    #[test]
    fn the_bar_shows_the_active_sessions_open_search_unless_zoomed() {
        let mut library = agterm_core::model::WindowLibrary::seeded("/home/u");
        let window = library.active_window_id().unwrap();
        let store = library.store_mut(window).unwrap();
        let session = store.sessions().next().unwrap().id();
        let mut core = Core::new(
            agterm_core::CoreConfig {
                app: agterm_proto::AppIdentity::from_recorded("0.1.0", None),
                capabilities: Vec::new(),
            },
            agterm_core::CoreState {
                library,
                ..agterm_core::CoreState::default()
            },
        );
        assert!(shown_search(&core, window).is_none());
        let search = &mut core
            .library_mut()
            .store_mut(window)
            .unwrap()
            .session_mut(session)
            .unwrap()
            .search;
        search.apply(StatusPane::Left, SearchReport::Started(Some("err".to_owned())));
        let (shown, state) = shown_search(&core, window).unwrap();
        assert_eq!((shown, state.needle.as_str()), (session, "err"));
        let zoom = agterm_core::zoom::SurfaceId {
            session,
            surface: agterm_core::zoom::ZoomSurface::Primary,
        };
        core.set_zoom_target(window, Some(zoom));
        assert!(shown_search(&core, window).is_none(), "hidden while zoomed");
    }
}
