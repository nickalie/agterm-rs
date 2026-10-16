//! The scrollback search bar (upstream's `Views/TerminalSearchBar.swift`, as `WindowContentView.searchBarLayer` hosts
//! it; port of the GTK build's `search.rs`): a compact panel at the top right of the terminal area, shown while the
//! active session's search is open and no zoom fills the window.
//!
//! The magnifier, a search field (`search-field`), the match counter (`search-counter`: `2 of 7`, `no matches`), and
//! previous, next and close buttons. Typing sends the needle to the pane the search is pinned to; Return steps to the
//! next match, Shift+Return to the previous one, Escape, Find's chord or the close button ends the search, whose end
//! report gives the terminal the keyboard back ([`agterm_core::Core::search_bar_close`]). The bar takes the keyboard
//! when it comes up. The field's keys reach the bar ahead of the field's own bindings through the window's key hook.
//!
//! The field is the needle's writer while the user types: the model's needle is pushed into it only once every needle
//! the field sent has landed, so a settle in between never rewinds what is being typed; a needle set elsewhere (a
//! `session.search --text`, the seed of a new search) shows once the field is idle.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agterm_core::Core;
use agterm_core::ghostty_config::binds::Mods;
use agterm_core::model::{AppStore, SearchState, StatusPane};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState, Position};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px,
};
use uuid::Uuid;

use crate::state::{AppState, Poster, post};
use crate::terminal::input::KeyStroke;

/// The field's width (upstream's 150-point field).
const FIELD_WIDTH: f32 = 150.;
/// The bar's clearance from the terminal area's top right corner.
pub const MARGIN: f32 = 8.;

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

/// The search a bar shows: its session and the pane the search is pinned to.
pub type SearchKey = (Uuid, Option<StatusPane>);

/// The key of the search the bar of window `window` shows now ([`shown_search`]).
#[must_use]
pub fn shown_key(core: &Core, window: Uuid) -> Option<SearchKey> {
    shown_search(core, window).map(|(session, search)| (session, search.owner))
}

/// What a key in the field does beyond editing it: Return steps forward, Shift+Return back, Escape closes.
#[must_use]
pub fn field_key(stroke: &KeyStroke) -> Option<SearchInput> {
    match stroke.key.name() {
        "enter" | "numpad_enter" => Some(SearchInput::Step(!stroke.mods.contains(Mods::SHIFT))),
        "escape" => Some(SearchInput::Close),
        _ => None,
    }
}

/// One window's search bar (module docs).
pub struct SearchBar {
    window: Uuid,
    poster: Poster,
    field: Entity<InputState>,
    counter: String,
    /// The needles the field sent, and how many of them landed.
    sent: u64,
    landed: Arc<AtomicU64>,
    /// The search the bar shows, which its input is meant for.
    shown: Option<SearchKey>,
    _field_events: Subscription,
}

impl SearchBar {
    /// A hidden bar for window `window`, posting through `poster`.
    pub fn new(window_id: Uuid, poster: Poster, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
        // `set_value` emits no change: only typing sends the needle
        let events = cx.subscribe(&field, |bar, field, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let needle = field.read(cx).value().to_string();
                bar.send(SearchInput::Needle(needle));
            }
        });
        Self {
            window: window_id,
            poster,
            field,
            counter: String::new(),
            sent: 0,
            landed: Arc::new(AtomicU64::new(0)),
            shown: None,
            _field_events: events,
        }
    }

    /// Whether the bar shows.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown.is_some()
    }

    /// The counter's text.
    #[must_use]
    pub fn counter(&self) -> &str {
        &self.counter
    }

    /// What takes the keyboard: the field.
    #[must_use]
    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.field.read(cx).focus_handle(cx)
    }

    /// The field.
    #[must_use]
    pub fn field(&self) -> &Entity<InputState> {
        &self.field
    }

    /// Posts `input` for the search the bar shows now; the app drops it if another search shows by the time it runs.
    pub fn send(&mut self, input: SearchInput) {
        let window = self.window;
        let Some(key) = self.shown else {
            return;
        };
        let landed = matches!(input, SearchInput::Needle(_)).then(|| {
            self.sent += 1;
            (Arc::clone(&self.landed), self.sent)
        });
        post(&self.poster, move |state: &mut AppState| {
            // landed before the settle renders it, so that render already counts it
            if let Some((landed, number)) = landed {
                landed.store(number, Ordering::Release);
            }
            state.search_bar(window, key, input);
        });
    }

    /// Whether every needle the field sent has landed in the model.
    fn idle(&self) -> bool {
        self.landed.load(Ordering::Acquire) == self.sent
    }

    /// Brings the bar in line with the window's active search; whether it came up for a session just now, which the
    /// caller gives the keyboard when `focus` (no modal holds it), the field's cursor at the needle's end.
    pub fn sync(&mut self, core: &Core, focus: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((session, search)) = shown_search(core, self.window) else {
            if self.shown.take().is_some() {
                cx.notify();
            }
            return false;
        };
        let fresh = self
            .shown
            .replace((session, search.owner))
            .is_none_or(|(shown, _)| shown != session);
        let needle = search.needle.clone();
        if (fresh || self.idle()) && self.field.read(cx).value().as_ref() != needle {
            let held = self.focus_handle(cx).is_focused(window);
            self.field.update(cx, |field, cx| {
                field.set_value(needle, window, cx);
                if held || (fresh && focus) {
                    // after its text, as the GTK entry's `set_position(-1)`; it takes the keyboard (it holds it, or
                    // is about to)
                    let end = Position {
                        line: 0,
                        character: u32::MAX,
                    };
                    field.set_cursor_position(end, window, cx);
                }
            });
        }
        self.counter = search.display_text();
        if fresh && focus {
            let handle = self.focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
        fresh
    }

    fn button(
        id: &'static str,
        icon: IconName,
        hint: &'static str,
        input: SearchInput,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = crate::chrome::current(cx);
        let ink = crate::chrome::color(colors.foreground, 0.8);
        div()
            .id(id)
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(px(20.))
            .rounded(px(4.))
            .cursor_pointer()
            .hover(move |style| style.bg(crate::chrome::color(colors.foreground, 0.12)))
            // a button never takes the keyboard from the field
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(cx.listener(move |bar, _, _, _| bar.send(input.clone())))
            .tooltip(move |_, cx| {
                let text = SharedString::from(hint);
                cx.new(|_| crate::titlebar::Hint { text, colors }).into()
            })
            .child(crate::picker::glyph(icon, ink, px(14.)))
            .into_any_element()
    }
}

impl Render for SearchBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.shown.is_none() {
            return div().id("search-bar");
        }
        let colors = crate::chrome::current(cx);
        let ink = crate::chrome::color(colors.foreground, 1.0);
        let soft = crate::chrome::color(colors.foreground, 0.7);
        let previous = Self::button(
            "search-previous",
            IconName::ChevronUp,
            "Previous match",
            SearchInput::Step(false),
            cx,
        );
        let next = Self::button(
            "search-next",
            IconName::ChevronDown,
            "Next match",
            SearchInput::Step(true),
            cx,
        );
        let close = Self::button("search-close", IconName::X, "Close search", SearchInput::Close, cx);
        div()
            .id("search-bar")
            .debug_selector(|| "search-bar".to_owned())
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(8.))
            .bg(crate::chrome::color(colors.background, 1.0))
            .border_1()
            .border_color(crate::chrome::color(colors.foreground, 0.18))
            .shadow_md()
            .text_size(px(13.))
            .text_color(ink)
            .child(crate::picker::glyph(IconName::Search, soft, px(14.)))
            .child(
                div()
                    .id("search-field")
                    .debug_selector(|| "search-field".to_owned())
                    .w(px(FIELD_WIDTH))
                    .child(Input::new(&self.field).appearance(false).text_color(ink)),
            )
            .child(
                div()
                    .id("search-counter")
                    .debug_selector(|| "search-counter".to_owned())
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(soft)
                    .child(SharedString::from(self.counter.clone())),
            )
            .child(previous)
            .child(next)
            .child(close)
    }
}

impl AppState {
    /// Window `window`'s search bar asked for `input` for search `key` (upstream's `updateSearchNeedle`,
    /// `navigateSearch`, `endSearch`); dropped when another search shows by now (a queued selection change landed
    /// first), so it never acts on a session it was not typed for.
    pub fn search_bar(&mut self, window: Uuid, key: SearchKey, input: SearchInput) {
        if self.is_quitting() || shown_key(&self.core, window) != Some(key) {
            return;
        }
        match input {
            SearchInput::Needle(needle) => self.core.search_bar_needle(window, &needle),
            SearchInput::Step(next) => self.core.search_bar_step(window, next),
            SearchInput::Close => self.core.search_bar_close(window),
        }
        self.settle();
    }
}

#[cfg(test)]
mod tests;
