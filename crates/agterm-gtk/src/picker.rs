//! The native picker (`pick.open`), after upstream's `Views/Palette.swift` (`CommandPalette` with caller-supplied
//! items) as `WindowContentView.swift`'s `pickPaletteOverlay` drives it.
//!
//! One per window, shown while the window's modal slot holds a pick: a scrim over the whole window content (a click on
//! it cancels) under a palette panel near the top, centered over the terminal area, with a search field (the caller's
//! prompt as its placeholder, its query prefilled) over the rows (a label and an optional subtitle). Typing filters
//! and ranks the rows by their labels ([`fuzzy_rank`]); an empty query keeps the caller's order. The selection opens on
//! the caller's `--select` row when the first filtered list holds it, else the first row, and goes back to the first
//! row on every edit. Up and Down (or Ctrl+J and Ctrl+K) move it, Return or a click picks a row, Escape cancels. With
//! `--allow-custom`, a non-empty query that matches no row offers one row, `Use "<query>"`, which answers with the typed
//! text. The answer reports the row's index in the caller's order, whatever the filter showed.
//!
//! The rows are a pure function of the items and the query ([`filter_rows`]), testable without a display. A new pick
//! id builds a fresh picker; the picker goes when the slot empties, and the deck gives the terminal the keyboard back.

use std::cell::RefCell;
use std::rc::Rc;

use agterm_core::fuzzy::fuzzy_rank;
use agterm_core::modal::PendingPick;
use agterm_core::palette::{palette_search_keys, pick_custom_row_label};
use agterm_proto::{ControlPickOutcome, ControlPickResult, PickItem};
use gtk::prelude::*;
use gtk::{gdk, glib};
use uuid::Uuid;

use crate::keymap_dispatch::{KeyLayout, chord_key, key_layout};
use crate::modal::{self, ModalFrame, PanelRule};
use crate::state::{self, AppState, Poster};

/// The search field's placeholder without a caller's prompt (upstream's picker default).
const DEFAULT_PROMPT: &str = "Select…";
/// The rows' height at the default interface size (upstream's `resultsHeightAtDefaultFontSize`).
const RESULTS_HEIGHT: i32 = 320;

/// One visible row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickRow {
    /// The caller's item at this index, in the caller's order.
    Item(usize),
    /// The free-text answer an unmatched query offers.
    Custom { label: String, query: String },
}

/// The rows `query` shows of `items` (upstream's `updateFiltered` for a caller-supplied picker): every item in the
/// caller's order for a blank query (trimmed of whitespace and newlines), else the items whose label matches, best
/// first (a caller's subtitle is never searched, upstream's `paletteSearchKeys`); with `allow_custom`, a query matching
/// none offers `Use "<query>"` alone (upstream's `pickCustomRowLabel`).
#[must_use]
pub fn filter_rows(items: &[PickItem], query: &str, allow_custom: bool) -> Vec<PickRow> {
    let query = query.trim();
    if query.is_empty() {
        return (0..items.len()).map(PickRow::Item).collect();
    }
    let ranked = fuzzy_rank(query, 0..items.len(), |index| {
        items
            .get(*index)
            .map(|item| palette_search_keys(&item.label, item.subtitle.as_deref(), true))
            .unwrap_or_default()
    });
    if let Some(label) = pick_custom_row_label(query, ranked.len(), allow_custom) {
        return vec![PickRow::Custom {
            label,
            query: query.to_owned(),
        }];
    }
    ranked.into_iter().map(PickRow::Item).collect()
}

/// The row the picker opens on: the caller's `selection` when the first rows show it, else the first.
#[must_use]
pub fn initial_selection(rows: &[PickRow], items: &[PickItem], selection: Option<&str>) -> usize {
    let Some(selection) = selection else { return 0 };
    rows.iter()
        .position(
            |row| matches!(row, PickRow::Item(index) if items.get(*index).is_some_and(|item| item.id == selection)),
        )
        .unwrap_or(0)
}

/// The selection after moving `delta` rows among `count`, held at the ends (upstream's `move`).
#[must_use]
pub fn moved(selection: usize, delta: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    selection.saturating_add_signed(delta).min(count - 1)
}

/// What `row` answers for `pick`; `None` for an item index past its last.
#[must_use]
pub fn outcome(pick: &PendingPick, row: &PickRow) -> Option<ControlPickResult> {
    match row {
        PickRow::Item(index) => pick.picked(*index),
        PickRow::Custom { query, .. } => Some(PendingPick::custom(query)),
    }
}

/// What a key does in the search field, beyond editing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKey {
    /// Moves the selection.
    Move(isize),
    /// Cancels the picker.
    Cancel,
    /// Kept from moving focus out of the picker.
    Swallow,
}

/// The search field's key handling (upstream's `onKeyPress` handlers): Up and Down move, Ctrl+J and Ctrl+K move (with
/// exactly Control held, the key resolved by [`selection_step`] from the press's `layout`), Escape cancels, and Tab
/// stays in the picker.
#[must_use]
pub fn pick_key(
    keyval: gdk::Key,
    keycode: u32,
    state: gdk::ModifierType,
    layout: Option<KeyLayout>,
) -> Option<PickKey> {
    match keyval {
        gdk::Key::Up | gdk::Key::KP_Up => return Some(PickKey::Move(-1)),
        gdk::Key::Down | gdk::Key::KP_Down => return Some(PickKey::Move(1)),
        gdk::Key::Escape => return Some(PickKey::Cancel),
        gdk::Key::Tab | gdk::Key::KP_Tab | gdk::Key::ISO_Left_Tab => return Some(PickKey::Swallow),
        _ => {}
    }
    let chord = gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::SHIFT_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK
        | gdk::ModifierType::META_MASK;
    if state & chord != gdk::ModifierType::CONTROL_MASK {
        return None;
    }
    let layout = layout?;
    selection_step(keycode, layout.produced, layout.ascii).map(PickKey::Move)
}

/// The selection move a Control chord asks for (upstream's `CommandPalette.selectionStep`): +1 for `j`, -1 for `k`,
/// `None` for any other key. The key resolves as keymap chords do ([`chord_key`]), so a layout that types ASCII moves
/// by the letter it types and a non-Latin one by the physical J and K positions.
#[must_use]
pub fn selection_step(keycode: u32, produced: Option<char>, ascii_layout: bool) -> Option<isize> {
    match chord_key(keycode, produced, ascii_layout)?.as_str() {
        "j" => Some(1),
        "k" => Some(-1),
        _ => None,
    }
}

/// The picker key of a press `controller` handles, its layout read from the event (as the keymap reads it).
#[must_use]
pub fn press_key(
    controller: &gtk::EventControllerKey,
    keyval: gdk::Key,
    keycode: u32,
    state: gdk::ModifierType,
) -> Option<PickKey> {
    pick_key(keyval, keycode, state, key_layout(controller, keycode))
}

// MARK: - the widget

/// What the picker's handlers share.
struct PickerState {
    pick: PendingPick,
    rows: Vec<PickRow>,
    selection: usize,
    list: gtk::ListBox,
    scroller: gtk::ScrolledWindow,
    /// An answer went out: later keys and clicks do nothing.
    done: bool,
    resolve: Box<dyn Fn(ControlPickResult)>,
}

impl PickerState {
    /// Shows the rows `query` filters, the selection on the first.
    fn refilter(&mut self, query: &str) {
        self.rows = filter_rows(&self.pick.items, query, self.pick.allow_custom);
        self.list.remove_all();
        for row in &self.rows {
            let widget = match row {
                PickRow::Item(index) => self
                    .pick
                    .items
                    .get(*index)
                    .map(|item| row_widget(&item.label, item.subtitle.as_deref())),
                PickRow::Custom { label, .. } => Some(row_widget(label, None)),
            };
            if let Some(widget) = widget {
                self.list.append(&widget);
            }
        }
        // a scrollbar holds the list at its own minimum height, which leaves a gap under a short list: the rows
        // scroll only once they outgrow the results area
        let (_, natural, _, _) = self.list.measure(gtk::Orientation::Vertical, -1);
        self.scroller.set_vscrollbar_policy(if natural > RESULTS_HEIGHT {
            gtk::PolicyType::Automatic
        } else {
            gtk::PolicyType::Never
        });
        self.select(0);
    }

    fn select(&mut self, index: usize) {
        self.selection = moved(index, 0, self.rows.len());
        let row = i32::try_from(self.selection)
            .ok()
            .and_then(|index| self.list.row_at_index(index));
        self.list.select_row(row.as_ref());
        if let Some(row) = row {
            modal::reveal(&self.scroller, row.upcast_ref());
        }
    }

    fn run(&mut self, index: usize) {
        if let Some(result) = self.rows.get(index).and_then(|row| outcome(&self.pick, row)) {
            self.finish(result);
        }
    }

    fn finish(&mut self, result: ControlPickResult) {
        if !self.done {
            self.done = true;
            (self.resolve)(result);
        }
    }

    fn key(&mut self, key: PickKey) {
        match key {
            PickKey::Move(delta) => {
                let index = moved(self.selection, delta, self.rows.len());
                self.select(index);
            }
            PickKey::Cancel => self.finish(ControlPickResult::outcome(ControlPickOutcome::Cancelled)),
            PickKey::Swallow => {}
        }
    }
}

/// One row: the label over its subtitle, dimmed, middle-truncated.
fn row_widget(title: &str, subtitle: Option<&str>) -> gtk::ListBoxRow {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 1);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    let label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    content.append(&label);
    if let Some(subtitle) = subtitle {
        let detail = gtk::Label::builder()
            .label(subtitle)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        detail.add_css_class("dim-label");
        detail.add_css_class("caption");
        content.append(&detail);
    }
    let row = gtk::ListBoxRow::builder().child(&content).focusable(false).build();
    row.update_property(&[gtk::accessible::Property::Label(title)]);
    row
}

/// One open picker.
struct Picker {
    id: String,
    frame: ModalFrame,
    entry: gtk::SearchEntry,
    state: Rc<RefCell<PickerState>>,
}

impl Picker {
    fn new(pick: &PendingPick, inset: f64, resolve: impl Fn(ControlPickResult) + 'static) -> Self {
        let entry = gtk::SearchEntry::builder()
            .placeholder_text(pick.prompt.as_deref().unwrap_or(DEFAULT_PROMPT))
            .hexpand(true)
            .build();
        entry.set_text(pick.query.as_deref().unwrap_or_default());
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .activate_on_single_click(true)
            .focusable(false)
            .build();
        list.add_css_class("agterm-pick-list");
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .max_content_height(RESULTS_HEIGHT)
            .propagate_natural_height(true)
            .vexpand(true)
            .child(&list)
            .build();
        let panel = panel(&entry, &scroller);
        let scrim = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        scrim.add_css_class(modal::DIM_CLASS);
        let frame = ModalFrame::new(&scrim, &panel, PanelRule::Palette { inset });
        let state = Rc::new(RefCell::new(PickerState {
            pick: pick.clone(),
            rows: Vec::new(),
            selection: 0,
            list: list.clone(),
            scroller,
            done: false,
            resolve: Box::new(resolve),
        }));
        {
            let mut state = state.borrow_mut();
            state.refilter(&entry.text());
            let first = initial_selection(&state.rows, &state.pick.items, pick.selection.as_deref());
            state.select(first);
        }
        connect_picker(&entry, &list, &scrim, &state);
        Self {
            id: pick.id.clone(),
            frame,
            entry,
            state,
        }
    }

    /// Gives the search field the keyboard, the selection scrolled into view; whether it took it.
    fn grab_focus(&self) -> bool {
        let took = self.entry.grab_focus();
        if let Ok(mut state) = self.state.try_borrow_mut() {
            let selection = state.selection;
            state.select(selection);
        }
        took
    }
}

/// The panel: the search field over a divider over the rows (the palettes' too).
pub fn panel(entry: &gtk::SearchEntry, scroller: &gtk::ScrolledWindow) -> gtk::Box {
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for margin in [
        gtk::Box::set_margin_top,
        gtk::Box::set_margin_bottom,
        gtk::Box::set_margin_start,
        gtk::Box::set_margin_end,
    ] {
        margin(&header, 12);
    }
    header.append(entry);
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.set_overflow(gtk::Overflow::Hidden);
    panel.add_css_class(modal::PANEL_CLASS);
    panel.append(&header);
    panel.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    panel.append(scroller);
    panel
}

/// Typing filters, the keys move or cancel, Return or a click picks, a click on the scrim cancels.
fn connect_picker(entry: &gtk::SearchEntry, list: &gtk::ListBox, scrim: &gtk::Box, state: &Rc<RefCell<PickerState>>) {
    let weak = Rc::downgrade(state);
    entry.connect_changed(move |entry| {
        if let Some(state) = weak.upgrade() {
            state.borrow_mut().refilter(&entry.text());
        }
    });
    let weak = Rc::downgrade(state);
    entry.connect_activate(move |_| {
        if let Some(state) = weak.upgrade() {
            let selection = state.borrow().selection;
            state.borrow_mut().run(selection);
        }
    });
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(state);
    keys.connect_key_pressed(move |controller, keyval, keycode, modifiers| {
        let Some(key) = press_key(controller, keyval, keycode, modifiers) else {
            return glib::Propagation::Proceed;
        };
        if let Some(state) = weak.upgrade() {
            state.borrow_mut().key(key);
        }
        glib::Propagation::Stop
    });
    entry.add_controller(keys);
    let weak = Rc::downgrade(state);
    list.connect_row_activated(move |_, row| {
        let (Some(state), Ok(index)) = (weak.upgrade(), usize::try_from(row.index())) else {
            return;
        };
        state.borrow_mut().run(index);
    });
    let click = gtk::GestureClick::new();
    let weak = Rc::downgrade(state);
    click.connect_pressed(move |_, _, _, _| {
        if let Some(state) = weak.upgrade() {
            state.borrow_mut().key(PickKey::Cancel);
        }
    });
    scrim.add_controller(click);
}

/// A window's picker layer: a permanent overlay child over the window's content, holding the picker while one is open.
pub struct WindowPicker {
    layer: gtk::Box,
    current: Option<Picker>,
}

impl WindowPicker {
    #[must_use]
    pub fn new() -> Self {
        let layer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        layer.set_visible(false);
        Self { layer, current: None }
    }

    #[must_use]
    pub fn widget(&self) -> &gtk::Box {
        &self.layer
    }

    /// Whether a picker is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.current.is_some()
    }

    /// Gives the picker's search field the keyboard; whether one took it.
    pub fn grab_focus(&self) -> bool {
        self.current.as_ref().is_some_and(Picker::grab_focus)
    }

    /// Brings the layer in line with window `window`'s pick, its panel centered over the terminal area `inset` points
    /// from the left; whether a new picker came up, which the caller gives the keyboard.
    pub fn sync(&mut self, pick: Option<&PendingPick>, inset: f64, window: Uuid, poster: &Poster) -> bool {
        let fresh = pick.is_some_and(|pick| self.current.as_ref().is_none_or(|current| current.id != pick.id));
        if (pick.is_none() || fresh)
            && let Some(current) = self.current.take()
        {
            self.layer.remove(&current.frame);
        }
        let Some(pick) = pick else {
            self.layer.set_visible(false);
            return false;
        };
        if fresh {
            let (posting, id) = (poster.clone(), pick.id.clone());
            let picker = Picker::new(pick, inset, move |result| {
                let id = id.clone();
                state::post(&posting, move |state: &mut AppState| {
                    state.resolve_pick(window, &id, result);
                });
            });
            self.layer.append(&picker.frame);
            self.current = Some(picker);
        }
        if let Some(current) = &self.current {
            current.frame.set_rule(PanelRule::Palette { inset });
        }
        self.layer.set_visible(true);
        fresh
    }
}

impl Default for WindowPicker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, label: &str) -> PickItem {
        PickItem {
            id: id.to_owned(),
            label: label.to_owned(),
            subtitle: Some(format!("{label} subtitle")),
        }
    }

    fn items() -> Vec<PickItem> {
        vec![
            item("b", "beta"),
            item("a", "Alpha"),
            item("ns", "New Session"),
            item("nw", "New Workspace"),
        ]
    }

    fn pick(allow_custom: bool) -> PendingPick {
        PendingPick {
            id: "P1".to_owned(),
            items: items(),
            prompt: None,
            query: None,
            allow_custom,
            selection: Some("ns".to_owned()),
        }
    }

    #[test]
    fn a_blank_query_keeps_the_callers_order() {
        let all: Vec<PickRow> = (0..4).map(PickRow::Item).collect();
        assert_eq!(filter_rows(&items(), "", false), all);
        assert_eq!(filter_rows(&items(), " \n\t", true), all, "trimmed of newlines too");
    }

    #[test]
    fn a_query_ranks_labels_best_first_and_drops_misses() {
        assert_eq!(
            filter_rows(&items(), "new", false),
            [PickRow::Item(2), PickRow::Item(3)]
        );
        assert_eq!(filter_rows(&items(), "work", false), [PickRow::Item(3)]);
        assert!(
            filter_rows(&items(), "subtitle", false).is_empty(),
            "a caller's subtitle is not searched"
        );
    }

    #[test]
    fn an_unmatched_query_offers_custom_text_only_when_allowed() {
        assert!(filter_rows(&items(), "zzz", false).is_empty());
        assert_eq!(
            filter_rows(&items(), "  zzz top ", true),
            [PickRow::Custom {
                label: "Use \"zzz top\"".to_owned(),
                query: "zzz top".to_owned()
            }]
        );
        assert_eq!(
            filter_rows(&items(), "alp", true),
            [PickRow::Item(1)],
            "a match offers no custom row"
        );
    }

    #[test]
    fn the_selection_opens_on_the_callers_row_else_the_first() {
        let items = items();
        let rows = filter_rows(&items, "", false);
        assert_eq!(initial_selection(&rows, &items, Some("nw")), 3);
        assert_eq!(initial_selection(&rows, &items, None), 0);
        let filtered = filter_rows(&items, "alp", false);
        assert_eq!(
            initial_selection(&filtered, &items, Some("nw")),
            0,
            "a prefill hiding it leaves the first row"
        );
    }

    #[test]
    fn moving_holds_at_the_ends() {
        assert_eq!(moved(0, -1, 3), 0);
        assert_eq!(moved(1, 1, 3), 2);
        assert_eq!(moved(2, 1, 3), 2);
        assert_eq!(moved(5, 0, 3), 2);
        assert_eq!(moved(0, 1, 0), 0);
    }

    #[test]
    fn a_row_answers_with_its_callers_index_or_the_typed_text() {
        let pick = pick(true);
        let picked = outcome(&pick, &PickRow::Item(2)).unwrap();
        assert_eq!(picked.result, ControlPickOutcome::Picked);
        assert_eq!(picked.id.as_deref(), Some("ns"));
        assert_eq!(picked.index, Some(2));
        let custom = outcome(
            &pick,
            &PickRow::Custom {
                label: String::new(),
                query: "zzz".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(custom.result, ControlPickOutcome::Custom);
        assert_eq!(custom.query.as_deref(), Some("zzz"));
        assert_eq!(outcome(&pick, &PickRow::Item(9)), None);
    }

    /// The XKB keycodes of the J and K keys.
    const KEYCODE_J: u32 = 44;
    const KEYCODE_K: u32 = 45;

    fn us(produced: char) -> KeyLayout {
        KeyLayout {
            produced: Some(produced),
            ascii: true,
        }
    }

    #[test]
    fn keys_move_cancel_and_stay_in_the_picker() {
        let none = gdk::ModifierType::empty();
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        assert_eq!(pick_key(gdk::Key::Up, 0, none, None), Some(PickKey::Move(-1)));
        assert_eq!(pick_key(gdk::Key::Down, 0, none, None), Some(PickKey::Move(1)));
        assert_eq!(pick_key(gdk::Key::Escape, 0, none, None), Some(PickKey::Cancel));
        assert_eq!(pick_key(gdk::Key::Tab, 0, none, None), Some(PickKey::Swallow));
        assert_eq!(
            pick_key(gdk::Key::j, KEYCODE_J, ctrl, Some(us('j'))),
            Some(PickKey::Move(1))
        );
        assert_eq!(
            pick_key(gdk::Key::k, KEYCODE_K, ctrl, Some(us('k'))),
            Some(PickKey::Move(-1))
        );
        let russian = Some(KeyLayout {
            produced: Some('о'),
            ascii: false,
        });
        assert_eq!(
            pick_key(gdk::Key::Cyrillic_o, KEYCODE_J, ctrl, russian),
            Some(PickKey::Move(1)),
            "the physical key on a non-Latin layout"
        );
        assert_eq!(
            pick_key(gdk::Key::j, KEYCODE_J, none, Some(us('j'))),
            None,
            "typed text"
        );
        for held in [
            gdk::ModifierType::SHIFT_MASK,
            gdk::ModifierType::ALT_MASK,
            gdk::ModifierType::SUPER_MASK,
            gdk::ModifierType::META_MASK,
        ] {
            assert_eq!(
                pick_key(gdk::Key::j, KEYCODE_J, ctrl | held, Some(us('j'))),
                None,
                "{held:?}"
            );
        }
        assert_eq!(pick_key(gdk::Key::a, 38, ctrl, Some(us('a'))), None);
        assert_eq!(
            pick_key(gdk::Key::j, KEYCODE_J, ctrl, None),
            None,
            "no layout, no letter"
        );
    }

    // upstream's PaletteTests, by XKB keycodes (J 44, K 45, Dvorak's j on C 54 and k on V 55, A 38)
    fn step(keycode: u32, produced: Option<char>, ascii: bool) -> Option<isize> {
        selection_step(keycode, produced, ascii)
    }

    #[test]
    fn latin_layout_moves_by_the_typed_letter() {
        assert_eq!(step(KEYCODE_J, Some('j'), true), Some(1));
        assert_eq!(step(KEYCODE_K, Some('k'), true), Some(-1));
        assert_eq!(step(KEYCODE_J, Some('J'), true), Some(1), "caps lock");
        assert_eq!(step(KEYCODE_K, Some('K'), true), Some(-1), "caps lock");
    }

    // regression: on a Cyrillic layout the J and K keys type `о` and `л`, and the selection did not move
    #[test]
    fn non_latin_layout_moves_by_the_physical_position() {
        assert_eq!(step(KEYCODE_J, Some('о'), false), Some(1));
        assert_eq!(step(KEYCODE_K, Some('л'), false), Some(-1));
        assert_eq!(step(KEYCODE_J, None, false), Some(1));
    }

    // on Dvorak the physical J position types `h`, and `j` sits on the C key
    #[test]
    fn alternative_latin_layout_keeps_its_own_letter_positions() {
        assert_eq!(step(KEYCODE_J, Some('h'), true), None);
        assert_eq!(step(54, Some('j'), true), Some(1));
        assert_eq!(step(55, Some('k'), true), Some(-1));
    }

    #[test]
    fn other_keys_do_not_move() {
        assert_eq!(step(38, Some('a'), true), None);
        assert_eq!(step(38, Some('ф'), false), None);
        assert_eq!(
            step(KEYCODE_J, Some('о'), true),
            None,
            "a Latin layout typing a non-ASCII character binds what it types"
        );
        assert_eq!(step(KEYCODE_J, None, true), None);
    }

    #[test]
    fn the_picker_filters_moves_and_answers_once() {
        crate::gtk_test::on_gtk_thread(picker_filters_moves_and_answers_once);
    }

    fn picker_filters_moves_and_answers_once() {
        let answers: Rc<RefCell<Vec<ControlPickResult>>> = Rc::default();
        let seen = Rc::clone(&answers);
        let mut pending = pick(true);
        pending.query = Some("new".to_owned());
        // the caller's row is not the filter's first, so the seeding is told apart from the first-row fallback
        pending.selection = Some("nw".to_owned());
        let picker = Picker::new(&pending, 0.0, move |result| seen.borrow_mut().push(result));
        assert_eq!(
            picker.state.borrow().rows,
            [PickRow::Item(2), PickRow::Item(3)],
            "the prefill filters"
        );
        assert_eq!(picker.state.borrow().selection, 1, "the caller's row is selected");
        let selected = || picker.state.borrow().list.selected_row().map(|row| row.index());
        assert_eq!(selected(), Some(1));
        picker.state.borrow_mut().key(PickKey::Move(-1));
        assert_eq!(selected(), Some(0));
        picker.entry.set_text("zzz");
        assert_eq!(picker.state.borrow().rows.len(), 1, "the custom row");
        assert_eq!(selected(), Some(0), "an edit goes back to the first row");
        picker.entry.set_text("");
        assert_eq!(picker.state.borrow().rows.len(), 4);
        picker.state.borrow_mut().key(PickKey::Move(1));
        picker.entry.emit_activate();
        picker.state.borrow_mut().key(PickKey::Cancel);
        let answers = answers.borrow();
        assert_eq!(answers.len(), 1, "the first answer only");
        assert_eq!(answers[0].index, Some(1), "the caller's index");
    }
}
