//! The question dialog (`ask.open`), after upstream's `Views/AskDialogView.swift` and `ControlServer+Ask.swift` (port
//! of the GTK build's `ask_dialog.rs`).
//!
//! One dialog view ([`AskDialog`]) serves both owners:
//! - a terminal ask waits on its session and is drawn inside the session's page above every pane and the HUD (the
//!   deck lays it out over the whole page or the pane it was opened over, [`shown_scope`]), in the terminal's font and
//!   colors, square, with plain block buttons;
//! - a GUI ask holds its window's modal slot and is drawn over the window ([`crate::window::Layer::Ask`]), covering the
//!   terminal area or the session or pane it is anchored to, as a native rounded panel with native buttons, while the
//!   rest of the window takes no clicks.
//!
//! Either way a dim layer covers the anchor and the panel is centered in it ([`crate::modal`]): the title, the message
//! and a row of equal-width buttons that falls back to a column when the row does not fit, aligned as asked. The
//! keyboard follows [`AskNavigation`]: Tab, Right and Down move forward, Shift+Tab, Left and Up back, Return answers
//! with the highlighted button, a button's hotkey letter answers with it, Escape dismisses (`escaped`); a key with
//! Ctrl, Alt or Super goes on to the window's key monitor; content taller than its anchor scrolls, and the highlighted
//! button is kept in view ([`reveal_offset`]). Answers go to the core as main-loop tasks; a stale id is
//! ignored there. Element ids are upstream's accessibility ids: `ask-dialog`, `ask-title`, `ask-message`,
//! `ask-button-<id>`.
//!
//! The dialog takes the keyboard when it is allowed to (upstream's `wantsFocus`, [`wants_focus`]): a GUI ask as it
//! opens, a terminal ask while its session is shown and its window holds no picker or GUI ask, the quick terminal is
//! hidden and, for a pane ask, its pane is the focused one. The deck gives it the keyboard in place of the terminal it
//! covers and gives the terminal the keyboard back once the dialog ends.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use agterm_core::Core;
use agterm_core::chrome::{ChromeColors, Rgb};
use agterm_core::model::{AskAlignment, AskNavigation, AskStyle, OverlayPane, PendingAsk, Session};
use agterm_proto::{AskButton, ControlAskOutcome, ControlAskResult};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FocusHandle, FontWeight, HighlightStyle, Hsla, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, ParentElement as _, Pixels, Render, ScrollHandle, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, StyledText, UnderlineStyle, Window, canvas, div, point, px,
};
use uuid::Uuid;

use crate::modal::{self, ButtonLook, NavKey};
use crate::split::{AskScope, Slot};
use crate::state::{AppState, Poster, post};
use crate::terminal::TerminalConfig;

/// The GUI dialog's spacing unit (upstream's `cell` for `.gui`).
const GUI_CELL: f64 = 8.0;
/// The GUI dialog's text size.
const GUI_TEXT: f32 = 13.;
/// The font size when neither the settings nor the terminal config name one.
const DEFAULT_FONT_SIZE: f64 = 13.0;

// MARK: - the look

/// The terminal's font and colors the terminal style draws in (upstream's `askFont`, `chromeText` and
/// `terminalColor`): the settings' font family and size, else the terminal config's, and the terminal config's
/// foreground and background.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    pub family: Option<String>,
    /// Points.
    pub size: f64,
    pub foreground: Rgb,
    pub background: Rgb,
}

impl Look {
    /// The look the settings and the terminals' `config` give now.
    #[must_use]
    pub fn current(core: &Core, config: &TerminalConfig) -> Self {
        let settings = core.settings();
        let rgb = |color: agterm_term::Rgb| Rgb::new(color.r, color.g, color.b);
        Self {
            family: settings
                .font_family
                .clone()
                .filter(|family| !family.trim().is_empty())
                .or_else(|| config.font.family.first().cloned()),
            size: settings
                .font_size
                .filter(|size| *size > 0.0)
                .or_else(|| Some(f64::from(config.font.size)).filter(|size| *size > 0.0))
                .unwrap_or(DEFAULT_FONT_SIZE),
            foreground: rgb(config.theme.foreground),
            background: rgb(config.theme.background),
        }
    }

    /// The font size in logical pixels (a point is 4/3 of a pixel, as the terminal counts it).
    #[must_use]
    pub fn font_pixels(&self) -> f64 {
        self.size * 4.0 / 3.0
    }

    /// One terminal cell across (upstream's `terminalCell`, `max(8, pointSize * 0.6)`).
    #[must_use]
    pub fn terminal_cell(&self) -> f64 {
        (self.font_pixels() * 0.6).max(8.0)
    }

    /// The spacing unit of a dialog in `style`.
    #[must_use]
    pub fn cell(&self, style: AskStyle) -> f64 {
        match style {
            AskStyle::Gui => GUI_CELL,
            AskStyle::Terminal => self.terminal_cell(),
        }
    }
}

// MARK: - keys and labels

/// What a key does to a dialog (upstream's `AskKey`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskKey {
    Forward,
    Backward,
    Activate,
    Cancel,
    /// A letter, lower-cased.
    Hotkey(String),
}

/// The dialog's action for a key press (upstream's `AskKeyCatcher.key(for:)`); `None` for a key it ignores, and for
/// any key with Ctrl, Alt or Super held, which goes on to the window.
#[must_use]
pub fn ask_key(keystroke: &gpui_kit::Keystroke) -> Option<AskKey> {
    Some(match modal::nav_key(keystroke)? {
        NavKey::Forward => AskKey::Forward,
        NavKey::Backward => AskKey::Backward,
        NavKey::Activate => AskKey::Activate,
        NavKey::Cancel => AskKey::Cancel,
        NavKey::Letter(letter) => AskKey::Hotkey(letter),
        NavKey::Space => return None,
    })
}

/// A button's label (upstream's `buttonLabel`): the text, the hotkey's first case-insensitive occurrence underlined
/// (else ` (X)` appended with the letter underlined), and whether it is bold; a terminal-style destructive button is
/// prefixed with `! ` and bold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonLabel {
    pub text: String,
    /// The byte range underlined.
    pub underline: Option<Range<usize>>,
    pub bold: bool,
}

/// The label of `button` (module docs of [`ButtonLabel`]).
#[must_use]
pub fn button_label(button: &AskButton, terminal_destructive: bool) -> ButtonLabel {
    let prefix = if terminal_destructive { "! " } else { "" };
    let label = &button.label;
    let (text, underline) = match button.hotkey.as_deref() {
        Some(hotkey) => {
            let wanted = hotkey.to_lowercase();
            let found = label
                .char_indices()
                .find(|(_, ch)| ch.to_lowercase().to_string() == wanted);
            if let Some((at, ch)) = found {
                let start = prefix.len() + at;
                (format!("{prefix}{label}"), Some(start..start + ch.len_utf8()))
            } else {
                let upper = hotkey.to_uppercase();
                let start = prefix.len() + label.len() + 2;
                (format!("{prefix}{label} ({upper})"), Some(start..start + upper.len()))
            }
        }
        None => (format!("{prefix}{label}"), None),
    };
    ButtonLabel {
        text,
        underline,
        bold: terminal_destructive,
    }
}

/// How a button draws (upstream's `guiButton`/`terminalButton`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskButtonLook {
    /// A GUI button.
    Gui(ButtonLook),
    /// A terminal block at 12% foreground.
    Block,
    /// The highlighted terminal block: inverted.
    Inverted,
}

/// In the GUI style the highlighted button is the prominent one (destructive red when it is the destructive button)
/// and a destructive one is otherwise red text; in the terminal style the highlighted block is inverted.
#[must_use]
pub fn button_look(style: AskStyle, highlighted: bool, destructive: bool) -> AskButtonLook {
    match style {
        AskStyle::Gui => AskButtonLook::Gui(match (highlighted, destructive) {
            (true, true) => ButtonLook::Destructive,
            (true, false) => ButtonLook::Suggested,
            (false, true) => ButtonLook::DestructiveText,
            (false, false) => ButtonLook::Plain,
        }),
        AskStyle::Terminal if highlighted => AskButtonLook::Inverted,
        AskStyle::Terminal => AskButtonLook::Block,
    }
}

// MARK: - the button strip

/// Whether the buttons go in a row or a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripAxis {
    Row,
    Column,
}

/// The row's width: every button as wide as the widest (upstream's `AskButtonLayout.sharedWidth`).
#[must_use]
pub fn row_width(widest: f64, count: usize, spacing: f64) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "at most six buttons")]
    let count = count as f64;
    widest * count + spacing * (count - 1.0).max(0.0)
}

/// A row while it fits `available`, else a column (upstream's `ViewThatFits(in: .horizontal)`).
#[must_use]
pub fn strip_axis(widest: f64, count: usize, spacing: f64, available: f64) -> StripAxis {
    if row_width(widest, count, spacing) <= available {
        StripAxis::Row
    } else {
        StripAxis::Column
    }
}

// MARK: - answers and placement

/// How the user ended a dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// The button at this index, in the caller's order.
    Button(usize),
    /// Escape.
    Escaped,
}

impl Answer {
    /// The outcome `ask` reports for this answer; `None` for a button past its last.
    #[must_use]
    pub fn result(self, ask: &PendingAsk) -> Option<ControlAskResult> {
        match self {
            Self::Button(index) => ask.answer(index),
            Self::Escaped => Some(ControlAskResult::outcome(ControlAskOutcome::Escaped)),
        }
    }
}

/// What a session's terminal ask covers while it shows (upstream's `SessionAskInput.visible` and `coveredFrame`): the
/// whole page, or its pane while that pane is laid out and no scratch covers it; nothing while the page is not shown.
#[must_use]
pub fn shown_scope(session: &Session, shown: bool) -> Option<AskScope> {
    // one a viewer presents keeps the slot here but is drawn and answered there
    let ask = session.ask().filter(|pending| pending.remote_owner.is_none())?;
    if !shown {
        return None;
    }
    if ask.pane_identity.is_none() {
        return Some(AskScope::Session);
    }
    let pane = session.ask_target_pane()?;
    (session.renders_overlay_pane(pane) && !session.scratch_active).then_some(AskScope::Pane(Slot::of_overlay(pane)))
}

/// Whether a shown terminal ask takes the keyboard (upstream's `wantsFocus`): nothing else holds it and, for a pane
/// ask, its pane is the focused one.
#[must_use]
pub fn wants_focus(session: &Session, focus_blocked: bool) -> bool {
    if focus_blocked {
        return false;
    }
    match session.ask_target_pane() {
        None => true,
        Some(pane) => {
            pane == if session.split.focused {
                OverlayPane::Right
            } else {
                OverlayPane::Left
            }
        }
    }
}

// MARK: - the dialog

/// The scroll offset (GPUI's: negative going down) that brings a button spanning `item` (as drawn at `offset`) into
/// the content's `viewport`, moving it the least (the GTK build's `clamp_page`, upstream's `scrollTo`): its top first
/// when it is taller than the viewport; `offset` when it is in view already.
#[must_use]
pub fn reveal_offset(viewport: Range<Pixels>, item: Range<Pixels>, offset: Pixels) -> Pixels {
    if item.start < viewport.start || item.end - item.start > viewport.end - viewport.start {
        offset + (viewport.start - item.start)
    } else if item.end > viewport.end {
        offset - (item.end - viewport.end)
    } else {
        offset
    }
}

/// Whose ask a dialog shows: a window's GUI ask, or a session's terminal ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskOwner {
    Window(Uuid),
    Session(Uuid),
}

/// One open dialog (module docs).
pub struct AskDialog {
    ask: PendingAsk,
    owner: AskOwner,
    look: Look,
    navigation: AskNavigation,
    destructive: Option<usize>,
    focus: FocusHandle,
    /// The anchor's size at the last layout, which the panel's cap is counted from.
    anchor: Rc<Cell<Option<Size<Pixels>>>>,
    /// The content's scroll, which keeps the highlighted button in view.
    scroll: ScrollHandle,
    /// The highlighted button is to be brought into view at the next layout (the dialog opened, the keyboard moved).
    reveal: Rc<Cell<bool>>,
    /// An answer went out: later keys and clicks do nothing.
    done: bool,
    poster: Poster,
}

impl AskDialog {
    #[must_use]
    pub fn new(ask: &PendingAsk, owner: AskOwner, look: Look, poster: Poster, cx: &mut Context<Self>) -> Self {
        Self {
            navigation: AskNavigation::new(ask),
            destructive: ask
                .destructive_id
                .as_ref()
                .and_then(|id| ask.buttons.iter().position(|button| &button.id == id)),
            ask: ask.clone(),
            owner,
            look,
            focus: cx.focus_handle(),
            anchor: Rc::default(),
            scroll: ScrollHandle::new(),
            reveal: Rc::new(Cell::new(true)),
            done: false,
            poster,
        }
    }

    /// The ask shown.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.ask.id
    }

    #[must_use]
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The highlighted button.
    #[must_use]
    pub fn highlighted(&self) -> Option<usize> {
        self.navigation.highlighted()
    }

    /// Takes a new look (the terminal's font or colors changed).
    pub fn set_look(&mut self, look: Look, cx: &mut Context<Self>) {
        if self.look != look {
            self.look = look;
            cx.notify();
        }
    }

    /// Ends the dialog with `answer`, once: the answer goes to the core.
    pub fn finish(&mut self, answer: Answer) {
        if self.done {
            return;
        }
        self.done = true;
        let Some(result) = answer.result(&self.ask) else {
            return;
        };
        let id = self.ask.id.clone();
        match self.owner {
            AskOwner::Window(window) => post(&self.poster, move |state: &mut AppState| {
                state.answer_window_ask(window, &id, result);
            }),
            AskOwner::Session(session) => post(&self.poster, move |state: &mut AppState| {
                state.answer_session_ask(session, &id, result);
            }),
        }
    }

    /// One key's effect.
    pub fn handle(&mut self, key: AskKey) {
        match key {
            AskKey::Forward => {
                self.navigation.move_forward();
                self.reveal.set(true);
            }
            AskKey::Backward => {
                self.navigation.move_backward();
                self.reveal.set(true);
            }
            AskKey::Activate => {
                if let Some(index) = self.navigation.activate() {
                    self.finish(Answer::Button(index));
                }
            }
            AskKey::Cancel => self.finish(Answer::Escaped),
            AskKey::Hotkey(letter) => {
                if let Some(index) = self.navigation.hotkey(&letter) {
                    self.finish(Answer::Button(index));
                }
            }
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            // the window's key monitor had it first; anything it left is no dialog's
            return;
        }
        cx.stop_propagation();
        if let Some(key) = ask_key(&event.keystroke) {
            self.handle(key);
            cx.notify();
        }
    }

    /// A press on the dialog outside its buttons: it takes the keyboard back, and a terminal ask's pane becomes the
    /// focused one (upstream's `SessionAskInput.selectPane`).
    fn pressed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if let AskOwner::Session(session) = self.owner {
            post(&self.poster, move |state: &mut AppState| state.select_ask_pane(session));
        }
    }

    fn gui(&self) -> bool {
        self.ask.style == AskStyle::Gui
    }

    fn ink(&self, colors: ChromeColors) -> Rgb {
        if self.gui() {
            colors.foreground
        } else {
            self.look.foreground
        }
    }

    fn text_size(&self) -> Pixels {
        if self.gui() {
            px(GUI_TEXT)
        } else {
            modal::logical(self.look.font_pixels())
        }
    }

    /// The widest button's natural width, its label shaped in the dialog's font.
    fn widest_button(&self, cell: f64, window: &Window) -> f64 {
        let mut style = window.text_style();
        if !self.gui()
            && let Some(family) = &self.look.family
        {
            style.font_family = family.clone().into();
        }
        let size = self.text_size();
        let padding = if self.gui() { 2.0 * 12.0 } else { cell };
        self.ask
            .buttons
            .iter()
            .enumerate()
            .map(|(index, button)| {
                let label = button_label(button, !self.gui() && self.destructive == Some(index));
                let run = style.to_run(label.text.len());
                let shaped = window
                    .text_system()
                    .shape_line(label.text.replace('\n', " ").into(), size, &[run], None);
                f64::from(f32::from(shaped.width)) + padding
            })
            .fold(0.0, f64::max)
    }

    fn render_button(
        &self,
        index: usize,
        width: Option<f64>,
        colors: ChromeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let button = &self.ask.buttons[index];
        let destructive = self.destructive == Some(index);
        let highlighted = self.navigation.highlighted() == Some(index);
        let label = button_label(button, !self.gui() && destructive);
        let mut highlights = Vec::new();
        if let Some(range) = label.underline.clone() {
            let underline = UnderlineStyle {
                thickness: px(1.),
                ..UnderlineStyle::default()
            };
            highlights.push((
                range,
                HighlightStyle {
                    underline: Some(underline),
                    ..HighlightStyle::default()
                },
            ));
        }
        let text = StyledText::new(label.text).with_highlights(highlights);
        let (fill, ink) = self.button_colors(button_look(self.ask.style, highlighted, destructive), colors);
        let cell = self.look.cell(self.ask.style);
        let selector = format!("ask-button-{}", button.id);
        let reveal = highlighted.then(|| self.reveal_canvas());
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector)
            .relative()
            .when_some(width, |button, width| button.w(modal::logical(width)))
            .flex()
            .justify_center()
            .text_center()
            .bg(fill)
            .text_color(ink)
            .when(label.bold, |button| button.font_weight(FontWeight::BOLD))
            .when(self.gui(), |button| button.px_3().py(px(5.)).rounded(px(6.)))
            .when(!self.gui(), |button| {
                button.px(modal::logical(cell * 0.5)).py(modal::logical(cell * 0.4))
            })
            .cursor_pointer()
            .child(text)
            .children(reveal)
            // a press on a button is the button's: it does not focus the dialog or select its pane
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |dialog, _, _, cx| {
                dialog.finish(Answer::Button(index));
                cx.notify();
            }))
            .into_any_element()
    }

    /// Over the highlighted button: once it is laid out, brings it into the content's view if asked to, on the next
    /// frame (a frame being drawn takes no refresh).
    fn reveal_canvas(&self) -> AnyElement {
        let (scroll, reveal) = (self.scroll.clone(), Rc::clone(&self.reveal));
        canvas(
            move |bounds, window, _| {
                let viewport = scroll.bounds();
                if !reveal.get() || viewport.size.height <= px(0.) {
                    return;
                }
                reveal.set(false);
                let offset = scroll.offset();
                let y = reveal_offset(
                    viewport.top()..viewport.bottom(),
                    bounds.top()..bounds.bottom(),
                    offset.y,
                );
                if y != offset.y {
                    window.on_next_frame(move |window, _| {
                        scroll.set_offset(point(offset.x, y));
                        window.refresh();
                    });
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    fn button_colors(&self, look: AskButtonLook, colors: ChromeColors) -> (Hsla, Hsla) {
        let (fg, bg) = (self.look.foreground, self.look.background);
        match look {
            AskButtonLook::Gui(look) => modal::button_colors(look, colors),
            AskButtonLook::Block => (crate::chrome::color(fg, 0.12), crate::chrome::color(fg, 1.0)),
            AskButtonLook::Inverted => (crate::chrome::color(fg, 1.0), crate::chrome::color(bg, 1.0)),
        }
    }

    fn render_strip(&self, cap: f64, window: &Window, colors: ChromeColors, cx: &mut Context<Self>) -> AnyElement {
        let cell = self.look.cell(self.ask.style);
        let count = self.ask.buttons.len();
        let widest = self.widest_button(cell, window);
        let available = (cap - 4.0 * cell).max(0.0);
        let axis = strip_axis(widest, count, cell, available);
        let width = match axis {
            StripAxis::Row => widest,
            StripAxis::Column => widest.min(available),
        };
        let buttons: Vec<AnyElement> = (0..count)
            .map(|index| self.render_button(index, Some(width), colors, cx))
            .collect();
        let strip = div()
            .flex()
            .w_full()
            .gap(modal::logical(cell))
            .when(axis == StripAxis::Column, gpui_kit::Styled::flex_col)
            .when(axis == StripAxis::Row, gpui_kit::Styled::flex_row);
        let strip = match (axis, self.ask.align) {
            (StripAxis::Row, AskAlignment::Left) => strip.justify_start(),
            (StripAxis::Row, AskAlignment::Center) => strip.justify_center(),
            (StripAxis::Row, AskAlignment::Right) => strip.justify_end(),
            (StripAxis::Column, AskAlignment::Left) => strip.items_start(),
            (StripAxis::Column, AskAlignment::Center) => strip.items_center(),
            (StripAxis::Column, AskAlignment::Right) => strip.items_end(),
        };
        strip.children(buttons).into_any_element()
    }

    fn render_panel(&self, anchor: Size<Pixels>, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = crate::chrome::current(cx);
        let cell = self.look.cell(self.ask.style);
        let anchor_width = f64::from(f32::from(anchor.width));
        let anchor_height = f64::from(f32::from(anchor.height));
        let cap = modal::ask_panel_cap(self.ask.width, anchor_width, self.look.terminal_cell());
        let ink = self.ink(colors);
        let message_ink = if self.gui() {
            crate::chrome::color(ink, crate::chrome::DIM_ALPHA)
        } else {
            crate::chrome::color(ink, 0.7)
        };
        let panel = if self.gui() {
            modal::panel(colors)
        } else {
            div()
                .bg(crate::chrome::color(self.look.background, 1.0))
                .text_color(crate::chrome::color(ink, 1.0))
                .border_1()
                .border_color(crate::chrome::color(self.look.foreground, 0.3))
                .when_some(self.look.family.clone(), gpui_kit::Styled::font_family)
        };
        let strip = self.render_strip(cap, window, colors, cx);
        let content = div()
            .id("ask-content")
            .debug_selector(|| "ask-content".to_owned())
            .flex()
            .flex_col()
            .gap(modal::logical(cell * 1.5))
            .p(modal::logical(cell * 2.0))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .max_h(modal::logical((anchor_height - 2.0 * cell).max(0.0)))
            .child(
                div()
                    .id("ask-title")
                    .font_weight(FontWeight::BOLD)
                    .child(SharedString::from(self.ask.title.clone())),
            )
            .when_some(
                self.ask.message.clone().filter(|message| !message.is_empty()),
                |content, message| {
                    content.child(
                        div()
                            .id("ask-message")
                            .text_color(message_ink)
                            .child(SharedString::from(message)),
                    )
                },
            )
            .child(strip);
        panel
            .id("ask-dialog")
            .text_size(self.text_size())
            .max_w(modal::logical(cap))
            .when(self.ask.width.is_some(), |panel| panel.w(modal::logical(cap)))
            .child(content)
            .into_any_element()
    }
}

impl Render for AskDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let anchor = Rc::clone(&self.anchor);
        // the panel is counted from the anchor measured here, so a new size is drawn on the next frame (a frame being
        // drawn takes no refresh)
        let measure = canvas(
            move |bounds, window, _| {
                if anchor.get() != Some(bounds.size) {
                    anchor.set(Some(bounds.size));
                    window.on_next_frame(|window, _| window.refresh());
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full();
        let panel = self.anchor.get().map(|size| self.render_panel(size, window, cx));
        div()
            .id("ask-layer")
            .key_context("AgtermAsk")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .relative()
            .occlude()
            .bg(modal::dim())
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|dialog, _, window, cx| {
                    dialog.pressed(window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(measure)
            .children(panel)
    }
}

#[cfg(test)]
mod tests;
