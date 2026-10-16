//! The Ctrl-Tab session switcher (upstream's `Views/SessionSwitcher.swift`; port of the GTK build's
//! `session_switcher.rs`): hold Control and press Tab to walk the window's most recently used sessions (Shift+Tab walks
//! back), release Control to select the highlighted one, Escape to give up.
//!
//! The order is the window's session recency over the navigable set, at most [`MAX_CANDIDATES`]
//! ([`agterm_core::Core::switcher_candidates`]), frozen when the walk begins so cycling never reorders it; only the
//! commit does, by selecting. The walk begins on the previous session and does nothing with fewer than two.
//!
//! The window's key hook sees Ctrl+Tab before the keymap, any text field and the terminal: it is always consumed, and
//! while the window takes no UI actions it only ends a walk. Control going up reaches the switcher through the window's
//! modifier listener; a walk also ends when the window loses the keyboard. The overlay (`session-switcher`) is a
//! click-through panel of the candidates over a light scrim that takes no focus, so the terminal keeps the keyboard
//! throughout. Each row is the display name (a remote session's after the cloud) over `workspace · detail`, at the
//! Interface font size.

use agterm_core::Core;
use agterm_core::chrome::ChromeColors;
use agterm_core::ghostty_config::binds::Mods;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use uuid::Uuid;

use crate::modal::{self, Metrics};
use crate::picker::Placement;
use crate::row_icon::{self, Glyph};
use crate::state::{AppState, Poster, post};
use crate::terminal::input::KeyStroke;

/// The most sessions a walk offers (upstream's `SessionSwitcher.maxCandidates`).
pub const MAX_CANDIDATES: usize = 10;
/// The panel's width at the default interface size (upstream's `panelWidthAtDefaultFontSize`).
const PANEL_WIDTH: f64 = 460.0;
/// The inset between the panel's edge and its rows (upstream's `panelPadding`).
const PANEL_PADDING: f64 = 6.0;

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
pub fn switch_key(stroke: &KeyStroke) -> Option<SwitchKey> {
    match stroke.key.name() {
        "tab" if stroke.mods.contains(Mods::CTRL) => Some(SwitchKey::Tab {
            reverse: stroke.mods.contains(Mods::SHIFT),
        }),
        "escape" => Some(SwitchKey::Escape),
        _ => None,
    }
}

/// One candidate row: the display name over `workspace · detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: Uuid,
    pub title: String,
    pub subtitle: String,
    /// A session attached from another machine: the title leads with the cloud.
    pub remote: bool,
}

/// Window `window`'s candidates in recency order, with their rows.
#[must_use]
pub fn candidates(core: &Core, window: Uuid) -> Vec<Candidate> {
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
                remote: session.remote_host().is_some(),
            })
        })
        .collect()
}

/// One window's switcher (module docs).
pub struct SessionSwitcher {
    window: Uuid,
    poster: Poster,
    switcher: Switcher,
    /// Whether the window takes UI actions as of the last sync.
    can_switch: bool,
    /// The recency order as of the last sync.
    candidates: Vec<Candidate>,
    /// The rows of the walk under way.
    frozen: Vec<Candidate>,
    inset: f64,
    metrics: Metrics,
    scroll: ScrollHandle,
}

impl SessionSwitcher {
    /// A switcher for window `window`, posting through `poster`.
    #[must_use]
    pub fn new(window: Uuid, poster: Poster) -> Self {
        Self {
            window,
            poster,
            switcher: Switcher::default(),
            can_switch: false,
            candidates: Vec::new(),
            frozen: Vec::new(),
            inset: 0.0,
            metrics: Metrics::default(),
            scroll: ScrollHandle::new(),
        }
    }

    /// Whether a walk is under way.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.switcher.is_active()
    }

    /// The highlighted candidate's position.
    #[must_use]
    pub fn index(&self) -> usize {
        self.switcher.index()
    }

    /// The rows of the walk under way, by title (tests).
    #[must_use]
    pub fn titles(&self) -> Vec<String> {
        self.frozen.iter().map(|candidate| candidate.title.clone()).collect()
    }

    /// A key press; whether the switcher took it (Ctrl+Tab always, Escape during a walk).
    pub fn press(&mut self, key: SwitchKey, cx: &mut Context<Self>) -> bool {
        match key {
            SwitchKey::Tab { .. } if !self.can_switch => self.switcher.reset(),
            SwitchKey::Tab { reverse } if self.switcher.is_active() => self.switcher.advance(reverse),
            SwitchKey::Tab { .. } => {
                if self
                    .switcher
                    .begin(self.candidates.iter().map(|candidate| candidate.id).collect())
                {
                    self.frozen.clone_from(&self.candidates);
                }
            }
            SwitchKey::Escape if self.switcher.is_active() => self.switcher.reset(),
            SwitchKey::Escape => return false,
        }
        self.scroll.scroll_to_item(self.switcher.index());
        cx.notify();
        true
    }

    /// Control went up: the walk selects its highlighted session.
    pub fn release(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.switcher.commit() {
            let window = self.window;
            post(&self.poster, move |state: &mut AppState| {
                state.switch_to_session(window, session);
            });
            cx.notify();
        }
    }

    /// Ends a walk without selecting (the window lost the keyboard).
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        if self.switcher.is_active() {
            self.switcher.reset();
            cx.notify();
        }
    }

    /// Takes the window's recency order, whether it takes UI actions, the terminal area and the interface size; a walk
    /// ends when the window stops taking UI actions.
    pub fn sync(&mut self, core: &Core, inset: f64, cx: &mut Context<Self>) {
        let window = self.window;
        self.can_switch = core.library().active_window_id() == Some(window) && core.ui_actions_enabled(window);
        self.candidates = candidates(core, window);
        let metrics = Metrics::of(core.settings());
        if (self.inset, self.metrics) != (inset, metrics) {
            self.inset = inset;
            self.metrics = metrics;
            cx.notify();
        }
        if !self.can_switch {
            self.reset(cx);
        }
    }

    /// Sets the candidates and whether the window switches, as a sync would (tests).
    #[cfg(test)]
    pub(crate) fn set_candidates(&mut self, candidates: Vec<Candidate>, can_switch: bool) {
        self.candidates = candidates;
        self.can_switch = can_switch;
    }

    fn render_row(&self, index: usize, candidate: &Candidate, colors: ChromeColors) -> AnyElement {
        let metrics = self.metrics;
        let ink = crate::chrome::color(colors.foreground, 1.0);
        let dim = crate::chrome::color(colors.foreground, crate::chrome::DIM_ALPHA);
        div()
            .id(("session-switcher-row", index))
            .flex()
            .flex_col()
            .gap(px(1.))
            .px(modal::logical(metrics.scaled(12.0)))
            .py(modal::logical(metrics.scaled(6.0)))
            .rounded(px(6.))
            .when(index == self.switcher.index(), |row| {
                row.bg(crate::chrome::color(modal::ACCENT, 0.25))
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(modal::logical(metrics.scaled(4.0)))
                    .text_size(modal::logical(metrics.base))
                    .text_color(ink)
                    .when(candidate.remote, |line| {
                        line.child(row_icon::icon(
                            Glyph::Remote {
                                split: false,
                                disconnected: false,
                            },
                            ink,
                        ))
                    })
                    .child(div().truncate().child(SharedString::from(candidate.title.clone()))),
            )
            .child(
                div()
                    .truncate()
                    .text_size(modal::logical(metrics.secondary))
                    .text_color(dim)
                    .child(SharedString::from(candidate.subtitle.clone())),
            )
            .into_any_element()
    }
}

impl Render for SessionSwitcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.switcher.is_active() {
            return div().id("session-switcher-layer");
        }
        let colors = crate::chrome::current(cx);
        let placement = Placement::of(window.viewport_size(), self.inset, PANEL_WIDTH, self.metrics);
        let rows: Vec<AnyElement> = self
            .frozen
            .iter()
            .enumerate()
            .map(|(index, candidate)| self.render_row(index, candidate, colors))
            .collect();
        // no listener and no occlusion: clicks and the pointer go through to the window
        div()
            .id("session-switcher-layer")
            .absolute()
            .inset_0()
            .bg(modal::dim())
            .child(
                modal::panel(colors)
                    .id("session-switcher")
                    .debug_selector(|| "session-switcher".to_owned())
                    .absolute()
                    .left(modal::logical(placement.left))
                    .top(modal::logical(placement.top))
                    .w(modal::logical(placement.width))
                    .max_h(modal::logical(placement.cap))
                    .p(modal::logical(PANEL_PADDING))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("session-switcher-rows")
                            .flex()
                            .flex_col()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .children(rows),
                    ),
            )
    }
}

impl AppState {
    /// The Ctrl-Tab walk in window `window` ended on `session`.
    pub fn switch_to_session(&mut self, window: Uuid, session: Uuid) {
        if self.is_quitting() {
            return;
        }
        self.core.switch_to_session(window, session);
        self.settle();
    }
}

#[cfg(test)]
mod tests;
