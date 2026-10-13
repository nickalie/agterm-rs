//! The Ctrl-Tab session switcher (upstream's `Views/SessionSwitcher.swift`): hold Control and press Tab to walk the
//! window's most recently used sessions (Shift+Tab walks back), release Control to select the highlighted one, Escape
//! to give up.
//!
//! The order is the window's session recency over the navigable set, at most [`MAX_CANDIDATES`]
//! ([`agterm_core::Core::switcher_candidates`]), frozen when the walk begins so cycling never reorders it; only the
//! commit does, by selecting. The walk begins on the previous session and does nothing with fewer than two.
//!
//! One capture-phase key controller per window sees Ctrl+Tab before the terminal and the keymap: it is always consumed,
//! and while the window takes no UI actions it only ends a walk. A walk also ends when the window loses the keyboard.
//! The overlay is a click-through, unfocusable panel of the candidates over a light scrim, so the terminal keeps the
//! keyboard throughout.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use agterm_core::Core;
use gtk::{gdk, glib};
use uuid::Uuid;

use crate::modal::{self, ModalFrame, PanelRule};
use crate::state::{self, AppState, Poster};

/// The most sessions a walk offers (upstream's `SessionSwitcher.maxCandidates`).
pub const MAX_CANDIDATES: usize = 10;

/// A walk through the recent sessions (upstream's `SessionSwitcher` state).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Switcher {
    candidates: Vec<Uuid>,
    index: usize,
    active: bool,
}

impl Switcher {
    /// Whether a walk is under way.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The highlighted candidate's position.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// Begins a walk over `order`, most recent first, on the previous session; nothing with fewer than two. Whether
    /// it began.
    pub fn begin(&mut self, order: Vec<Uuid>) -> bool {
        if order.len() < 2 {
            return false;
        }
        self.candidates = order;
        self.index = 1;
        self.active = true;
        true
    }

    /// Steps the highlight, wrapping at either end.
    pub fn advance(&mut self, reverse: bool) {
        let count = self.candidates.len();
        if count == 0 {
            return;
        }
        self.index = if reverse {
            (self.index + count - 1) % count
        } else {
            (self.index + 1) % count
        };
    }

    /// Ends the walk, answering the highlighted session.
    pub fn commit(&mut self) -> Option<Uuid> {
        let picked = self.active.then(|| self.candidates.get(self.index).copied()).flatten();
        self.reset();
        picked
    }

    /// Ends the walk without selecting.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// What a key press means to the switcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchKey {
    /// Ctrl+Tab, or Ctrl+Shift+Tab walking back.
    Tab { reverse: bool },
    /// Escape.
    Escape,
}

/// The switcher's reading of a press.
#[must_use]
pub fn switch_key(keyval: gdk::Key, state: gdk::ModifierType) -> Option<SwitchKey> {
    match keyval {
        gdk::Key::Tab | gdk::Key::KP_Tab | gdk::Key::ISO_Left_Tab
            if state.contains(gdk::ModifierType::CONTROL_MASK) =>
        {
            Some(SwitchKey::Tab {
                reverse: keyval == gdk::Key::ISO_Left_Tab || state.contains(gdk::ModifierType::SHIFT_MASK),
            })
        }
        gdk::Key::Escape => Some(SwitchKey::Escape),
        _ => None,
    }
}

/// One candidate row: the display name over `workspace · detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    id: Uuid,
    title: String,
    subtitle: String,
}

/// What the window's controller and overlay share.
struct Shared {
    window: Uuid,
    poster: Poster,
    switcher: Switcher,
    /// Whether the window takes UI actions as of the last sync.
    can_switch: bool,
    /// The recency order as of the last sync.
    candidates: Vec<Candidate>,
    /// The rows of the walk under way.
    frozen: Vec<Candidate>,
    layer: gtk::Box,
    list: gtk::ListBox,
}

impl Shared {
    fn press(&mut self, key: SwitchKey) -> bool {
        match key {
            SwitchKey::Tab { .. } if !self.can_switch => self.switcher.reset(),
            SwitchKey::Tab { reverse } if self.switcher.is_active() => self.switcher.advance(reverse),
            SwitchKey::Tab { .. } => {
                if self
                    .switcher
                    .begin(self.candidates.iter().map(|candidate| candidate.id).collect())
                {
                    self.frozen.clone_from(&self.candidates);
                    self.render_rows();
                }
            }
            SwitchKey::Escape if self.switcher.is_active() => self.switcher.reset(),
            SwitchKey::Escape => return false,
        }
        self.render();
        true
    }

    /// Control went up: the walk selects its highlighted session.
    fn release(&mut self) {
        if let Some(session) = self.switcher.commit() {
            let window = self.window;
            state::post(&self.poster, move |state: &mut AppState| {
                state.switch_to_session(window, session);
            });
        }
        self.render();
    }

    fn reset(&mut self) {
        if self.switcher.is_active() {
            self.switcher.reset();
            self.render();
        }
    }

    fn render_rows(&self) {
        self.list.remove_all();
        for candidate in &self.frozen {
            self.list.append(&row_widget(candidate));
        }
    }

    fn render(&self) {
        let active = self.switcher.is_active();
        self.layer.set_visible(active);
        if !active {
            return;
        }
        let row = i32::try_from(self.switcher.index())
            .ok()
            .and_then(|index| self.list.row_at_index(index));
        self.list.select_row(row.as_ref());
    }
}

fn row_widget(candidate: &Candidate) -> gtk::ListBoxRow {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 1);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    let title = gtk::Label::builder()
        .label(&candidate.title)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let subtitle = gtk::Label::builder()
        .label(&candidate.subtitle)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .build();
    subtitle.add_css_class("dim-label");
    subtitle.add_css_class("caption");
    content.append(&title);
    content.append(&subtitle);
    gtk::ListBoxRow::builder().child(&content).focusable(false).build()
}

/// One window's switcher: its key controller's state and its overlay layer.
pub struct WindowSwitcher {
    shared: Rc<RefCell<Shared>>,
    frame: ModalFrame,
}

impl WindowSwitcher {
    /// A switcher for window `window`, posting through `poster`.
    #[must_use]
    pub fn new(window: Uuid, poster: &Poster) -> Self {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .focusable(false)
            .build();
        list.add_css_class("agterm-pick-list");
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        panel.set_overflow(gtk::Overflow::Hidden);
        panel.add_css_class(modal::PANEL_CLASS);
        panel.append(&list);
        let scrim = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        scrim.add_css_class(modal::DIM_CLASS);
        let frame = ModalFrame::new(&scrim, &panel, PanelRule::Palette { inset: 0.0 });
        let layer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        layer.append(&frame);
        layer.set_visible(false);
        // click-through and unfocusable: the terminal keeps the keyboard and the pointer
        layer.set_can_target(false);
        layer.set_can_focus(false);
        let shared = Rc::new(RefCell::new(Shared {
            window,
            poster: poster.clone(),
            switcher: Switcher::default(),
            can_switch: false,
            candidates: Vec::new(),
            frozen: Vec::new(),
            layer,
            list,
        }));
        Self { shared, frame }
    }

    /// The overlay layer.
    #[must_use]
    pub fn widget(&self) -> gtk::Box {
        self.shared.borrow().layer.clone()
    }

    /// Installs the key controller on `window`, ahead of the keymap's; a walk ends when the window loses the keyboard.
    pub fn install(&self, window: &adw::ApplicationWindow) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let shared = Rc::downgrade(&self.shared);
        keys.connect_key_pressed(move |_, keyval, _, state| {
            let (Some(shared), Some(key)) = (shared.upgrade(), switch_key(keyval, state)) else {
                return glib::Propagation::Proceed;
            };
            if shared.borrow_mut().press(key) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        let shared = Rc::downgrade(&self.shared);
        keys.connect_key_released(move |_, keyval, _, _| {
            if matches!(keyval, gdk::Key::Control_L | gdk::Key::Control_R)
                && let Some(shared) = shared.upgrade()
            {
                shared.borrow_mut().release();
            }
        });
        window.add_controller(keys);
        let shared = Rc::downgrade(&self.shared);
        window.connect_is_active_notify(move |window| {
            if !window.is_active()
                && let Some(shared) = shared.upgrade()
            {
                shared.borrow_mut().reset();
            }
        });
    }

    /// Takes the window's recency order and whether it takes UI actions; a walk ends when it stops taking them.
    pub fn sync(&self, core: &Core, inset: f64) {
        let mut shared = self.shared.borrow_mut();
        let window = shared.window;
        shared.can_switch = core.library().active_window_id() == Some(window) && core.ui_actions_enabled(window);
        shared.candidates = candidates(core, window);
        if !shared.can_switch {
            shared.reset();
        }
        self.frame.set_rule(PanelRule::Palette { inset });
    }
}

/// Window `window`'s candidates in recency order, with their rows.
fn candidates(core: &Core, window: Uuid) -> Vec<Candidate> {
    let Some(store) = core.library().store(window) else {
        return Vec::new();
    };
    core.switcher_candidates(window, MAX_CANDIDATES)
        .into_iter()
        .filter_map(|id| {
            let session = store.session(id)?;
            let workspace = store
                .workspace_for_session(id)
                .map_or("", |workspace| workspace.name.as_str());
            Some(Candidate {
                id,
                title: session.display_name(),
                subtitle: format!("{workspace} · {}", session.switcher_detail()),
            })
        })
        .collect()
}

impl AppState {
    /// The Ctrl-Tab walk in window `window` ended on `session`.
    pub fn switch_to_session(&mut self, window: Uuid, session: Uuid) {
        self.core.switch_to_session(window, session);
        self.settle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(count: usize) -> Vec<Uuid> {
        (0..count).map(|_| Uuid::new_v4()).collect()
    }

    #[test]
    fn a_walk_needs_two_sessions_and_starts_on_the_previous_one() {
        let mut switcher = Switcher::default();
        assert!(!switcher.begin(ids(1)));
        assert!(!switcher.is_active());
        let order = ids(3);
        assert!(switcher.begin(order.clone()));
        assert_eq!(switcher.index(), 1);
        assert_eq!(switcher.commit(), Some(order[1]));
        assert!(!switcher.is_active(), "the commit ends the walk");
        assert_eq!(switcher.commit(), None);
    }

    #[test]
    fn the_highlight_wraps_both_ways_and_a_reset_selects_nothing() {
        let mut switcher = Switcher::default();
        let order = ids(3);
        switcher.begin(order.clone());
        switcher.advance(false);
        assert_eq!(switcher.index(), 2);
        switcher.advance(false);
        assert_eq!(switcher.index(), 0, "wraps forward");
        switcher.advance(true);
        assert_eq!(switcher.index(), 2, "wraps back");
        switcher.advance(true);
        assert_eq!(switcher.commit(), Some(order[1]));
        switcher.begin(order);
        switcher.reset();
        assert_eq!(switcher.commit(), None);
    }

    #[test]
    fn ctrl_tab_walks_ctrl_shift_tab_walks_back_and_plain_tab_is_typing() {
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        let shift = gdk::ModifierType::SHIFT_MASK;
        assert_eq!(switch_key(gdk::Key::Tab, ctrl), Some(SwitchKey::Tab { reverse: false }));
        assert_eq!(
            switch_key(gdk::Key::ISO_Left_Tab, ctrl | shift),
            Some(SwitchKey::Tab { reverse: true })
        );
        assert_eq!(switch_key(gdk::Key::Tab, gdk::ModifierType::empty()), None);
        assert_eq!(
            switch_key(gdk::Key::Escape, gdk::ModifierType::empty()),
            Some(SwitchKey::Escape)
        );
    }
}
