//! The lead cover over a live pane whose zmx client does not lead (upstream's `Views/PaneLeadCover.swift` and the key
//! handling of `Ghostty/PaneLead.swift`): the terminal draws output laid out for another client's grid, so the pane
//! shows an opaque cover saying so, and a key press on it takes the pane over in a fresh terminal. A click on the
//! cover gives the pane the keyboard, so the next press can take it over.
//!
//! Each page keeps one cover per pane position, laid over the pane by its [`crate::split::PaneStack`] above the
//! terminals; the core decides what it says ([`Core::pane_lead_cover`]).

use std::cell::RefCell;
use std::rc::Rc;

use agterm_core::{Core, LeadCover};
use gtk::glib;
use gtk::prelude::*;
use uuid::Uuid;

use crate::state::{self, AppState, Poster};
use crate::surface::TerminalSurface;

/// What the cover asks for under its title while no takeover is on its way.
const TAKEOVER_HINT: &str = "Press any key to use it here";

/// One pane position's cover.
pub struct LeadCoverWidget {
    root: gtk::Box,
    title: gtk::Label,
    hint: gtk::Label,
    /// The terminal under the cover, which a click focuses.
    target: Rc<RefCell<glib::WeakRef<gtk::GLArea>>>,
}

impl LeadCoverWidget {
    /// A hidden cover over no terminal yet.
    #[must_use]
    pub fn new() -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .valign(gtk::Align::Fill)
            .halign(gtk::Align::Fill)
            .visible(false)
            .build();
        root.add_css_class("view");
        let icon = gtk::Image::from_icon_name("view-dual-symbolic");
        icon.set_pixel_size(28);
        icon.set_valign(gtk::Align::End);
        icon.set_vexpand(true);
        let title = gtk::Label::new(None);
        title.add_css_class("heading");
        let hint = gtk::Label::new(Some(TAKEOVER_HINT));
        hint.add_css_class("dim-label");
        hint.set_valign(gtk::Align::Start);
        hint.set_vexpand(true);
        root.append(&icon);
        root.append(&title);
        root.append(&hint);
        let target: Rc<RefCell<glib::WeakRef<gtk::GLArea>>> = Rc::default();
        let click = gtk::GestureClick::new();
        let focused = Rc::clone(&target);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(area) = focused.borrow().upgrade() {
                area.grab_focus();
            }
        });
        root.add_controller(click);
        Self {
            root,
            title,
            hint,
            target,
        }
    }

    /// Lays the cover over `terminal`, which a click on it focuses.
    pub fn set_target(&self, terminal: &TerminalSurface) {
        self.target.borrow().set(Some(terminal.widget()));
    }

    /// The widget the page lays over its pane.
    #[must_use]
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Shows `cover`, or hides the cover with `None` or while the pane is not shown.
    pub fn show(&self, cover: Option<&LeadCover>, shown: bool) {
        match cover.filter(|_| shown) {
            Some(cover) => {
                self.title.set_label(cover.title);
                self.hint.set_visible(cover.offers_takeover);
                self.root.set_visible(true);
            }
            None => self.root.set_visible(false),
        }
    }
}

/// Wires a pane terminal's takeover: a key press while it is covered asks the core to attach the pane again with the
/// lead.
pub fn connect_takeover(terminal: &TerminalSurface, session: Uuid, token: &str, poster: &Poster) {
    let (posting, token) = (poster.clone(), token.to_owned());
    terminal.connect_takeover(move || {
        let token = token.clone();
        state::post(&posting, move |state: &mut AppState| {
            if state.core.take_pane_lead(session, &token) {
                state.settle();
            }
        });
    });
}

/// The cover each pane token of `tokens` needs now, from the core's lead book.
#[must_use]
pub fn covers(core: &Core, tokens: &[String]) -> Vec<(String, Option<LeadCover>)> {
    tokens
        .iter()
        .map(|token| (token.clone(), core.pane_lead_cover(token)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cover_shows_its_title_and_hides_the_hint_while_taking_over() {
        let cover = LeadCoverWidget::new();
        let following = LeadCover {
            title: "In use from another machine",
            offers_takeover: true,
        };
        cover.show(Some(&following), true);
        assert!(cover.widget().is_visible());
        assert_eq!(cover.title.label(), "In use from another machine");
        assert!(cover.hint.is_visible());
        let taking_over = LeadCover {
            title: "Taking over…",
            offers_takeover: false,
        };
        cover.show(Some(&taking_over), true);
        assert!(!cover.hint.is_visible());
        cover.show(Some(&taking_over), false);
        assert!(!cover.widget().is_visible());
        cover.show(None, true);
        assert!(!cover.widget().is_visible());
    }

    #[test]
    fn a_cover_shows_what_the_core_decided() {
        crate::gtk_test::on_gtk_thread(cover_shows_its_title_and_hides_the_hint_while_taking_over);
    }
}
