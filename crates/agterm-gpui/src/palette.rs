//! The command palettes (upstream's `Views/Palette.swift`, `CommandPalette` over the built-in feeds; port of the GTK
//! build's `palette.rs`), drawn in the frontmost window while [`agterm_core::palette`] holds one open.
//!
//! The same frame as the native picker ([`crate::picker`]): a scrim over the window (`palette-scrim`, a press on it
//! closes the palette) under a panel near the top (`command-palette`), centered over the terminal area, with a search
//! field over the rows. A row (`palette-item-<id>`) shows its title (dimmed while it cannot run) over an optional
//! subtitle (`palette-subtitle`), a leading status glyph on the attention palette, a trailing badge (`palette-badge`:
//! `custom`, `current`) and its shortcut: a built-in's live chord as GTK labels accelerators, a custom command's as the
//! user wrote it. Typing ranks the rows by title and subtitle ([`filter_entries`]): a blank query lists them A→Z, the
//! attention palette in its own order. Up and Down (or Ctrl+J and Ctrl+K) move the selection, Return or a click runs
//! the row when it can run, Escape closes; a palette launcher's chord still reaches its action from the field, so the
//! same chord closes or switches the palette. Text and lengths follow the Interface font size ([`Metrics`]).
//!
//! The theme palette opens on the theme on screen and previews the selected row live, debounced (upstream's 70 ms
//! preview debounce); its run commits the theme.
//!
//! The core decides when a palette shows and with which rows; every sync hands the window a fresh [`PaletteView`],
//! which a new generation builds afresh (an empty query) and an unchanged one only refreshes. What the user does is
//! posted to [`AppState`] with the generation it was built for, so a stale palette's request does nothing.

use std::rc::Rc;
use std::time::Duration;

use agterm_core::Core;
use agterm_core::app_actions::ActionEffect;
use agterm_core::keymap::BuiltinAction;
use agterm_core::palette::{PaletteAction, PaletteEntry, PaletteMode, PaletteShortcut, filter_entries};
use agterm_core::palette_catalog::PaletteCommand;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, px,
};
use uuid::Uuid;

use crate::keymap_dispatch::{Fired, KeyOrigin};
use crate::modal::{self, Metrics};
use crate::picker::{self, Frame, Ids, PickKey, Placement, moved};
use crate::state::{AppState, Poster, post};
use crate::status_glyph::GlyphLook;

/// How long the theme palette waits for the selection to settle before previewing it (upstream's
/// `previewThemeDebounceInterval`).
pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(70);
/// The launchers whose chord reaches its action from the search field.
pub const LAUNCHERS: [BuiltinAction; 5] = [
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
                shortcut: entry.shortcut.as_ref().map(shortcut_label),
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

/// A shortcut as the row shows it: a chord as GTK labels an accelerator (`Shift+Ctrl+T`), a custom command's as
/// written.
#[must_use]
pub fn shortcut_label(shortcut: &PaletteShortcut) -> String {
    match shortcut {
        PaletteShortcut::Text(text) => text.clone(),
        PaletteShortcut::Chord(chord) => crate::titlebar::chord_label(chord),
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
pub enum Request {
    Run(PaletteAction),
    Close,
    Preview(Option<String>),
    Launcher(BuiltinAction),
}

/// Where the palette's requests go: the app, with the window and generation it was built for (tests collect them).
pub type Sink = Rc<dyn Fn(Request)>;

/// The sink posting window `window`'s palette of generation `generation` to the app.
#[must_use]
pub fn sink(window: Uuid, generation: u64, poster: &Poster) -> Sink {
    let poster = poster.clone();
    Rc::new(move |request| {
        post(&poster, move |state: &mut AppState| {
            state.palette_request(window, generation, request);
        });
    })
}

/// One open palette (module docs).
pub struct Palette {
    view: PaletteView,
    /// The rows shown, as indices into the view's entries.
    shown: Vec<usize>,
    selection: usize,
    query: String,
    field: Entity<InputState>,
    sink: Sink,
    /// The theme preview waiting for the selection to rest.
    preview: Option<Task<()>>,
    inset: f64,
    metrics: Metrics,
    scroll: ScrollHandle,
    _field_events: Subscription,
}

impl Palette {
    /// A palette showing `view` over a terminal area `inset` from the left, its requests going to `sink`.
    pub fn new(
        view: PaletteView,
        inset: f64,
        metrics: Metrics,
        sink: Sink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = view.mode.placeholder();
        let field = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let events = cx.subscribe(&field, |palette, field, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = field.read(cx).value().to_string();
                palette.refilter(&query, cx);
                cx.notify();
            }
        });
        let shown = filter_entries(&view.entries, "", view.mode);
        let selection = reselect(&view.entries, &shown, view.initial.as_deref(), 0);
        let scroll = ScrollHandle::new();
        scroll.scroll_to_item(selection);
        Self {
            view,
            shown,
            selection,
            query: String::new(),
            field,
            sink,
            preview: None,
            inset,
            metrics,
            scroll,
            _field_events: events,
        }
    }

    /// The generation shown.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.view.generation
    }

    /// The titles of the rows shown, in order (tests).
    #[must_use]
    pub fn titles(&self) -> Vec<String> {
        self.shown
            .iter()
            .filter_map(|index| self.view.entries.get(*index))
            .map(|entry| entry.title.clone())
            .collect()
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

    fn entry_at(&self, row: usize) -> Option<(&PaletteEntry, &RowLook)> {
        let index = *self.shown.get(row)?;
        Some((self.view.entries.get(index)?, self.view.looks.get(index)?))
    }

    /// The rows changed under an unchanged palette: the query stays, the selection stays on its row where it can; and
    /// it follows the terminal area and the interface size.
    pub fn refresh(&mut self, view: PaletteView, inset: f64, metrics: Metrics, cx: &mut Context<Self>) {
        if (self.inset, self.metrics) != (inset, metrics) {
            self.inset = inset;
            self.metrics = metrics;
            cx.notify();
        }
        if self.view == view {
            return;
        }
        let selected = self.entry_at(self.selection).map(|(entry, _)| entry.id.clone());
        self.view = view;
        self.shown = filter_entries(&self.view.entries, &self.query, self.view.mode);
        let index = reselect(&self.view.entries, &self.shown, selected.as_deref(), self.selection);
        self.select(index, false, cx);
        cx.notify();
    }

    /// Shows the rows `query` ranks, the selection on the first, previewing it (upstream's query `onChange`).
    fn refilter(&mut self, query: &str, cx: &mut Context<Self>) {
        query.clone_into(&mut self.query);
        self.shown = filter_entries(&self.view.entries, query, self.view.mode);
        self.select(0, true, cx);
    }

    /// Selects row `index`, previewing a theme row when `preview` (upstream's `onSelect` on a selection change).
    fn select(&mut self, index: usize, preview: bool, cx: &mut Context<Self>) {
        self.selection = moved(index, 0, self.shown.len());
        self.scroll.scroll_to_item(self.selection);
        if preview && self.view.mode == PaletteMode::Themes {
            let theme = self.entry_at(self.selection).map(|(entry, _)| entry.action.clone());
            if let Some(PaletteAction::Theme(name)) = theme {
                self.request(Request::Preview(name), cx);
            }
        }
    }

    /// Sends `request`, a theme preview once the selection rested for [`PREVIEW_DEBOUNCE`]; any request drops a
    /// preview still waiting.
    fn request(&mut self, request: Request, cx: &mut Context<Self>) {
        self.preview = None;
        if !matches!(request, Request::Preview(_)) {
            (self.sink)(request);
            return;
        }
        let sink = Rc::clone(&self.sink);
        self.preview = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(PREVIEW_DEBOUNCE).await;
            sink(request);
        }));
    }

    /// Runs row `row` when it can run now; an inert row does nothing and the palette stays.
    fn run(&mut self, row: usize, cx: &mut Context<Self>) {
        if let Some((entry, look)) = self.entry_at(row)
            && look.enabled
        {
            let action = entry.action.clone();
            self.request(Request::Run(action), cx);
        }
    }

    /// A key of the field's (or a scrim press as [`PickKey::Cancel`]).
    pub fn key(&mut self, key: PickKey, cx: &mut Context<Self>) {
        match key {
            PickKey::Move(delta) => {
                let index = moved(self.selection, delta, self.shown.len());
                self.select(index, true, cx);
            }
            PickKey::Run => self.run(self.selection, cx),
            PickKey::Cancel => self.request(Request::Close, cx),
            PickKey::Swallow => {}
        }
        cx.notify();
    }

    /// A palette launcher's chord typed in the field.
    pub fn launcher(&mut self, action: BuiltinAction, cx: &mut Context<Self>) {
        self.request(Request::Launcher(action), cx);
    }

    fn render_row(&self, row: usize, animate: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (entry, look) = self.entry_at(row)?;
        let (metrics, colors) = (self.metrics, crate::chrome::current(cx));
        let dim = crate::chrome::color(colors.foreground, crate::chrome::DIM_ALPHA);
        let glyph = look
            .glyph
            .as_ref()
            .map(|glyph| crate::status_glyph::glyph(glyph, animate).into_any_element());
        let badge = entry.badge.as_ref().map(|badge| {
            div()
                .id("palette-badge")
                .flex_shrink_0()
                .px(px(6.))
                .py(px(2.))
                .rounded_full()
                .bg(crate::chrome::color(colors.foreground, 0.2))
                .text_size(modal::logical(metrics.secondary))
                .text_color(dim)
                .child(SharedString::from(badge.clone()))
        });
        let shortcut = look.shortcut.as_ref().map(|shortcut| {
            div()
                .flex_shrink_0()
                .text_size(modal::logical(metrics.shortcut))
                .text_color(dim)
                .child(SharedString::from(shortcut.clone()))
        });
        let element = picker::row(
            SharedString::from(format!("palette-item-{}", entry.id)),
            row == self.selection,
            look.enabled,
            metrics,
            colors,
        )
        .children(glyph)
        .child(picker::text_column(
            &entry.title,
            entry.subtitle.as_deref(),
            look.enabled,
            metrics,
            colors,
        ))
        .children(badge)
        .children(shortcut)
        .on_click(cx.listener(move |palette, _, _, cx| {
            palette.run(row, cx);
            cx.notify();
        }));
        Some(element.into_any_element())
    }
}

impl Render for Palette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = crate::chrome::current(cx);
        let animate = crate::status_glyph::animations_enabled(cx);
        let rows: Vec<AnyElement> = (0..self.shown.len())
            .filter_map(|row| self.render_row(row, animate, cx))
            .collect();
        let placement = Placement::of(window.viewport_size(), self.inset, picker::PANEL_WIDTH, self.metrics);
        let palette = cx.entity().downgrade();
        picker::frame(
            Frame {
                ids: Ids {
                    scrim: "palette-scrim",
                    panel: "command-palette",
                },
                placement,
                header: picker::header(&self.field, self.metrics, colors),
                rows,
                scroll: &self.scroll,
                metrics: self.metrics,
                colors,
            },
            move |_, _, cx| {
                if let Some(palette) = palette.upgrade() {
                    palette.update(cx, |palette, cx| palette.key(PickKey::Cancel, cx));
                }
            },
        )
    }
}

// MARK: - the app's side

impl AppState {
    /// A request of the palette of generation `generation` window `window` draws: run a row, close it, preview a
    /// theme, or a launcher's chord typed in its field. A palette closed or rebuilt since asks nothing.
    pub fn palette_request(&mut self, window: Uuid, generation: u64, request: Request) {
        if self.is_quitting() || self.core.palette_mode().is_none() || self.core.palette_generation() != generation {
            return;
        }
        match request {
            Request::Run(PaletteAction::Command(PaletteCommand::ClearFlagged)) => {
                // upstream's `clearFlags` confirms first: the palette closes, then the window asks
                self.core.close_palette();
                self.settle();
                self.clear_flags_requested(window);
            }
            Request::Run(action) => {
                let effect = self.core.run_palette_entry(window, &action);
                // the frontend half lands after the windows' sync, so the terminal has the keyboard back before a
                // rename field takes it
                if let Some(ActionEffect::Frontend(action)) = effect {
                    self.frontend_action(action);
                }
                self.settle();
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
mod tests;
