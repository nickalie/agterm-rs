//! The panel window's root (upstream's `QuickTerminalPanelContent`): the terminal on the terminal's own background,
//! clipped to a 12-point radius with upstream's faint white edge, over the transparent window, so the corners show
//! what is behind; `quick-terminal` is its element id, as upstream's accessibility identifier.
//!
//! Each window the panel opens gets a root of its own and a fresh owner view of the long-lived terminal (a view keeps
//! what it painted in its window's atlas); the terminal itself lives in [`super::QuickPanel`]. The root reports what
//! its window does to the model: holding the keyboard and losing it (a summoned panel's dismissal), the display it
//! landed on (the layer host's refit), the window manager's close request (a dismissal), and the window going without
//! the panel closing it (the compositor's `closed` on a layer surface whose output went away: a dismissal too).

use std::cell::Cell;
use std::rc::Rc;

use agterm_core::chrome::Rgb;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, DisplayId, Entity, Focusable as _, Global, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, Size, Styled as _, Subscription, Window, div, px,
};

use crate::keymap_dispatch::{self, KeyOrigin};
use crate::state::{AppState, Poster, post};
use crate::terminal::{Terminal, TerminalView, ViewRole};

/// The frame's corner radius and the edge's white opacity (upstream's `QuickTerminalPanelContent`).
const RADIUS: f32 = 12.;
const EDGE_OPACITY: f64 = 0.18;

/// Whether the panel holds the keyboard (upstream's `holdsKey`): the decks read it, not the panel's visibility, since a
/// pinned panel stays on screen after the keyboard left it and must not keep the windows from their terminals.
#[derive(Default)]
struct Holds(bool);

impl Global for Holds {}

/// Whether the quick terminal's panel holds the keyboard.
#[must_use]
pub fn holds_keyboard(cx: &gpui_kit::App) -> bool {
    cx.try_global::<Holds>().is_some_and(|holds| holds.0)
}

/// Records whether the panel holds the keyboard.
pub(super) fn set_holds(holds: bool, cx: &mut gpui_kit::App) {
    if holds_keyboard(cx) != holds {
        cx.set_global(Holds(holds));
    }
}

/// What a panel window and the panel share.
#[derive(Debug, Default)]
pub struct Links {
    /// Set once the panel closes the window itself, so the window going is no dismissal.
    pub closing: Cell<bool>,
    /// The display the window is on, as the platform last reported it.
    pub landed: Cell<Option<DisplayId>>,
}

/// The panel window's root view.
pub struct QuickView {
    terminal: Entity<TerminalView>,
    background: Rgb,
    links: Rc<Links>,
    /// Whether the window held the keyboard since it opened: only losing it then is a focus loss.
    holds_key: bool,
    /// The window's size, last logged.
    size: Option<Size<Pixels>>,
    poster: Poster,
    _subscriptions: Vec<Subscription>,
}

impl QuickView {
    /// The root of a panel window drawing `terminal` on `background`.
    pub fn new(
        terminal: &Entity<Terminal>,
        background: Rgb,
        links: Rc<Links>,
        poster: Poster,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = cx.new(|cx| TerminalView::new(terminal, ViewRole::Owner, cx));
        let handle = window.window_handle();
        keymap_dispatch::register_window(handle, KeyOrigin::Quick);
        let (asking, closed) = (poster.clone(), Rc::clone(&links));
        window.on_window_should_close(cx, move |_, _| {
            if closed.closing.get() || asking.upgrade().is_none() {
                return true;
            }
            post(&asking, dismiss);
            false
        });
        links.landed.set(window.display(cx).map(|display| display.id()));
        let size = window.viewport_size();
        log_size(size);
        let subscriptions = vec![
            cx.observe_window_activation(window, Self::activation_changed),
            cx.observe_window_display(window, |view, window, cx| {
                let landed = window.display(cx).map(|display| display.id());
                if view.links.landed.replace(landed) != landed {
                    tracing::debug!(?landed, "quick terminal: on another display");
                    post(&view.poster, |_| {});
                }
            }),
            cx.observe_window_bounds(window, |view: &mut Self, window, cx| {
                let size = window.viewport_size();
                if view.size.replace(size) != Some(size) {
                    log_size(size);
                }
                cx.notify();
            }),
            cx.on_release(move |view: &mut Self, cx| view.released(handle, cx)),
        ];
        Self {
            terminal: view,
            background,
            links,
            holds_key: false,
            size: Some(size),
            poster,
            _subscriptions: subscriptions,
        }
    }

    /// The terminal's view in this window.
    #[must_use]
    pub fn terminal(&self) -> &Entity<TerminalView> {
        &self.terminal
    }

    /// Gives the keyboard to the terminal.
    pub fn focus_terminal(&self, window: &mut Window, cx: &mut gpui_kit::App) {
        let handle = self.terminal.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// The window took or lost the keyboard: holding it is the panel's (the decks step aside), losing it after holding
    /// it dismisses a panel the user summoned. A window the panel is closing reports nothing: its keyboard is the next
    /// window's (a reopen) or nobody's (a hide), which the panel settled as it closed it.
    fn activation_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.links.closing.get() {
            self.holds_key = false;
            return;
        }
        if window.is_window_active() {
            self.holds_key = true;
            tracing::info!("quick terminal: holds the keyboard");
            set_holds(true, cx);
            post(&self.poster, |_| {});
            return;
        }
        if std::mem::take(&mut self.holds_key) {
            tracing::info!("quick terminal: lost the keyboard");
            set_holds(false, cx);
            post(&self.poster, |state: &mut AppState| {
                state.core.quick_lost_focus();
                state.settle();
            });
            crate::window::check_keyboard_left(cx);
        }
    }

    /// The window went: the panel closed it, or the compositor did (its output went away), which hides the panel as a
    /// dismissal, its shell kept.
    fn released(&mut self, handle: AnyWindowHandle, cx: &mut gpui_kit::App) {
        keymap_dispatch::unregister_window(handle);
        if self.links.closing.get() {
            return;
        }
        if std::mem::take(&mut self.holds_key) {
            set_holds(false, cx);
        }
        tracing::info!("quick terminal: the compositor closed the panel");
        post(&self.poster, dismiss);
    }
}

/// The panel's own dismissal: it hides, its shell kept.
fn dismiss(state: &mut AppState) {
    state.core.dismiss_quick_terminal();
    state.settle();
}

/// Logs the panel's new size (what the tests on real displays read).
fn log_size(size: Size<Pixels>) {
    let size = format!("{}x{}", f32::from(size.width), f32::from(size.height));
    tracing::info!(%size, "quick terminal: sized");
}

impl Render for QuickView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let edge = crate::chrome::color(Rgb::WHITE, EDGE_OPACITY);
        div().id("quick-terminal").size_full().child(
            div()
                .size_full()
                .relative()
                .rounded(px(RADIUS))
                .overflow_hidden()
                .bg(crate::chrome::color(self.background, 1.0))
                .child(self.terminal.clone())
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(RADIUS))
                        .border_1()
                        .border_color(edge),
                ),
        )
    }
}
