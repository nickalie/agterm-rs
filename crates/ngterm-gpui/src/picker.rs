//! The native picker (`pick.open`), after upstream's `Views/Palette.swift` (`CommandPalette` with caller-supplied
//! items) as `WindowContentView.swift`'s `pickPaletteOverlay` drives it; port of the GTK build's `picker.rs`.
//!
//! One per window, shown while the window's modal slot holds a pick: a scrim over the whole window (`pick-scrim`, a
//! press on it cancels) under a palette panel near the top (`pick-palette`), centered over the terminal area, with a
//! search field (the caller's prompt as its placeholder, its query prefilled) over the rows (a label and an optional
//! subtitle). Typing filters and ranks the rows by their labels ([`filter_rows`]); an empty query keeps the caller's
//! order. The selection opens on the caller's `--select` row when the first filtered list holds it, else the first row,
//! and goes back to the first row on every edit. Up and Down (or Ctrl+J and Ctrl+K) move it, Return or a click picks a
//! row, Escape cancels. With `--allow-custom`, a non-empty query that matches no row offers one row, `Use "<query>"`,
//! which answers with the typed text. The answer reports the row's index in the caller's order, whatever the filter
//! showed.
//!
//! The rows are a pure function of the items and the query, testable without a display. A new pick id builds a fresh
//! picker; the picker goes when the slot empties, and the deck gives the terminal the keyboard back. The field's keys
//! reach the picker ahead of the field's own bindings through the window's key hook ([`PickKey`], read off the
//! key monitor's [`KeyStroke`]). The panel's frame, field and rows ([`frame`], [`header`], [`row`]) are the palettes'
//! too ([`crate::palette`]).

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, Entity, FocusHandle, Focusable as _, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels, Render, ScrollHandle, SharedString, Size,
    Stateful, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use ngterm_core::chrome::ChromeColors;
use ngterm_core::fuzzy::fuzzy_rank;
use ngterm_core::ghostty_config::binds::Mods;
use ngterm_core::modal::PendingPick;
use ngterm_core::palette::{palette_search_keys, pick_custom_row_label};
use ngterm_proto::{ControlPickOutcome, ControlPickResult, PickItem};

use crate::modal::{self, Metrics};
use crate::terminal::input::{KeyStroke, chord, chord_key};

/// The search field's placeholder without a caller's prompt (upstream's picker default).
const DEFAULT_PROMPT: &str = "Select…";
/// The panel's width at the default interface size (upstream's `panelWidthAtDefaultFontSize`).
pub const PANEL_WIDTH: f64 = 520.0;
/// The rows' height at the default interface size (upstream's `resultsHeightAtDefaultFontSize`).
pub const RESULTS_HEIGHT: f64 = 320.0;

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

/// What a key does in a list panel's field, beyond editing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKey {
    /// Moves the selection.
    Move(isize),
    /// Runs the selected row (the field's activation).
    Run,
    /// Cancels the panel.
    Cancel,
    /// Kept from moving focus out of the panel.
    Swallow,
}

/// The field's key handling (upstream's `onKeyPress` handlers): Up and Down move (the keypad's too, its 8 and 2 with
/// `NumLock` off, as the GTK build's `KP_Up` and `KP_Down`), Ctrl+J and Ctrl+K move (with exactly Control held, the key
/// resolved by [`selection_step`]), Return runs, Escape cancels, and Tab stays in the panel.
#[must_use]
pub fn pick_key(stroke: &KeyStroke) -> Option<PickKey> {
    let keypad_arrows = !stroke.mods.contains(Mods::NUM_LOCK);
    match stroke.key.name() {
        "arrow_up" => return Some(PickKey::Move(-1)),
        "arrow_down" => return Some(PickKey::Move(1)),
        "numpad_8" if keypad_arrows => return Some(PickKey::Move(-1)),
        "numpad_2" if keypad_arrows => return Some(PickKey::Move(1)),
        "enter" | "numpad_enter" => return Some(PickKey::Run),
        "escape" => return Some(PickKey::Cancel),
        "tab" => return Some(PickKey::Swallow),
        _ => {}
    }
    let chord = chord(stroke)?;
    if chord.mods != ngterm_core::keymap::Modifiers::CONTROL {
        return None;
    }
    selection_step(stroke.keycode, stroke.unshifted, stroke.ascii_layout).map(PickKey::Move)
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

// MARK: - the panel both list panels draw

/// Where a list panel sits in the window: near the top, centered over the terminal area `inset` from the left, `ideal`
/// wide at the default interface size (upstream's `fittedPanelWidth`, `panelOffset`, `fittedPanelHeight`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    /// The tallest the panel grows.
    pub cap: f64,
}

impl Placement {
    /// The placement in a window `viewport` large.
    #[must_use]
    pub fn of(viewport: Size<Pixels>, inset: f64, ideal: f64, metrics: Metrics) -> Self {
        let (width, height) = (
            f64::from(f32::from(viewport.width)),
            f64::from(f32::from(viewport.height)),
        );
        let panel = modal::fitted_width(metrics.scaled(ideal), width, inset);
        Self {
            left: (width - panel) / 2.0 + modal::palette_offset(panel, width, inset),
            top: modal::palette_top(height),
            width: panel,
            cap: modal::palette_cap(height),
        }
    }
}

/// The ids a list panel answers to (upstream's accessibility ids).
#[derive(Debug, Clone, Copy)]
pub struct Ids {
    pub scrim: &'static str,
    pub panel: &'static str,
}

/// A glyph of gpui-kit's catalog, `size` square in `ink`.
#[must_use]
pub fn glyph(name: IconName, ink: gpui_kit::Hsla, size: Pixels) -> AnyElement {
    gpui_kit::svg()
        .path(name.path())
        .size(size)
        .flex_shrink_0()
        .text_color(ink)
        .into_any_element()
}

/// The search field's row: the magnifier and the field, at the interface size.
#[must_use]
pub fn header(field: &Entity<InputState>, metrics: Metrics, colors: ChromeColors) -> AnyElement {
    let dim = crate::chrome::color(colors.foreground, crate::chrome::DIM_ALPHA);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .p(modal::logical(metrics.scaled(12.0)))
        .child(glyph(IconName::Search, dim, modal::logical(metrics.base)))
        .child(
            div().flex_1().min_w(px(0.)).child(
                Input::new(field)
                    .id("palette-field")
                    .appearance(false)
                    .text_size(modal::logical(metrics.base)),
            ),
        )
        .into_any_element()
}

/// One row's frame: the selection's accent wash, a hover wash on a live row, the interface size's padding.
#[must_use]
pub fn row(id: SharedString, selected: bool, live: bool, metrics: Metrics, colors: ChromeColors) -> Stateful<Div> {
    let hover = crate::chrome::color(colors.foreground, 0.04);
    div()
        .id(id.clone())
        .accessibility_id(id.clone())
        .role(gpui_kit::Role::ListBoxOption)
        .aria_selected(selected)
        .debug_selector(move || id.to_string())
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px(modal::logical(metrics.scaled(12.0)))
        .py(modal::logical(metrics.scaled(6.0)))
        .when(selected, |row| row.bg(crate::chrome::color(modal::ACCENT, 0.25)))
        .when(live && !selected, |row| row.hover(move |style| style.bg(hover)))
        .when(live, gpui_kit::Styled::cursor_pointer)
}

/// A row's text column: the title (dimmed while `enabled` is not) over its optional subtitle.
#[must_use]
pub fn text_column(
    title: &str,
    subtitle: Option<&str>,
    enabled: bool,
    metrics: Metrics,
    colors: ChromeColors,
) -> AnyElement {
    let ink = crate::chrome::color(colors.foreground, if enabled { 1.0 } else { 0.45 });
    let dim = crate::chrome::color(colors.foreground, crate::chrome::DIM_ALPHA);
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.))
        .gap(px(1.))
        .child(
            div()
                .truncate()
                .text_size(modal::logical(metrics.base))
                .text_color(ink)
                .child(SharedString::from(title.to_owned())),
        )
        .children(subtitle.map(|subtitle| {
            div()
                .id("palette-subtitle")
                .accessibility_id("palette-subtitle")
                .role(gpui_kit::Role::Label)
                .aria_label(SharedString::from(subtitle.to_owned()))
                .truncate()
                .text_size(modal::logical(metrics.secondary))
                .text_color(dim)
                .child(SharedString::from(subtitle.to_owned()))
        }))
        .into_any_element()
}

/// What a list panel's frame holds.
pub struct Frame<'a> {
    pub ids: Ids,
    pub placement: Placement,
    pub header: AnyElement,
    pub rows: Vec<AnyElement>,
    pub scroll: &'a ScrollHandle,
    pub metrics: Metrics,
    pub colors: ChromeColors,
}

/// The frame both list panels share: the scrim over the whole window (a press on it runs `dismiss`) under the panel
/// at its placement, the header over a divider over the scrolling rows, which grow to the results cap.
#[must_use]
pub fn frame(parts: Frame<'_>, dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    let Frame {
        ids,
        placement,
        header,
        rows,
        scroll,
        metrics,
        colors,
    } = parts;
    let separator = crate::chrome::separator(&colors);
    div()
        .id(ids.scrim)
        // upstream's scrim is a dismiss button over the window
        .accessibility_id(ids.scrim)
        .role(gpui_kit::Role::Button)
        .aria_label("Dismiss")
        .debug_selector(move || ids.scrim.to_owned())
        .absolute()
        .inset_0()
        .occlude()
        .bg(modal::dim())
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            dismiss(event, window, cx);
        })
        .child(
            modal::panel(colors)
                .id(ids.panel)
                .accessibility_id(ids.panel)
                .role(gpui_kit::Role::Dialog)
                .debug_selector(move || ids.panel.to_owned())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .absolute()
                .left(modal::logical(placement.left))
                .top(modal::logical(placement.top))
                .w(modal::logical(placement.width))
                .max_h(modal::logical(placement.cap))
                .flex()
                .flex_col()
                .child(header)
                .child(div().h(px(1.)).flex_shrink_0().bg(separator))
                .child(
                    div()
                        .id("palette-results")
                        .flex()
                        .flex_col()
                        .min_h(px(0.))
                        .max_h(modal::logical(metrics.scaled(RESULTS_HEIGHT)))
                        .overflow_y_scroll()
                        .track_scroll(scroll)
                        .children(rows),
                ),
        )
}

// MARK: - the view

/// What the picker answers with: the window posts it to the core.
pub type Resolve = Rc<dyn Fn(ControlPickResult)>;

/// One open picker (module docs).
pub struct Picker {
    pick: PendingPick,
    field: Entity<InputState>,
    rows: Vec<PickRow>,
    selection: usize,
    /// An answer went out: later keys and clicks do nothing.
    done: bool,
    resolve: Resolve,
    inset: f64,
    metrics: Metrics,
    scroll: ScrollHandle,
    _field_events: Subscription,
}

impl Picker {
    /// A picker for `pick`, over a terminal area `inset` from the left; `resolve` takes its one answer.
    pub fn new(
        pick: &PendingPick,
        inset: f64,
        metrics: Metrics,
        resolve: Resolve,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prompt = pick.prompt.clone().unwrap_or_else(|| DEFAULT_PROMPT.to_owned());
        let query = pick.query.clone().unwrap_or_default();
        let field = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(prompt)
                .default_value(query.clone())
        });
        let events = cx.subscribe(&field, |picker, field, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = field.read(cx).value().to_string();
                picker.refilter(&query);
                cx.notify();
            }
        });
        let rows = filter_rows(&pick.items, &query, pick.allow_custom);
        let selection = initial_selection(&rows, &pick.items, pick.selection.as_deref());
        let scroll = ScrollHandle::new();
        scroll.scroll_to_item(selection);
        Self {
            pick: pick.clone(),
            field,
            rows,
            selection,
            done: false,
            resolve,
            inset,
            metrics,
            scroll,
            _field_events: events,
        }
    }

    /// The pick shown.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.pick.id
    }

    /// The rows shown.
    #[must_use]
    pub fn rows(&self) -> &[PickRow] {
        &self.rows
    }

    /// The selected row.
    #[must_use]
    pub fn selection(&self) -> usize {
        self.selection
    }

    /// What takes the keyboard: the search field.
    #[must_use]
    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.field.read(cx).focus_handle(cx)
    }

    /// The search field.
    #[must_use]
    pub fn field(&self) -> &Entity<InputState> {
        &self.field
    }

    /// Follows the terminal area and the interface size.
    pub fn place(&mut self, inset: f64, metrics: Metrics, cx: &mut Context<Self>) {
        if (self.inset, self.metrics) != (inset, metrics) {
            self.inset = inset;
            self.metrics = metrics;
            cx.notify();
        }
    }

    /// Shows the rows `query` filters, the selection on the first.
    fn refilter(&mut self, query: &str) {
        self.rows = filter_rows(&self.pick.items, query, self.pick.allow_custom);
        self.select(0);
    }

    fn select(&mut self, index: usize) {
        self.selection = moved(index, 0, self.rows.len());
        self.scroll.scroll_to_item(self.selection);
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

    /// A key of the field's (or a scrim press as [`PickKey::Cancel`]).
    pub fn key(&mut self, key: PickKey, cx: &mut Context<Self>) {
        match key {
            PickKey::Move(delta) => self.select(moved(self.selection, delta, self.rows.len())),
            PickKey::Run => self.run(self.selection),
            PickKey::Cancel => self.finish(ControlPickResult::outcome(ControlPickOutcome::Cancelled)),
            PickKey::Swallow => {}
        }
        cx.notify();
    }

    fn render_row(&self, index: usize, shown: &PickRow, colors: ChromeColors, cx: &mut Context<Self>) -> AnyElement {
        let (id, title, subtitle) = match shown {
            PickRow::Item(item) => {
                let item = &self.pick.items[*item];
                (item.id.as_str(), item.label.as_str(), item.subtitle.as_deref())
            }
            PickRow::Custom { label, .. } => ("pick-custom", label.as_str(), None),
        };
        row(
            SharedString::from(format!("palette-item-{id}")),
            index == self.selection,
            true,
            self.metrics,
            colors,
        )
        .child(text_column(title, subtitle, true, self.metrics, colors))
        .on_click(cx.listener(move |picker, _, _, cx| {
            picker.run(index);
            cx.notify();
        }))
        .into_any_element()
    }
}

impl Render for Picker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = crate::chrome::current(cx);
        let rows: Vec<AnyElement> = self
            .rows
            .clone()
            .iter()
            .enumerate()
            .map(|(index, row)| self.render_row(index, row, colors, cx))
            .collect();
        let placement = Placement::of(window.viewport_size(), self.inset, PANEL_WIDTH, self.metrics);
        let picker = cx.entity().downgrade();
        frame(
            Frame {
                ids: Ids {
                    scrim: "pick-scrim",
                    panel: "pick-palette",
                },
                placement,
                header: header(&self.field, self.metrics, colors),
                rows,
                scroll: &self.scroll,
                metrics: self.metrics,
                colors,
            },
            move |_, _, cx| {
                if let Some(picker) = picker.upgrade() {
                    picker.update(cx, |picker, cx| picker.key(PickKey::Cancel, cx));
                }
            },
        )
    }
}

#[cfg(test)]
mod tests;
