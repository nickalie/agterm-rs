//! A terminal's keyboard: the keys the app's interceptor hands the focused owner view (`crate::keymap_dispatch`), the
//! input method, and focus reports.
//!
//! - Keys: ghostty's binds first (`binds::Keyboard`, the half-typed sequence and the active key tables; actions
//!   performed through [`TerminalSurface::perform`]), then `agterm-term`'s encoder (legacy or Kitty per the
//!   terminal's modes). Every key the terminal gets is the terminal's, an `ignore`d one too (the window's keymap
//!   already passed it, and GPUI's input handler would otherwise type its text); only a key while the input method
//!   composes, or a key for a terminal with no pane, goes back to the window. Ghostty's app, window, tab and split
//!   actions stay unperformed, as libghostty's apprt callbacks left them in the GTK build.
//! - Releases come through the view's key-up listener: the binds consume the release of a press they consumed, the
//!   encoder reports the rest only under the Kitty protocol's event reporting.
//! - The input method: the owner view is GPUI's input handler while focused. A composition is the terminal's preedit
//!   (drawn at the cursor, never sent); a commit of several characters is sent as text (Ghostty's `textCallback`, a
//!   paste of `PasteSource::Text`), one character comes as a key with no position. The candidate window is placed at
//!   the cursor's cell.
//! - Focus reports (mode 1004): the view holds the keyboard and its window is active; sent on each change.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::{Rc, Weak};

use agterm_core::ghostty_config::binds::{Action, KeyEffect, Keyboard, Performer, Queued};
use agterm_core::keymap::TypingSurface;
use agterm_term::{ClipboardLocation, Pane, PasteSource, SelectionText, Typing};
use gpui_kit::{
    AnyWindowHandle, App, Bounds, Context, CursorHideMode, EntityInputHandler, FocusHandle, KeyUpEvent, Pixels,
    Subscription, UTF16Selection, WeakEntity, Window, point, px, size,
};

use super::input::KeyStroke;
use super::{Shared, TerminalSurface, TerminalView};

/// A terminal owner view the interceptor can hand keys to.
struct Owner {
    focus: FocusHandle,
    view: WeakEntity<TerminalView>,
    shared: Weak<Shared>,
    /// The window the view last painted in.
    window: Option<AnyWindowHandle>,
}

thread_local! {
    static OWNERS: RefCell<Vec<Owner>> = const { RefCell::new(Vec::new()) };
}

/// Registers an owner view (its terminal's input).
pub(super) fn register(focus: &FocusHandle, view: WeakEntity<TerminalView>, shared: &Rc<Shared>) {
    OWNERS.with_borrow_mut(|owners| {
        owners.retain(|owner| owner.shared.strong_count() > 0 && owner.view.upgrade().is_some());
        owners.push(Owner {
            focus: focus.clone(),
            view,
            shared: Rc::downgrade(shared),
            window: None,
        });
    });
}

/// Records the window terminal `key`'s owner painted in.
pub(super) fn painted_in(key: u64, window: AnyWindowHandle) {
    OWNERS.with_borrow_mut(|owners| {
        for owner in owners.iter_mut() {
            if owner.shared.upgrade().is_some_and(|shared| shared.key() == key) {
                owner.window = Some(window);
            }
        }
    });
}

/// The window terminal `key`'s owner painted in last.
pub(super) fn window_of(key: u64) -> Option<AnyWindowHandle> {
    OWNERS.with_borrow(|owners| {
        owners
            .iter()
            .find(|owner| owner.shared.upgrade().is_some_and(|shared| shared.key() == key))
            .and_then(|owner| owner.window)
    })
}

/// Every live terminal with an owner view.
fn all_terminals() -> Vec<Rc<Shared>> {
    OWNERS.with_borrow(|owners| owners.iter().filter_map(|owner| owner.shared.upgrade()).collect())
}

/// The owner view holding `window`'s keyboard.
fn focused(window: &Window) -> Option<(WeakEntity<TerminalView>, Rc<Shared>)> {
    OWNERS.with_borrow(|owners| {
        owners
            .iter()
            .find(|owner| owner.focus.is_focused(window))
            .and_then(|owner| Some((owner.view.clone(), owner.shared.upgrade()?)))
    })
}

/// A key press for the terminal holding `window`'s keyboard; whether the terminal took it.
pub fn key_down(stroke: &KeyStroke, window: &mut Window, cx: &mut App) -> bool {
    let Some((view, _)) = focused(window) else {
        return false;
    };
    view.update(cx, |view, cx| view.key_down(stroke, window, cx))
        .unwrap_or(false)
}

/// The terminal holding `window`'s keyboard as a custom command's typing surface: its owner and its selection now.
#[must_use]
pub fn typing_surface(window: &Window, _cx: &App) -> Option<TypingSurface> {
    let (_, shared) = focused(window)?;
    let owner = shared.owner()?;
    Some(TypingSurface {
        owner,
        selection: shared.pane().and_then(|pane| pane.selection_text(SelectionText::Raw)),
    })
}

/// The owner view's keyboard state.
#[derive(Default)]
pub(super) struct KeyState {
    /// ghostty's half-typed sequence and active key tables.
    keyboard: Keyboard,
    /// The keys a half-typed sequence held back, in order.
    queued: Vec<KeyStroke>,
    /// The focus last reported to the program; `None` before the first report.
    reported: Option<bool>,
    subscriptions: Vec<Subscription>,
}

/// The binds' performer: actions on this terminal ([`TerminalSurface::perform`]), `all:` actions on every terminal,
/// the sequence's held keys sent on `end_key_sequence`.
struct KeyPerformer<'a> {
    surface: &'a TerminalSurface,
    pane: &'a Pane,
    queued: &'a mut Vec<KeyStroke>,
}

impl Performer for KeyPerformer<'_> {
    fn perform(&mut self, action: &Action) -> bool {
        self.surface.perform(action)
    }

    fn perform_all(&mut self, action: &Action) {
        let this = self.surface.shared().key();
        let _ = self.surface.perform(action);
        for shared in all_terminals().into_iter().filter(|shared| shared.key() != this) {
            let _ = TerminalSurface::new(shared).perform(action);
        }
    }

    fn flush_sequence(&mut self) {
        send(self.surface.shared(), self.pane, self.queued.drain(..));
    }
}

/// Encodes keys to the pane, in order; dropped in read-only mode (`toggle_readonly`).
fn send(shared: &Shared, pane: &Pane, keys: impl IntoIterator<Item = KeyStroke>) {
    if shared.is_readonly() {
        return;
    }
    for key in keys {
        if let Err(err) = pane.key(&key.input()) {
            tracing::warn!(target: "agterm::surface", %err, "a key did not reach the terminal");
        }
    }
}

impl TerminalView {
    /// A key press the interceptor handed this view (module docs); whether the terminal took it.
    pub(super) fn key_down(&mut self, stroke: &KeyStroke, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        if stroke.composing {
            return false;
        }
        let Some(pane) = self.shared.pane().filter(|_| self.shared.is_realized()) else {
            return false;
        };
        // a key that types something ends a composition the platform left on screen (a completed dead key)
        if !stroke.text.is_empty() && self.shared.preedit().is_some() {
            self.shared.set_preedit(None);
        }
        let config = self.shared.config();
        pane.set_typing(Typing {
            scroll_to_bottom: true,
            clear_selection: config.selection_clear_on_typing,
        });
        let surface = TerminalSurface::new(Rc::clone(&self.shared));
        let KeyState { keyboard, queued, .. } = &mut self.keys;
        let mut performer = KeyPerformer {
            surface: &surface,
            pane: &pane,
            queued,
        };
        let result = keyboard.handle(&config.keybinds, &stroke.input().binds_event(), &mut performer);
        match result.queued {
            Queued::Keep => {}
            Queued::Flush => send(&self.shared, &pane, queued.drain(..)),
            Queued::Drop => queued.clear(),
        }
        match result.effect {
            KeyEffect::Encode => send(&self.shared, &pane, [stroke.clone()]),
            KeyEffect::Queue => queued.push(stroke.clone()),
            KeyEffect::Consumed | KeyEffect::Closed | KeyEffect::Ignored => {}
        }
        cx.notify();
        true
    }

    /// A key went up while this view held the keyboard.
    pub(super) fn key_up(&mut self, event: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        crate::keymap_dispatch::key_up(event, window, cx);
        let Some(pane) = self.shared.pane().filter(|_| self.shared.is_realized()) else {
            return;
        };
        let stroke = KeyStroke::new(&event.keystroke, &super::input::current_source(&event.keystroke), true);
        let config = self.shared.config();
        let surface = TerminalSurface::new(Rc::clone(&self.shared));
        let KeyState { keyboard, queued, .. } = &mut self.keys;
        let mut performer = KeyPerformer {
            surface: &surface,
            pane: &pane,
            queued,
        };
        let result = keyboard.handle(&config.keybinds, &stroke.input().binds_event(), &mut performer);
        if result.effect == KeyEffect::Encode {
            send(&self.shared, &pane, [stroke]);
        }
    }

    /// What the owner view keeps up with each render: its focus subscriptions, the focus report, and GPUI's cursor
    /// hide mode from `mouse-hide-while-typing`.
    pub(super) fn watch_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.watch_focus(window, cx);
        self.sync_focus(window, cx);
        let hide = if self.shared.config().mouse.hide_while_typing {
            CursorHideMode::OnTyping
        } else {
            CursorHideMode::Never
        };
        if cx.cursor_hide_mode() != hide {
            cx.set_cursor_hide_mode(hide);
        }
    }

    /// Subscribes to the view's focus and its window's activation, once (the first render has the window).
    fn watch_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.keys.subscriptions.is_empty() {
            return;
        }
        let focus = self.focus.clone();
        self.keys.subscriptions = vec![
            cx.on_focus_in(&focus, window, Self::sync_focus),
            cx.on_focus_out(&focus, window, |view, _, window, cx| view.sync_focus(window, cx)),
            cx.observe_window_activation(window, Self::sync_focus),
        ];
    }

    /// Reports a focus change to a program in focus-reporting mode; the state the pane starts in is no change.
    fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focus.is_focused(window) && window.is_window_active();
        let Some(pane) = self.shared.pane().filter(|_| self.shared.is_realized()) else {
            return;
        };
        let Some(reported) = self.keys.reported.replace(focused) else {
            return;
        };
        if reported == focused {
            return;
        }
        if let Err(err) = pane.focus(focused) {
            tracing::debug!(target: "agterm::surface", %err, "focus report failed");
        }
        cx.notify();
    }

    /// Text an input method committed: sent as text, the composition ended.
    fn commit(&mut self, text: &str) {
        self.shared.set_preedit(None);
        if text.is_empty() || self.shared.is_readonly() {
            return;
        }
        let Some(pane) = self.shared.pane().filter(|_| self.shared.is_realized()) else {
            return;
        };
        if let Err(err) = pane.paste(text, PasteSource::Text, ClipboardLocation::Standard, true) {
            tracing::warn!(target: "agterm::surface", %err, "input method text did not reach the terminal");
        }
    }

    /// The cursor's cell in window coordinates, for the input method's candidate window.
    fn cursor_bounds(&self, element: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
        let frame = self.painted.as_ref()?;
        let position = frame.cursor.position?;
        let laid = self.shared.laid.get()?;
        let scale = self.shared.scale().max(1.0);
        let metrics = self.shared.metrics();
        let to_px = |device: u32| {
            #[expect(clippy::cast_precision_loss, reason = "pixel counts are far below 2^24")]
            let logical = device as f32 / scale;
            px(logical)
        };
        let x = laid.padding.left + u32::from(position.column) * metrics.cell_width;
        let y = laid.padding.top + u32::from(position.row) * metrics.cell_height;
        Some(Bounds::new(
            element.origin + point(to_px(x), to_px(y)),
            size(to_px(metrics.cell_width), to_px(metrics.cell_height)),
        ))
    }
}

/// The length of `text` in UTF-16 units.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.shared.preedit().as_deref().map_or(0, utf16_len);
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.shared.preedit().map(|preedit| 0..utf16_len(&preedit))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.shared.set_preedit(None);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit(text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shared.set_preedit(Some(new_text.to_owned()));
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui_kit::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
