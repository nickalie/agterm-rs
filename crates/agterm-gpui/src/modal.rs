//! What the window's dialogs share (the GTK build's `modal.rs`): the panel geometry, the dimming backdrop and the
//! native panel's look, the keys a dialog navigates by, and the window's alerts.
//!
//! - Geometry: an ask's panel is centered in its anchor over a dim layer covering that anchor (upstream's
//!   `AskDialogView`): the layout centers it, as wide as its content up to [`ask_panel_cap`] (or that percent of the
//!   anchor), its content scrolling past the anchor's height less a cell each side. A palette's panel sits near the
//!   top of the window over a scrim covering all of it (upstream's `CommandPalette` in `Palette.swift`), a pure
//!   function of the sizes ([`palette_rect`]).
//! - Alerts ([`open_alert`]): the confirmations, Rename Window and the clipboard prompt (upstream's `NSAlert` sheets,
//!   the GTK build's `adw::AlertDialog`): a heading, a body, an optional text field (which takes the keyboard) or
//!   checks ("Don't ask again this session", the welcome's installs), and a row of buttons (stacked from three on), one
//!   the default (Return) and one the cancel (Escape); a body taller than the window scrolls under the buttons; Tab
//!   and the arrows move the keyboard's ring between the checks and the buttons. An alert is modal over its whole
//!   window: it dims the window, takes every click and keeps every key while it is up (the key monitor leaves an `AgtermAlert` context alone).
//!   Alerts of one window queue: the next comes up once the front one is answered. The window's view draws the front
//!   one ([`alert_layer`]); the [`Alerts`] registry tells observers when one opens or closes, which is how the window
//!   gives the keyboard back to the shown terminal once the last one is answered. A window that closes cancels its
//!   alerts ([`dismiss_alerts`]).

use std::collections::HashMap;

use agterm_core::chrome::{ChromeColors, Rgb};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, BorrowAppContext as _, Context, Entity, FocusHandle,
    Focusable as _, FontWeight, Global, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, Keystroke,
    MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, div, px,
};

use crate::split::Rect;

// MARK: - geometry

/// Clearance kept between a panel edge and the window edge (upstream's `InterfaceMetrics.panelMargin`).
const PANEL_MARGIN: f64 = 16.0;
/// The width a palette prefers not to shrink below (upstream's `minimumPanelWidth`).
const MINIMUM_PANEL_WIDTH: f64 = 280.0;
/// The height below which a palette stops shrinking (upstream's `minimumPanelHeight`).
const MINIMUM_PANEL_HEIGHT: f64 = 160.0;
/// The palette's width at the default interface size (upstream's `panelWidthAtDefaultFontSize`).
const PALETTE_WIDTH: f64 = 520.0;
/// How far down the frame the palette starts (upstream's `topInsetFraction`).
const PALETTE_TOP_FRACTION: f64 = 0.12;
/// A content-sized ask's widest panel, in terminal cells (upstream's `72 * terminalCell`).
pub const ASK_MAX_CELLS: f64 = 72.0;

/// The widest a content-sized ask panel may grow in an anchor `anchor_width` wide: 90% of the anchor or 72 terminal
/// cells, whichever is less (upstream's `ViewThatFits` cap); a fixed-width one is its clamped percent of the anchor.
#[must_use]
pub fn ask_panel_cap(width_percent: Option<i64>, anchor_width: f64, terminal_cell: f64) -> f64 {
    let anchor_width = anchor_width.max(0.0);
    match width_percent {
        // a percent within 10...100, so the conversion is exact
        #[expect(clippy::cast_precision_loss, reason = "a percent within 10...100")]
        Some(percent) => anchor_width * percent.clamp(10, 100) as f64 / 100.0,
        None => (anchor_width * 0.9).min(ASK_MAX_CELLS * terminal_cell),
    }
}

/// The palette's width in a frame `width` wide with the terminal area `inset` from its left (upstream's
/// `fittedPanelWidth`): the ideal width, shrunk to what the terminal area leaves, never past the frame.
#[must_use]
pub fn palette_width(width: f64, inset: f64) -> f64 {
    fitted_width(PALETTE_WIDTH, width, inset)
}

/// A panel `ideal` wide (already scaled to the interface size) fitted to a frame `width` wide with the terminal area
/// `inset` from its left (upstream's `InterfaceMetrics.fittedPanelWidth`).
#[must_use]
pub fn fitted_width(ideal: f64, width: f64, inset: f64) -> f64 {
    let ceiling = (width - 2.0 * PANEL_MARGIN).max(0.0);
    let fits_terminal_area = width - inset - 2.0 * PANEL_MARGIN;
    ideal.min(MINIMUM_PANEL_WIDTH.max(fits_terminal_area)).min(ceiling)
}

/// The tallest a palette panel grows in a frame `height` tall (upstream's `fittedPanelHeight`): what the frame leaves
/// under the panel's top less a margin, never under the minimum.
#[must_use]
pub fn palette_cap(height: f64) -> f64 {
    MINIMUM_PANEL_HEIGHT.max(height * (1.0 - PALETTE_TOP_FRACTION) - PANEL_MARGIN)
}

/// How far down a frame `height` tall a palette panel starts (upstream's `topInsetFraction`).
#[must_use]
pub fn palette_top(height: f64) -> f64 {
    height * PALETTE_TOP_FRACTION
}

/// How far right of the frame's center a palette `panel` wide sits, so it centers over the terminal area without
/// leaving the frame (upstream's `panelOffset`).
#[must_use]
pub fn palette_offset(panel: f64, width: f64, inset: f64) -> f64 {
    (inset / 2.0).min((width - panel) / 2.0).max(0.0)
}

/// The palette panel's rectangle in a `width`×`height` frame, for content wanting `natural_height` at that width.
#[must_use]
pub fn palette_rect(width: f64, height: f64, inset: f64, natural_height: f64, minimum_height: f64) -> Rect {
    let panel = palette_width(width, inset);
    let cap = palette_cap(height);
    Rect {
        x: pixels((width - panel) / 2.0 + palette_offset(panel, width, inset)),
        y: pixels(palette_top(height)),
        width: pixels(panel),
        height: pixels(natural_height.min(cap).max(minimum_height)),
    }
}

/// `value` rounded to whole pixels, saturating.
#[must_use]
pub fn pixels(value: f64) -> i32 {
    let value = value.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX));
    #[expect(clippy::cast_possible_truncation, reason = "rounded and clamped into i32")]
    let pixels = value as i32;
    pixels
}

/// `value` as GPUI pixels.
#[must_use]
pub fn logical(value: f64) -> gpui_kit::Pixels {
    #[expect(clippy::cast_possible_truncation, reason = "a window coordinate fits an f32")]
    let value = value as f32;
    px(value)
}

// MARK: - interface metrics

/// The palettes' and the switcher's text sizes and scale (upstream's `InterfaceMetrics`), from the Interface font
/// size setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Row titles and the search field.
    pub base: f64,
    /// Subtitles and badges.
    pub secondary: f64,
    /// Shortcut hints.
    pub shortcut: f64,
    /// `base` over the default, for the panels' lengths.
    pub scale: f64,
}

impl Metrics {
    /// The floor of the derived sizes (upstream's `minimumTextSize`).
    const MINIMUM_TEXT: f64 = 8.0;

    /// The metrics of the clamped Interface font size `base` (upstream's `InterfaceMetrics(fontSize:)`).
    #[must_use]
    pub fn new(base: f64) -> Self {
        let scale = base / agterm_core::settings_model::DEFAULT_INTERFACE_FONT_SIZE;
        Self {
            base,
            secondary: Self::MINIMUM_TEXT.max((10.0 * scale).round()),
            shortcut: Self::MINIMUM_TEXT.max((12.0 * scale).round()),
            scale,
        }
    }

    /// The metrics `settings` asks for.
    #[must_use]
    pub fn of(settings: &agterm_core::persist::AppSettings) -> Self {
        Self::new(settings.effective_interface_font_size())
    }

    /// A length given at the default size, at this one (upstream's `scaled`).
    #[must_use]
    pub fn scaled(&self, length: f64) -> f64 {
        (length * self.scale).round()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new(agterm_core::settings_model::DEFAULT_INTERFACE_FONT_SIZE)
    }
}

// MARK: - the look

/// The dim layer's opacity over what a dialog covers (the GTK build's `alpha(black, 0.2)`).
pub const DIM_ALPHA: f64 = 0.2;
/// The suggested action's fill (libadwaita's accent).
pub const ACCENT: Rgb = Rgb::new(0x35, 0x84, 0xe4);
/// A destructive action's color (libadwaita's destructive).
pub const DESTRUCTIVE: Rgb = Rgb::new(0xe0, 0x1b, 0x24);
/// The native panel's corner radius.
pub const PANEL_RADIUS: f32 = 12.;

/// The dim layer's color.
#[must_use]
pub fn dim() -> Hsla {
    crate::chrome::color(Rgb::BLACK, DIM_ALPHA)
}

/// The native (rounded) panel (upstream's material panel, the GTK build's popover-backed panel) in the chrome's
/// colors: the terminal background, a hairline and a shadow.
#[must_use]
pub fn panel(colors: ChromeColors) -> gpui_kit::Div {
    div()
        .bg(crate::chrome::color(colors.background, 1.0))
        .text_color(crate::chrome::color(colors.foreground, 1.0))
        .rounded(px(PANEL_RADIUS))
        .border_1()
        .border_color(crate::chrome::color(Rgb::WHITE, 0.1))
        .shadow_lg()
        .overflow_hidden()
}

/// How a native button draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonLook {
    Plain,
    /// The prominent default (libadwaita's `suggested-action`).
    Suggested,
    /// The prominent destructive default (`destructive-action`).
    Destructive,
    /// A destructive button that is not the default: red text.
    DestructiveText,
}

/// A native button's fill and text color in `colors`.
#[must_use]
pub fn button_colors(look: ButtonLook, colors: ChromeColors) -> (Hsla, Hsla) {
    let fg = colors.foreground;
    match look {
        ButtonLook::Plain => (crate::chrome::color(fg, 0.1), crate::chrome::color(fg, 1.0)),
        ButtonLook::Suggested => (crate::chrome::color(ACCENT, 1.0), crate::chrome::color(Rgb::WHITE, 1.0)),
        ButtonLook::Destructive => (
            crate::chrome::color(DESTRUCTIVE, 1.0),
            crate::chrome::color(Rgb::WHITE, 1.0),
        ),
        ButtonLook::DestructiveText => (crate::chrome::color(fg, 0.1), crate::chrome::color(DESTRUCTIVE, 1.0)),
    }
}

// MARK: - keys

/// What a key does to a dialog or a popover list (upstream's `AskKey`, plus Space for a check).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavKey {
    Forward,
    Backward,
    Activate,
    Cancel,
    Space,
    /// A letter, lower-cased.
    Letter(String),
}

/// The navigation a key press asks for (upstream's `AskKeyCatcher.key(for:)`): Tab, Right and Down forward, Shift+Tab,
/// Left and Up back, Return activates, Escape cancels, an ASCII letter is a hotkey; `None` for any other key and for
/// any key with Ctrl, Alt or Super held, which goes on to the window.
#[must_use]
pub fn nav_key(keystroke: &Keystroke) -> Option<NavKey> {
    let modifiers = &keystroke.modifiers;
    if modifiers.control || modifiers.alt || modifiers.platform {
        return None;
    }
    match keystroke.key.as_str() {
        "tab" if modifiers.shift => Some(NavKey::Backward),
        "tab" | "right" | "down" => Some(NavKey::Forward),
        "left" | "up" => Some(NavKey::Backward),
        "enter" | "kp_enter" => Some(NavKey::Activate),
        "escape" => Some(NavKey::Cancel),
        "space" => Some(NavKey::Space),
        key => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(letter), None) if letter.is_ascii_alphabetic() => {
                    Some(NavKey::Letter(letter.to_ascii_lowercase().to_string()))
                }
                _ => None,
            }
        }
    }
}

// MARK: - alerts

/// Up to this many buttons share one row; more stack full width, first on top, as an `NSAlert` with three or more
/// buttons lays them out (the `OpenCode` version chooser), so no label is squeezed into a third of the panel.
const STACK_FROM: usize = 2;

/// How an alert's button draws (libadwaita's response appearances).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Plain,
    Suggested,
    Destructive,
}

/// One button of an alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertButton {
    pub label: SharedString,
    /// Its element id (upstream's accessibility identifier); `alert-button-<index>` without one.
    pub id: Option<SharedString>,
    /// Return answers with it (the alert's default response), and the keyboard starts on it.
    pub default: bool,
    /// Escape answers with it (the close response).
    pub cancel: bool,
    pub appearance: Appearance,
}

impl AlertButton {
    #[must_use]
    pub fn plain(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            id: None,
            default: false,
            cancel: false,
            appearance: Appearance::Plain,
        }
    }

    /// The button Escape answers with.
    #[must_use]
    pub fn cancel(label: impl Into<SharedString>) -> Self {
        Self {
            cancel: true,
            ..Self::plain(label)
        }
    }

    /// Makes Return answer with it.
    #[must_use]
    pub fn as_default(mut self) -> Self {
        self.default = true;
        self
    }

    /// Gives it an element id.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = Some(id.into());
        self
    }

    #[must_use]
    pub fn suggested(mut self) -> Self {
        self.appearance = Appearance::Suggested;
        self
    }

    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.appearance = Appearance::Destructive;
        self
    }

    fn look(&self) -> ButtonLook {
        match self.appearance {
            Appearance::Plain => ButtonLook::Plain,
            Appearance::Suggested => ButtonLook::Suggested,
            Appearance::Destructive => ButtonLook::Destructive,
        }
    }
}

/// One check of an alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertCheck {
    pub label: String,
    /// Checked at first.
    pub checked: bool,
    /// Its element id (upstream's accessibility identifier); `alert-check-<index>` without one.
    pub id: Option<SharedString>,
}

impl AlertCheck {
    /// A check unchecked at first.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            checked: false,
            id: None,
        }
    }

    /// Checked at first.
    #[must_use]
    pub fn checked(mut self) -> Self {
        self.checked = true;
        self
    }

    /// Gives it an element id.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = Some(id.into());
        self
    }
}

/// What an alert shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertSpec {
    pub heading: String,
    pub body: String,
    /// The buttons left to right.
    pub buttons: Vec<AlertButton>,
    /// The checks under the body, top to bottom ("Don't ask again this session", the welcome's installs).
    pub checks: Vec<AlertCheck>,
    /// A text field under the body holding this text, all of it selected, which takes the keyboard (Rename Window).
    pub field: Option<String>,
}

impl AlertSpec {
    /// A destructive confirmation (upstream's `confirmDelete` and `confirmCloseSession` sheets, the GTK build's
    /// alerts): Cancel, the default and the close response, then `verb` drawn destructive.
    #[must_use]
    pub fn confirm(heading: impl Into<String>, body: impl Into<String>, verb: impl Into<SharedString>) -> Self {
        Self {
            heading: heading.into(),
            body: body.into(),
            buttons: vec![
                AlertButton::cancel("Cancel").as_default(),
                AlertButton::plain(verb).destructive(),
            ],
            checks: Vec::new(),
            field: None,
        }
    }

    /// An information with an OK alone.
    #[must_use]
    pub fn info(heading: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            heading: heading.into(),
            body: body.into(),
            buttons: vec![AlertButton::cancel("OK").as_default().suggested()],
            checks: Vec::new(),
            field: None,
        }
    }

    /// The button Return answers with: the default, else the last.
    #[must_use]
    pub fn default_button(&self) -> Option<usize> {
        self.buttons
            .iter()
            .position(|button| button.default)
            .or_else(|| self.buttons.len().checked_sub(1))
    }

    /// The button Escape answers with: the cancel one; none without one.
    #[must_use]
    pub fn cancel_button(&self) -> Option<usize> {
        self.buttons.iter().position(|button| button.cancel)
    }
}

/// How the user ended an alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertAnswer {
    /// The button's index in the spec.
    pub button: usize,
    /// Whether the first check was checked.
    pub checked: bool,
    /// Whether each check was checked, in the spec's order.
    pub checks: Vec<bool>,
    /// The field's text, with a field.
    pub text: Option<String>,
}

/// What runs once with the answer.
pub type AlertDone = Box<dyn FnOnce(AlertAnswer, &mut Window, &mut App)>;

/// One alert (module docs).
pub struct Alert {
    spec: AlertSpec,
    window: AnyWindowHandle,
    focus: FocusHandle,
    field: Option<Entity<InputState>>,
    /// The keyboard's place: the checks, then the buttons.
    highlighted: usize,
    checked: Vec<bool>,
    done: Option<AlertDone>,
    _field_events: Option<Subscription>,
}

impl Alert {
    fn new(spec: AlertSpec, done: AlertDone, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let offset = spec.checks.len();
        let checked = spec.checks.iter().map(|check| check.checked).collect();
        let highlighted = spec.default_button().map_or(0, |index| index + offset);
        let field = spec.field.clone().map(|text| {
            let input = cx.new(|cx| InputState::new(window, cx).default_value(text));
            input.update(cx, |input, cx| input.select_all(window, cx));
            input
        });
        let field_events = field.as_ref().map(|input| {
            cx.subscribe_in(input, window, |alert, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. })
                    && let Some(button) = alert.spec.default_button()
                {
                    alert.answer(button, window, cx);
                }
            })
        });
        Self {
            spec,
            window: window.window_handle(),
            focus: cx.focus_handle(),
            field,
            highlighted,
            checked,
            done: Some(done),
            _field_events: field_events,
        }
    }

    /// What the alert shows.
    #[must_use]
    pub fn spec(&self) -> &AlertSpec {
        &self.spec
    }

    /// What takes the keyboard while the alert is in front: its field, else the alert.
    #[must_use]
    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.field {
            Some(field) => field.read(cx).focus_handle(cx),
            None => self.focus.clone(),
        }
    }

    /// Whether the alert, or its field, holds `window`'s keyboard.
    #[must_use]
    pub fn holds_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx) || self.focus_handle(cx).is_focused(window)
    }

    /// Whether the first check is checked.
    #[must_use]
    pub fn checked(&self) -> bool {
        self.checked.first().copied().unwrap_or(false)
    }

    /// Whether each check is checked.
    #[must_use]
    pub fn checks(&self) -> &[bool] {
        &self.checked
    }

    /// Checks or unchecks the first check.
    pub fn set_checked(&mut self, checked: bool, cx: &mut Context<Self>) {
        if let Some(first) = self.checked.first_mut() {
            *first = checked;
        }
        cx.notify();
    }

    /// The highlighted button's index; `None` while a check is highlighted.
    #[must_use]
    pub fn highlighted_button(&self) -> Option<usize> {
        self.highlighted.checked_sub(self.offset())
    }

    fn offset(&self) -> usize {
        self.spec.checks.len()
    }

    fn stops(&self) -> usize {
        self.spec.buttons.len() + self.offset()
    }

    /// Return and Escape answer even from the field, which would otherwise keep them.
    fn capture_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.field.is_none() {
            return;
        }
        match nav_key(&event.keystroke) {
            Some(NavKey::Activate) => {
                if let Some(button) = self.spec.default_button() {
                    cx.stop_propagation();
                    self.answer(button, window, cx);
                }
            }
            Some(NavKey::Cancel) => {
                if let Some(button) = self.spec.cancel_button() {
                    cx.stop_propagation();
                    self.answer(button, window, cx);
                }
            }
            _ => {}
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.field.is_some() {
            // the field types every other key (Return and Escape were taken on the way down)
            return;
        }
        cx.stop_propagation();
        let Some(key) = nav_key(&event.keystroke) else {
            return;
        };
        let stops = self.stops().max(1);
        match key {
            NavKey::Forward => self.highlighted = (self.highlighted + 1) % stops,
            NavKey::Backward => self.highlighted = (self.highlighted + stops - 1) % stops,
            NavKey::Activate | NavKey::Space => match self.highlighted_button() {
                Some(button) => self.answer(button, window, cx),
                None => self.toggle(self.highlighted),
            },
            NavKey::Cancel => {
                if let Some(button) = self.spec.cancel_button() {
                    self.answer(button, window, cx);
                }
            }
            NavKey::Letter(_) => {}
        }
        cx.notify();
    }

    /// Ends the alert with `button`: it leaves the window's queue (the next one takes the keyboard) and the answer
    /// runs. A second answer does nothing.
    pub fn answer(&mut self, button: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(done) = self.done.take() else {
            return;
        };
        let answer = AlertAnswer {
            button,
            checked: self.checked(),
            checks: self.checked.clone(),
            text: self.field.as_ref().map(|field| field.read(cx).value().to_string()),
        };
        let id = cx.entity_id();
        let handle = self.window;
        let next = cx.update_default_global::<Alerts, _>(|alerts, _| alerts.remove(handle, id));
        if let Some(next) = next {
            let focus = next.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
        window.refresh();
        done(answer, window, cx);
    }

    fn toggle(&mut self, index: usize) {
        if let Some(check) = self.checked.get_mut(index) {
            *check = !*check;
        }
    }

    fn render_check(&self, index: usize, colors: ChromeColors, cx: &mut Context<Self>) -> impl IntoElement {
        let spec = &self.spec.checks[index];
        let checked = self.checked.get(index).copied().unwrap_or(false);
        let ink = crate::chrome::color(colors.foreground, 1.0);
        let ring = self.highlighted == index;
        let id: gpui_kit::ElementId = match &spec.id {
            Some(id) => id.clone().into(),
            None => ("alert-check", index).into(),
        };
        let fill = if checked {
            crate::chrome::color(ACCENT, 1.0)
        } else {
            crate::chrome::color(colors.foreground, 0.1)
        };
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .child(
                div()
                    .size(px(16.))
                    .rounded(px(4.))
                    .bg(fill)
                    .border_1()
                    .border_color(if ring {
                        crate::chrome::color(ACCENT, 1.0)
                    } else {
                        crate::chrome::color(colors.foreground, 0.3)
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.))
                    .text_color(crate::chrome::color(Rgb::WHITE, 1.0))
                    .when(checked, |mark| mark.child("✓")),
            )
            .child(div().text_color(ink).child(SharedString::from(spec.label.clone())))
            .on_click(cx.listener(move |alert, _, _, cx| {
                alert.toggle(index);
                alert.highlighted = index;
                cx.notify();
            }))
    }

    /// Button `index`, sharing its row with the others, or `stacked` full width under the one before it.
    fn render_button(
        &self,
        index: usize,
        stacked: bool,
        colors: ChromeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let button = &self.spec.buttons[index];
        let highlighted = self.highlighted_button() == Some(index);
        let (fill, ink) = button_colors(button.look(), colors);
        let (id, selector): (gpui_kit::ElementId, String) = match &button.id {
            Some(id) => (id.clone().into(), id.to_string()),
            None => (("alert-button", index).into(), format!("alert-button-{index}")),
        };
        div()
            .id(id)
            .debug_selector(move || selector)
            .when(!stacked, gpui_kit::Styled::flex_1)
            .flex()
            .justify_center()
            .px_3()
            .py(px(7.))
            .rounded(px(8.))
            .bg(fill)
            .text_color(ink)
            .font_weight(FontWeight::MEDIUM)
            .border_2()
            .border_color(if highlighted && self.field.is_none() {
                crate::chrome::color(ACCENT, 0.6)
            } else {
                gpui_kit::transparent_black()
            })
            .cursor_pointer()
            .child(button.label.clone())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |alert, _, window, cx| alert.answer(index, window, cx)))
    }
}

impl Render for Alert {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = crate::chrome::current(cx);
        let dim_ink = crate::chrome::color(colors.foreground, crate::chrome::DIM_ALPHA);
        let checks: Vec<AnyElement> = (0..self.spec.checks.len())
            .map(|index| self.render_check(index, colors, cx).into_any_element())
            .collect();
        let field = self
            .field
            .as_ref()
            .map(|field| Input::new(field).id("alert-field").into_any_element());
        let stacked = self.spec.buttons.len() > STACK_FROM;
        let buttons: Vec<AnyElement> = (0..self.spec.buttons.len())
            .map(|index| self.render_button(index, stacked, colors, cx).into_any_element())
            .collect();
        panel(colors)
            .id("alert-dialog")
            .key_context("AgtermAlert")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::capture_key))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .w(px(372.))
            .max_w_full()
            .max_h_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(15.))
                    .font_weight(FontWeight::BOLD)
                    .child(SharedString::from(self.spec.heading.clone())),
            )
            .when(!self.spec.body.is_empty(), |content| {
                // a body taller than the window (the hooks install's report) scrolls; the buttons stay in view
                content.child(
                    div()
                        .id("alert-body")
                        .debug_selector(|| "alert-body".to_owned())
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .text_size(px(13.))
                        .text_color(dim_ink)
                        .child(SharedString::from(self.spec.body.clone())),
                )
            })
            .children(field)
            .children(checks)
            .child(
                div()
                    .flex_shrink_0()
                    .mt_2()
                    .flex()
                    .when(stacked, gpui_kit::Styled::flex_col)
                    .when(!stacked, gpui_kit::Styled::flex_row)
                    .gap_2()
                    .children(buttons),
            )
    }
}

/// The alerts of every window, front first (module docs).
#[derive(Default)]
pub struct Alerts {
    queues: HashMap<AnyWindowHandle, Vec<Entity<Alert>>>,
}

impl Global for Alerts {}

impl Alerts {
    /// Drops alert `id` from `window`'s queue; the alert now in front.
    fn remove(&mut self, window: AnyWindowHandle, id: gpui_kit::EntityId) -> Option<Entity<Alert>> {
        let queue = self.queues.get_mut(&window)?;
        queue.retain(|alert| alert.entity_id() != id);
        let front = queue.first().cloned();
        if queue.is_empty() {
            self.queues.remove(&window);
        }
        front
    }
}

/// Opens an alert over `window`, behind any it already shows; `done` runs once with the answer. The alert in front
/// takes the keyboard.
pub fn open_alert(
    window: &mut Window,
    spec: AlertSpec,
    done: impl FnOnce(AlertAnswer, &mut Window, &mut App) + 'static,
    cx: &mut App,
) -> Entity<Alert> {
    let handle = window.window_handle();
    let alert = cx.new(|cx| Alert::new(spec, Box::new(done), window, cx));
    let front = cx.update_default_global::<Alerts, _>(|alerts, _| {
        let queue = alerts.queues.entry(handle).or_default();
        queue.push(alert.clone());
        queue.len() == 1
    });
    if front {
        let focus = alert.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    }
    window.refresh();
    alert
}

/// Answers every alert `window` shows or queues with its cancel button, as a dialog closed with its window responds
/// with its close response (the GTK build's `adw::AlertDialog`): the window is going, and what waits on an answer (a
/// clipboard prompt's program) is denied rather than left waiting. An alert without a cancel button goes unanswered.
pub fn dismiss_alerts(window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    if !cx
        .try_global::<Alerts>()
        .is_some_and(|alerts| alerts.queues.contains_key(&handle))
    {
        return;
    }
    let queue = cx
        .update_default_global::<Alerts, _>(|alerts, _| alerts.queues.remove(&handle))
        .unwrap_or_default();
    for alert in queue {
        if let Some(cancel) = alert.read(cx).spec.cancel_button() {
            alert.update(cx, |alert, cx| alert.answer(cancel, window, cx));
        }
    }
}

/// The alert `window` shows in front.
#[must_use]
pub fn front_alert(window: AnyWindowHandle, cx: &App) -> Option<Entity<Alert>> {
    cx.try_global::<Alerts>()
        .and_then(|alerts| alerts.queues.get(&window))
        .and_then(|queue| queue.first().cloned())
}

/// Whether `window` shows an alert.
#[must_use]
pub fn alert_open(window: AnyWindowHandle, cx: &App) -> bool {
    front_alert(window, cx).is_some()
}

/// The front alert of `window` over a dimmed window that takes every click; `None` without one.
#[must_use]
pub fn alert_layer(window: AnyWindowHandle, cx: &App) -> Option<AnyElement> {
    let alert = front_alert(window, cx)?;
    Some(
        div()
            .id("alert-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(dim())
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .child(alert)
            .into_any_element(),
    )
}

/// Keeps the keyboard on `window`'s front alert (a menu that closed after its item opened one may have taken it back).
pub fn refocus_alert(window: &mut Window, cx: &mut App) {
    if let Some(alert) = front_alert(window.window_handle(), cx)
        && !alert.read(cx).holds_keyboard(window, cx)
    {
        let focus = alert.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    }
}

/// Answers `window`'s front alert with the button labelled `label`, as a click on it; whether one was there.
pub fn answer_front_alert(window: &mut Window, label: &str, cx: &mut App) -> bool {
    let Some(alert) = front_alert(window.window_handle(), cx) else {
        return false;
    };
    let Some(index) = alert
        .read(cx)
        .spec
        .buttons
        .iter()
        .position(|button| button.label.as_ref() == label)
    else {
        return false;
    };
    alert.update(cx, |alert, cx| alert.answer(index, window, cx));
    true
}

#[cfg(test)]
pub(crate) mod tests;
