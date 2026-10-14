//! The command palettes (upstream's `Views/Palette.swift`, `CommandPalette` over the built-in feeds), drawn in the
//! frontmost window while [`agterm_core::palette`] holds one open.
//!
//! The same frame as the native picker ([`crate::picker`]): a scrim over the window's content (a click on it closes the
//! palette) under a panel near the top, centered over the terminal area, with a search field over the rows. A row shows
//! its title (dimmed while it cannot run) over an optional subtitle, a leading status glyph on the attention palette, a
//! trailing badge (`custom`, `current`) and its shortcut: a built-in's live chord as GTK labels accelerators, a custom
//! command's as the user wrote it. Typing ranks the rows by title and subtitle ([`filter_entries`]): a blank query
//! lists them A→Z, the attention palette in its own order. Up and Down (or Ctrl+J and Ctrl+K) move the selection,
//! Return or a click runs the row when it can run, Escape closes; a palette launcher's chord still reaches its action
//! from the field, so the same chord closes or switches the palette.
//!
//! The theme palette opens on the theme on screen and previews the selected row live, debounced (upstream's 70 ms
//! preview debounce); its run commits the theme.
//!
//! The core decides when a palette shows and with which rows; every sync hands the window a fresh [`PaletteView`],
//! which a new generation builds afresh (an empty query) and an unchanged one only refreshes. What the user does is
//! posted to [`AppState`] with the generation it was built for, so a stale palette's request does nothing.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use agterm_core::Core;
use agterm_core::keymap::BuiltinAction;
use agterm_core::palette::{PaletteAction, PaletteEntry, PaletteMode, PaletteShortcut, filter_entries};
use gtk::prelude::*;
use gtk::{gdk, glib};
use uuid::Uuid;

use crate::keymap_dispatch::{Fired, KeyOrigin};
use crate::modal::{self, ModalFrame, PanelRule};
use crate::picker::{PickKey, moved, press_key};
use crate::state::{self, AppState, Poster};
use crate::status_glyph::{GlyphLook, StatusGlyph};

/// The rows' height at the default interface size (upstream's `resultsHeightAtDefaultFontSize`).
const RESULTS_HEIGHT: i32 = 320;
/// How long the theme palette waits for the selection to settle before previewing it (upstream's
/// `previewThemeDebounceInterval`).
const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(70);
/// The launchers whose chord reaches its action from the search field.
const LAUNCHERS: [BuiltinAction; 5] = [
    BuiltinAction::SessionPalette,
    BuiltinAction::CommandPalette,
    BuiltinAction::CustomCommandPalette,
    BuiltinAction::ShowAttention,
    BuiltinAction::SelectTheme,
];

/// What the frontmost window shows of the open palette: its rows and how each renders now.
#[derive(Debug, Clone, PartialEq)]
pub struct PaletteView {
    pub mode: PaletteMode,
    pub generation: u64,
    pub entries: Vec<PaletteEntry>,
    pub looks: Vec<RowLook>,
    /// The row the palette opens on (the theme on screen); the first row otherwise.
    pub initial: Option<String>,
}

/// How one row renders beyond its entry.
#[derive(Debug, Clone, PartialEq)]
pub struct RowLook {
    /// Whether the row can run now: an inert row renders dimmed and neither runs nor closes the palette.
    pub enabled: bool,
    pub shortcut: Option<String>,
    pub glyph: Option<GlyphLook>,
}

impl PaletteView {
    /// The palette window `window` draws now, `None` when it draws none.
    #[must_use]
    pub fn of(core: &Core, window: Uuid) -> Option<Self> {
        let mode = core.palette_mode().filter(|_| core.palette_holds(window))?;
        let entries = core.palette_entries(mode);
        let looks = entries
            .iter()
            .map(|entry| RowLook {
                enabled: core.palette_entry_enabled(window, &entry.action),
                shortcut: entry.shortcut.as_ref().and_then(shortcut_label),
                glyph: entry
                    .indicator
                    .as_ref()
                    .and_then(|indicator| crate::status_glyph::resolve(indicator, core.settings())),
            })
            .collect();
        Some(Self {
            mode,
            generation: core.palette_generation(),
            entries,
            looks,
            initial: (mode == PaletteMode::Themes).then(|| core.palette_current_theme_id()),
        })
    }
}

/// A shortcut as the row shows it: a chord by GTK's accelerator label (`Shift+Ctrl+T`), a custom command's as written.
#[must_use]
pub fn shortcut_label(shortcut: &PaletteShortcut) -> Option<String> {
    match shortcut {
        PaletteShortcut::Text(text) => Some(text.clone()),
        PaletteShortcut::Chord(chord) => {
            let accel = crate::app_menu::accelerator(chord)?;
            let (key, mods) = gtk::accelerator_parse(&accel)?;
            Some(gtk::accelerator_get_label(key, mods).to_string())
        }
    }
}

/// The selection after the rows changed under it: the row with `id` when it still shows, else the old index clamped.
#[must_use]
pub fn reselect(entries: &[PaletteEntry], shown: &[usize], id: Option<&str>, index: usize) -> usize {
    id.and_then(|id| {
        shown
            .iter()
            .position(|row| entries.get(*row).is_some_and(|entry| entry.id == id))
    })
    .unwrap_or_else(|| moved(index, 0, shown.len()))
}

/// What the palette asks of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    Run(PaletteAction),
    Close,
    Preview(Option<String>),
    Launcher(BuiltinAction),
}

/// What the palette's handlers share.
struct PaletteState {
    view: PaletteView,
    /// The rows shown, as indices into the view's entries.
    shown: Vec<usize>,
    selection: usize,
    query: String,
    list: gtk::ListBox,
    scroller: gtk::ScrolledWindow,
    post: Box<dyn Fn(Request)>,
}

impl PaletteState {
    fn entry_at(&self, row: usize) -> Option<(&PaletteEntry, &RowLook)> {
        let index = *self.shown.get(row)?;
        Some((self.view.entries.get(index)?, self.view.looks.get(index)?))
    }

    /// Shows the rows `query` ranks, the selection on the first, previewing it (upstream's query `onChange`).
    fn refilter(&mut self, query: &str) {
        query.clone_into(&mut self.query);
        self.shown = filter_entries(&self.view.entries, query, self.view.mode);
        self.render();
        self.select(0, true);
    }

    /// The rows changed under an unchanged palette: the query stays, the selection stays on its row where it can.
    fn refresh(&mut self, view: PaletteView) {
        let selected = self.entry_at(self.selection).map(|(entry, _)| entry.id.clone());
        self.view = view;
        self.shown = filter_entries(&self.view.entries, &self.query, self.view.mode);
        self.render();
        let index = reselect(&self.view.entries, &self.shown, selected.as_deref(), self.selection);
        self.select(index, false);
    }

    fn render(&self) {
        self.list.remove_all();
        for row in 0..self.shown.len() {
            if let Some((entry, look)) = self.entry_at(row) {
                self.list.append(&row_widget(entry, look));
            }
        }
        let (_, natural, _, _) = self.list.measure(gtk::Orientation::Vertical, -1);
        self.scroller.set_vscrollbar_policy(if natural > RESULTS_HEIGHT {
            gtk::PolicyType::Automatic
        } else {
            gtk::PolicyType::Never
        });
    }

    /// Selects row `index`, previewing a theme row when `preview` (upstream's `onSelect` on a selection change).
    fn select(&mut self, index: usize, preview: bool) {
        self.selection = moved(index, 0, self.shown.len());
        let row = i32::try_from(self.selection)
            .ok()
            .and_then(|index| self.list.row_at_index(index));
        self.list.select_row(row.as_ref());
        self.reveal_selection();
        if preview {
            self.schedule_preview();
        }
    }

    /// Scrolls the selected row into view; run again whenever the rows are laid out anew, as a row added a moment
    /// ago has no place yet to scroll to.
    fn reveal_selection(&self) {
        let row = i32::try_from(self.selection)
            .ok()
            .and_then(|index| self.list.row_at_index(index));
        if let Some(row) = row {
            modal::reveal(&self.scroller, row.upcast_ref());
        }
    }

    fn schedule_preview(&mut self) {
        if self.view.mode != PaletteMode::Themes {
            return;
        }
        let Some(PaletteAction::Theme(name)) = self.entry_at(self.selection).map(|(entry, _)| entry.action.clone())
        else {
            return;
        };
        (self.post)(Request::Preview(name));
    }

    /// Runs row `row` when it can run now; an inert row does nothing and the palette stays.
    fn run(&self, row: usize) {
        if let Some((entry, look)) = self.entry_at(row)
            && look.enabled
        {
            (self.post)(Request::Run(entry.action.clone()));
        }
    }

    fn key(&mut self, key: PickKey) {
        match key {
            PickKey::Move(delta) => {
                let index = moved(self.selection, delta, self.shown.len());
                self.select(index, true);
            }
            PickKey::Cancel => (self.post)(Request::Close),
            PickKey::Swallow => {}
        }
    }
}

/// One row: the status glyph, the title over its subtitle, the badge and the shortcut.
fn row_widget(entry: &PaletteEntry, look: &RowLook) -> gtk::ListBoxRow {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    if let Some(glyph_look) = &look.glyph {
        let glyph = StatusGlyph::new();
        glyph.apply(Some(glyph_look));
        content.append(glyph.widget());
    }
    content.append(&text_column(entry, look.enabled));
    if let Some(badge) = &entry.badge {
        let label = gtk::Label::new(Some(badge));
        label.add_css_class(modal::BADGE_CLASS);
        label.add_css_class("caption");
        label.set_valign(gtk::Align::Center);
        content.append(&label);
    }
    if let Some(shortcut) = &look.shortcut {
        let label = gtk::Label::new(Some(shortcut));
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        content.append(&label);
    }
    let row = gtk::ListBoxRow::builder().child(&content).focusable(false).build();
    row.update_property(&[gtk::accessible::Property::Label(&entry.title)]);
    row
}

/// The title over its subtitle, the title dimmed while the row is inert.
fn text_column(entry: &PaletteEntry, enabled: bool) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 1);
    column.set_hexpand(true);
    let title = gtk::Label::builder()
        .label(&entry.title)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    if !enabled {
        title.add_css_class("dim-label");
    }
    column.append(&title);
    if let Some(subtitle) = &entry.subtitle {
        let detail = gtk::Label::builder()
            .label(subtitle)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        detail.add_css_class("dim-label");
        detail.add_css_class("caption");
        column.append(&detail);
    }
    column
}

/// One open palette.
struct Palette {
    generation: u64,
    frame: ModalFrame,
    entry: gtk::SearchEntry,
    state: Rc<RefCell<PaletteState>>,
}

impl Palette {
    fn new(view: PaletteView, inset: f64, post: impl Fn(Request) + 'static) -> Self {
        let entry = gtk::SearchEntry::builder()
            .placeholder_text(view.mode.placeholder())
            .hexpand(true)
            .build();
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
        let panel = crate::picker::panel(&entry, &scroller);
        let scrim = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        scrim.add_css_class(modal::DIM_CLASS);
        let frame = ModalFrame::new(&scrim, &panel, PanelRule::Palette { inset });
        let generation = view.generation;
        let initial = view.initial.clone();
        let state = Rc::new(RefCell::new(PaletteState {
            view,
            shown: Vec::new(),
            selection: 0,
            query: String::new(),
            list: list.clone(),
            scroller,
            post: Box::new(post),
        }));
        {
            let mut state = state.borrow_mut();
            state.shown = filter_entries(&state.view.entries, "", state.view.mode);
            state.render();
            let first = reselect(&state.view.entries, &state.shown, initial.as_deref(), 0);
            state.select(first, false);
        }
        connect_palette(&entry, &list, &scrim, &state);
        let weak = Rc::downgrade(&state);
        state.borrow().scroller.vadjustment().connect_changed(move |_| {
            if let Some(state) = weak.upgrade()
                && let Ok(state) = state.try_borrow()
            {
                state.reveal_selection();
            }
        });
        Self {
            generation,
            frame,
            entry,
            state,
        }
    }

    fn grab_focus(&self) -> bool {
        let took = self.entry.grab_focus();
        if let Ok(mut state) = self.state.try_borrow_mut() {
            let selection = state.selection;
            state.select(selection, false);
        }
        took
    }
}

/// Typing filters, the keys move, close or reach a launcher, Return or a click runs, a click on the scrim closes.
fn connect_palette(entry: &gtk::SearchEntry, list: &gtk::ListBox, scrim: &gtk::Box, state: &Rc<RefCell<PaletteState>>) {
    let weak = Rc::downgrade(state);
    entry.connect_changed(move |entry| {
        if let Some(state) = weak.upgrade() {
            state.borrow_mut().refilter(&entry.text());
        }
    });
    let weak = Rc::downgrade(state);
    entry.connect_activate(move |_| {
        if let Some(state) = weak.upgrade() {
            let state = state.borrow();
            state.run(state.selection);
        }
    });
    entry.add_controller(key_controller(state));
    let weak = Rc::downgrade(state);
    list.connect_row_activated(move |_, row| {
        if let (Some(state), Ok(index)) = (weak.upgrade(), usize::try_from(row.index())) {
            state.borrow().run(index);
        }
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

/// The field's keys: the picker's moves and Escape, then the launchers' chords.
fn key_controller(state: &Rc<RefCell<PaletteState>>) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(state);
    keys.connect_key_pressed(move |controller, keyval, keycode, modifiers| {
        let Some(state) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if let Some(key) = press_key(controller, keyval, keycode, modifiers) {
            state.borrow_mut().key(key);
            return glib::Propagation::Stop;
        }
        match launcher(controller, keyval, keycode, modifiers) {
            Some(action) => {
                (state.borrow().post)(Request::Launcher(action));
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    });
    keys
}

/// The palette launcher a press spells by its menu chord.
fn launcher(
    controller: &gtk::EventControllerKey,
    keyval: gdk::Key,
    keycode: u32,
    state: gdk::ModifierType,
) -> Option<BuiltinAction> {
    crate::keymap_dispatch::menu_action(controller, keyval, keycode, state).filter(|action| LAUNCHERS.contains(action))
}

/// A window's palette layer: a permanent overlay child over the window's content, holding the palette while the window
/// draws one.
pub struct WindowPalette {
    layer: gtk::Box,
    current: Option<Palette>,
}

impl WindowPalette {
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

    /// Gives the palette's search field the keyboard; whether one took it.
    pub fn grab_focus(&self) -> bool {
        self.current.as_ref().is_some_and(Palette::grab_focus)
    }

    /// Brings the layer in line with `view`, the palette window `window` draws, its panel centered over the terminal
    /// area `inset` points from the left; whether a new palette came up, which the caller gives the keyboard.
    pub fn sync(&mut self, view: Option<PaletteView>, inset: f64, window: Uuid, poster: &Poster) -> bool {
        let fresh = view.as_ref().is_some_and(|view| {
            self.current
                .as_ref()
                .is_none_or(|current| current.generation != view.generation)
        });
        if (view.is_none() || fresh)
            && let Some(current) = self.current.take()
        {
            self.layer.remove(&current.frame);
        }
        let Some(view) = view else {
            self.layer.set_visible(false);
            return false;
        };
        if fresh {
            let generation = view.generation;
            let palette = Palette::new(view, inset, requester(window, generation, poster));
            self.layer.append(&palette.frame);
            self.current = Some(palette);
        } else if let Some(current) = &self.current {
            let changed = current.state.borrow().view != view;
            if changed {
                current.state.borrow_mut().refresh(view);
            }
        }
        if let Some(current) = &self.current {
            current.frame.set_rule(PanelRule::Palette { inset });
        }
        self.layer.set_visible(true);
        fresh
    }
}

impl Default for WindowPalette {
    fn default() -> Self {
        Self::new()
    }
}

/// Posts the requests of the palette of generation `generation` in window `window` to the app, a theme preview once
/// the selection rested for [`PREVIEW_DEBOUNCE`] (a newer one replacing a pending one).
fn requester(window: Uuid, generation: u64, poster: &Poster) -> impl Fn(Request) + 'static {
    let poster = poster.clone();
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    move |request| {
        let preview = matches!(request, Request::Preview(_));
        let poster = poster.clone();
        let send = move || {
            state::post(&poster, move |state: &mut AppState| {
                state.palette_request(window, generation, request);
            });
        };
        if let Some(timer) = pending.borrow_mut().take() {
            timer.remove();
        }
        if !preview {
            send();
            return;
        }
        let slot = Rc::downgrade(&pending);
        let timer = glib::timeout_add_local_once(PREVIEW_DEBOUNCE, move || {
            if let Some(slot) = slot.upgrade() {
                slot.borrow_mut().take();
            }
            send();
        });
        *pending.borrow_mut() = Some(timer);
    }
}

// MARK: - the app's side

impl AppState {
    /// A request of the palette of generation `generation` window `window` draws: run a row, close it, preview a
    /// theme, or a launcher's chord typed in its field. A palette closed or rebuilt since asks nothing.
    fn palette_request(&mut self, window: Uuid, generation: u64, request: Request) {
        if self.core.palette_mode().is_none() || self.core.palette_generation() != generation {
            return;
        }
        match request {
            Request::Run(action) => {
                let effect = self.core.run_palette_entry(window, &action);
                // settled first, so the terminal has the keyboard back before a rename field takes it
                self.settle();
                if let Some(effect) = effect {
                    self.apply_action_effect(effect);
                }
            }
            Request::Close => {
                self.core.close_palette();
                self.settle();
            }
            Request::Launcher(action) => {
                self.keymap_fired(KeyOrigin::Window(window), Fired::Builtin(action), None);
            }
            Request::Preview(name) => {
                self.core.preview_palette_theme(name.as_deref());
                self.settle();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use agterm_core::palette_catalog::PaletteCommand;

    use super::*;

    fn entry(id: &str, action: PaletteAction) -> PaletteEntry {
        PaletteEntry {
            id: id.to_owned(),
            title: id.to_owned(),
            subtitle: None,
            shortcut: None,
            badge: None,
            indicator: None,
            action,
        }
    }

    fn look(enabled: bool) -> RowLook {
        RowLook {
            enabled,
            shortcut: None,
            glyph: None,
        }
    }

    fn view(mode: PaletteMode, generation: u64, entries: Vec<PaletteEntry>, enabled: &[bool]) -> PaletteView {
        PaletteView {
            mode,
            generation,
            looks: enabled.iter().map(|enabled| look(*enabled)).collect(),
            entries,
            initial: None,
        }
    }

    #[test]
    fn the_selection_stays_on_its_row_when_the_rows_change() {
        let entries = [
            entry("a", PaletteAction::Theme(None)),
            entry("b", PaletteAction::Theme(None)),
            entry("c", PaletteAction::Theme(None)),
        ];
        assert_eq!(reselect(&entries, &[0, 1, 2], Some("c"), 0), 2);
        assert_eq!(reselect(&entries, &[2, 0], Some("a"), 0), 1);
        assert_eq!(reselect(&entries, &[0, 1], Some("gone"), 5), 1, "clamped");
        assert_eq!(reselect(&entries, &[], None, 3), 0);
    }

    #[test]
    fn shortcuts_read_as_gtk_labels_and_custom_text() {
        crate::gtk_test::on_gtk_thread(|| {
            let chord = agterm_core::keymap::parse_keybind("ctrl+shift+p").unwrap().remove(0);
            let label = shortcut_label(&PaletteShortcut::Chord(chord)).unwrap();
            assert!(label.contains("Ctrl") && label.contains('P'), "{label}");
            assert_eq!(
                shortcut_label(&PaletteShortcut::Text("ctrl+a>g".to_owned())).as_deref(),
                Some("ctrl+a>g")
            );
        });
    }

    /// Runs the GTK main context until `done` holds or a few seconds pass; whether it held.
    fn pump_until(mut done: impl FnMut() -> bool) -> bool {
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            while context.iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    /// Whether the palette's selected row lies inside its scroller's visible band.
    fn selection_in_view(palette: &Palette) -> bool {
        let state = palette.state.borrow();
        let Some(row) = i32::try_from(state.selection)
            .ok()
            .and_then(|index| state.list.row_at_index(index))
        else {
            return false;
        };
        let Some(bounds) = row.compute_bounds(&state.list) else {
            return false;
        };
        let adjustment = state.scroller.vadjustment();
        let (top, bottom) = (f64::from(bounds.y()), f64::from(bounds.y() + bounds.height()));
        row.height() > 0
            && top >= adjustment.value() - 0.5
            && bottom <= adjustment.value() + adjustment.page_size() + 0.5
    }

    #[test]
    fn the_selection_scrolls_into_view_on_open_and_on_a_move() {
        crate::gtk_test::on_gtk_thread(selection_scrolls_into_view);
    }

    fn selection_scrolls_into_view() {
        let themes: Vec<PaletteEntry> = (0..40)
            .map(|index| {
                entry(
                    &format!("theme:{index:02}"),
                    PaletteAction::Theme(Some(index.to_string())),
                )
            })
            .collect();
        let mut opened = view(PaletteMode::Themes, 1, themes, &[true; 40]);
        opened.initial = Some("theme:20".to_owned());
        let palette = Palette::new(opened, 0.0, |_| {});
        let window = gtk::Window::builder().default_width(960).default_height(640).build();
        window.set_child(Some(&palette.frame));
        window.present();
        palette.grab_focus();
        assert!(pump_until(|| selection_in_view(&palette)), "the opening row shows");
        for _ in 0..8 {
            palette.state.borrow_mut().key(PickKey::Move(1));
        }
        assert!(pump_until(|| selection_in_view(&palette)), "a moved selection shows");
        palette.state.borrow_mut().key(PickKey::Move(-25));
        assert!(pump_until(|| selection_in_view(&palette)), "back up too");
        window.destroy();
    }

    #[test]
    fn the_palette_filters_moves_runs_live_rows_and_previews_themes() {
        crate::gtk_test::on_gtk_thread(palette_filters_moves_and_runs);
    }

    fn palette_filters_moves_and_runs() {
        let requests: Rc<RefCell<Vec<Request>>> = Rc::default();
        let seen = Rc::clone(&requests);
        let entries = vec![
            entry("New Session", PaletteAction::Command(PaletteCommand::NewSession)),
            entry("Swap Panes", PaletteAction::Command(PaletteCommand::SwapPanes)),
            entry("Close Session", PaletteAction::Command(PaletteCommand::CloseSession)),
        ];
        let palette = Palette::new(
            view(PaletteMode::Actions, 1, entries, &[true, false, true]),
            0.0,
            move |request| seen.borrow_mut().push(request),
        );
        let titles = |palette: &Palette| {
            let state = palette.state.borrow();
            state
                .shown
                .iter()
                .map(|index| state.view.entries[*index].title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(&palette), ["Close Session", "New Session", "Swap Panes"], "A→Z");
        palette.state.borrow_mut().refilter("swap");
        assert_eq!(titles(&palette), ["Swap Panes"]);
        palette.entry.emit_activate();
        assert!(requests.borrow().is_empty(), "an inert row does not run");
        palette.state.borrow_mut().refilter("");
        palette.state.borrow_mut().key(PickKey::Move(1));
        palette.entry.emit_activate();
        palette.state.borrow_mut().key(PickKey::Cancel);
        assert_eq!(
            *requests.borrow(),
            [
                Request::Run(PaletteAction::Command(PaletteCommand::NewSession)),
                Request::Close
            ]
        );

        let themes = vec![
            entry("theme:A", PaletteAction::Theme(Some("A".to_owned()))),
            entry("theme:B", PaletteAction::Theme(Some("B".to_owned()))),
        ];
        let seen = Rc::clone(&requests);
        let mut opened = view(PaletteMode::Themes, 2, themes, &[true, true]);
        opened.initial = Some("theme:B".to_owned());
        requests.borrow_mut().clear();
        let palette = Palette::new(opened, 0.0, move |request| seen.borrow_mut().push(request));
        assert_eq!(palette.state.borrow().selection, 1, "opens on the theme on screen");
        assert!(requests.borrow().is_empty(), "opening previews nothing");
        palette.state.borrow_mut().key(PickKey::Move(-1));
        assert_eq!(*requests.borrow(), [Request::Preview(Some("A".to_owned()))]);
    }
}
