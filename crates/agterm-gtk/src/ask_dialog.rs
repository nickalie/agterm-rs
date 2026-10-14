//! The question dialog (`ask.open`), after upstream's `Views/AskDialogView.swift` and `ControlServer+Ask.swift`.
//!
//! One dialog view serves both owners:
//! - a terminal ask waits on its session and is drawn inside the session's page, in a layer added once to the page's
//!   [`crate::split::PaneStack`] above every pane and the HUD ([`SessionAskLayer`], [`sync_page`]), covering the whole
//!   page or the pane it was opened over, in the terminal's font and colors, square, with plain block buttons;
//! - a GUI ask holds its window's modal slot and is drawn over the window ([`WindowAsk`]), covering the terminal area
//!   or the session or pane it is anchored to, as a native rounded panel with native buttons, while the rest of the
//!   window takes no clicks.
//!
//! Either way a dim layer covers the anchor, the panel is centered in it ([`crate::modal::ModalFrame`]): the title,
//! the message and a row of buttons that falls back to a column when the row does not fit, aligned as asked. The
//! keyboard follows [`AskNavigation`]: Tab, Right and Down move forward, Shift+Tab, Left and Up back, Return answers
//! with the highlighted button, a button's hotkey letter answers with it, Escape dismisses (`escaped`); a key with
//! Ctrl, Alt or Super goes on to the window. Answers go to the core as main-loop tasks; a stale id is ignored there.
//!
//! The dialog takes the keyboard when it is allowed to (upstream's `wantsFocus`): a GUI ask as it opens, a terminal
//! ask while its session is shown and its window holds no picker or GUI ask, the quick terminal is hidden and, for a
//! pane ask, its pane is the focused one. The deck gives it the keyboard in place of the terminal it covers and gives
//! the terminal the keyboard back once the dialog ends.

use std::cell::RefCell;
use std::rc::Rc;

use agterm_core::Core;
use agterm_core::chrome::FALLBACK_BACKGROUND;
use agterm_core::model::{AskAlignment, AskNavigation, AskStyle, OverlayPane, PendingAsk, Session};
use agterm_ghostty::Color;
use agterm_proto::{AskButton, ControlAskOutcome, ControlAskResult};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};
use uuid::Uuid;

use crate::modal::{self, AnchorFn, ModalFrame, PanelRule};
use crate::split::{AskScope, PaneStack, PaneView, Rect, Slot};
use crate::state::{self, AppState, Poster};

/// The GUI dialog's spacing unit (upstream's `cell` for `.gui`).
const GUI_CELL: f64 = 8.0;
/// The font size when neither the settings nor the terminal config name one.
const DEFAULT_FONT_SIZE: f64 = 13.0;
/// libghostty's default colors, for a terminal config that names none; the background is the window chrome's fallback.
const DEFAULT_FOREGROUND: Color = Color {
    r: 0xff,
    g: 0xff,
    b: 0xff,
};
const DEFAULT_BACKGROUND: Color = Color {
    r: FALLBACK_BACKGROUND.r,
    g: FALLBACK_BACKGROUND.g,
    b: FALLBACK_BACKGROUND.b,
};

/// Style classes of the dialog's parts.
const TERMINAL_CLASS: &str = "agterm-ask-terminal";
const TITLE_CLASS: &str = "agterm-ask-title";
const MESSAGE_CLASS: &str = "agterm-ask-message";
const BLOCK_CLASS: &str = "agterm-ask-block";
const HIGHLIGHTED_CLASS: &str = "agterm-ask-highlighted";
const DESTRUCTIVE_CLASS: &str = "agterm-ask-destructive";
/// Every class [`button_classes`] manages.
const BUTTON_STATE_CLASSES: [&str; 4] = [
    "suggested-action",
    "destructive-action",
    HIGHLIGHTED_CLASS,
    DESTRUCTIVE_CLASS,
];

// MARK: - the look

/// The terminal's font and colors the terminal style draws in (upstream's `askFont`, `chromeText` and
/// `terminalColor`): the settings' font family and size, else the terminal config's size in the default monospace face,
/// and the terminal config's foreground and background.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    pub family: Option<String>,
    /// Points.
    pub size: f64,
    pub foreground: Color,
    pub background: Color,
}

impl Look {
    /// The look the settings and the terminal config give now.
    #[must_use]
    pub fn current(core: &Core) -> Self {
        let settings = core.settings();
        Self {
            family: settings.font_family.clone().filter(|family| !family.trim().is_empty()),
            size: settings
                .font_size
                .filter(|size| *size > 0.0)
                .or_else(crate::ghostty_app::base_font_size)
                .unwrap_or(DEFAULT_FONT_SIZE),
            foreground: crate::ghostty_app::foreground_color().unwrap_or(DEFAULT_FOREGROUND),
            background: crate::ghostty_app::background_color().unwrap_or(DEFAULT_BACKGROUND),
        }
    }

    /// The font size in pixels (a point is 4/3 of a pixel, as GTK and libghostty count it on Linux).
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

fn rgba(color: Color, alpha: f64) -> String {
    format!("rgba({},{},{},{alpha})", color.r, color.g, color.b)
}

/// The terminal style's stylesheet for `look` (upstream's `AskPanelStyle` and `terminalButton`): the terminal's
/// background with a thin border at 30% foreground, square; the message at 70% foreground; plain block buttons at 12%
/// foreground, the highlighted one inverted.
#[must_use]
pub fn terminal_css(look: &Look) -> String {
    let family: String = look
        .family
        .as_deref()
        .unwrap_or("monospace")
        .chars()
        .filter(|ch| !matches!(ch, '"' | '\\' | '\n' | ';' | '{' | '}'))
        .collect();
    let (fg, bg) = (look.foreground, look.background);
    let cell = look.terminal_cell();
    format!(
        ".{TERMINAL_CLASS} {{ background-color: {bg}; color: {fg}; border: 1px solid {border}; border-radius: 0; \
         font-family: \"{family}\", monospace; font-size: {size}pt; }}\n\
         .{TERMINAL_CLASS} .{TITLE_CLASS} {{ font-weight: bold; }}\n\
         .{TERMINAL_CLASS} .{MESSAGE_CLASS} {{ color: {dim}; }}\n\
         .{TERMINAL_CLASS} button.{BLOCK_CLASS} {{ background: {wash}; color: {fg}; border-radius: 0; \
         box-shadow: none; min-height: 0; min-width: 0; padding: {vertical:.1}px {horizontal:.1}px; \
         font-weight: normal; }}\n\
         .{TERMINAL_CLASS} button.{BLOCK_CLASS}.{HIGHLIGHTED_CLASS} {{ background: {fg}; color: {bg}; }}\n",
        bg = rgba(bg, 1.0),
        fg = rgba(fg, 1.0),
        border = rgba(fg, 0.3),
        dim = rgba(fg, 0.7),
        wash = rgba(fg, 0.12),
        size = look.size,
        vertical = cell * 0.4,
        horizontal = cell * 0.5,
    )
}

thread_local! {
    /// The terminal style's provider and the stylesheet it holds.
    static TERMINAL_CSS: RefCell<Option<(gtk::CssProvider, String)>> = const { RefCell::new(None) };
}

/// Installs or updates the terminal style's stylesheet for `look`.
fn install_terminal_css(look: &Look) {
    let css = terminal_css(look);
    TERMINAL_CSS.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some((provider, current)) = slot.as_mut() {
            if *current != css {
                provider.load_from_string(&css);
                *current = css;
            }
            return;
        }
        let Some(display) = gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
        *slot = Some((provider, css));
    });
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
pub fn ask_key(keyval: gdk::Key, state: gdk::ModifierType) -> Option<AskKey> {
    let blocked = gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK
        | gdk::ModifierType::META_MASK;
    if state.intersects(blocked) {
        return None;
    }
    let backward_tab = keyval == gdk::Key::ISO_Left_Tab || state.contains(gdk::ModifierType::SHIFT_MASK);
    match keyval {
        gdk::Key::Tab | gdk::Key::KP_Tab | gdk::Key::ISO_Left_Tab if backward_tab => Some(AskKey::Backward),
        gdk::Key::Tab
        | gdk::Key::KP_Tab
        | gdk::Key::Right
        | gdk::Key::KP_Right
        | gdk::Key::Down
        | gdk::Key::KP_Down => Some(AskKey::Forward),
        gdk::Key::Left | gdk::Key::KP_Left | gdk::Key::Up | gdk::Key::KP_Up => Some(AskKey::Backward),
        gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => Some(AskKey::Activate),
        gdk::Key::Escape => Some(AskKey::Cancel),
        _ => keyval
            .to_unicode()
            .filter(char::is_ascii_alphabetic)
            .map(|letter| AskKey::Hotkey(letter.to_ascii_lowercase().to_string())),
    }
}

/// A button's label as Pango markup (upstream's `buttonLabel`): the hotkey's first case-insensitive occurrence
/// underlined, else ` (X)` appended with the letter underlined; a terminal-style destructive button is prefixed with
/// `! ` and bold.
#[must_use]
pub fn button_markup(button: &AskButton, terminal_destructive: bool) -> String {
    let escape = |text: &str| glib::markup_escape_text(text).to_string();
    let label = &button.label;
    let mut markup = match button.hotkey.as_deref() {
        Some(hotkey) => {
            let wanted = hotkey.to_lowercase();
            let found = label
                .char_indices()
                .find(|(_, ch)| ch.to_lowercase().to_string() == wanted);
            match found {
                Some((at, ch)) => format!(
                    "{}<u>{}</u>{}",
                    escape(&label[..at]),
                    escape(&ch.to_string()),
                    escape(&label[at + ch.len_utf8()..])
                ),
                None => format!("{} (<u>{}</u>)", escape(label), escape(&hotkey.to_uppercase())),
            }
        }
        None => escape(label),
    };
    if terminal_destructive {
        markup = format!("<b>! {markup}</b>");
    }
    markup
}

/// The style classes of a button (upstream's `guiButton`/`terminalButton`): in the GUI style the highlighted button is
/// the prominent one (destructive red when it is the destructive button) and a destructive one is otherwise red text;
/// in the terminal style the highlighted block is inverted.
#[must_use]
pub fn button_classes(style: AskStyle, highlighted: bool, destructive: bool) -> Vec<&'static str> {
    match style {
        AskStyle::Gui => match (highlighted, destructive) {
            (true, true) => vec!["destructive-action"],
            (true, false) => vec!["suggested-action"],
            (false, true) => vec![DESTRUCTIVE_CLASS],
            (false, false) => Vec::new(),
        },
        AskStyle::Terminal if highlighted => vec![HIGHLIGHTED_CLASS],
        AskStyle::Terminal => Vec::new(),
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
pub fn row_width(widest: i32, count: usize, spacing: i32) -> i32 {
    let count = i32::try_from(count).unwrap_or(i32::MAX);
    widest.saturating_mul(count) + spacing.saturating_mul((count - 1).max(0))
}

/// A row while it fits `available`, else a column (upstream's `ViewThatFits(in: .horizontal)`).
#[must_use]
pub fn strip_axis(widest: i32, count: usize, spacing: i32, available: i32) -> StripAxis {
    if row_width(widest, count, spacing) <= available {
        StripAxis::Row
    } else {
        StripAxis::Column
    }
}

/// Where a block `used` wide starts in `available` under `align`.
#[must_use]
pub fn aligned_start(align: AskAlignment, available: i32, used: i32) -> i32 {
    let free = (available - used).max(0);
    match align {
        AskAlignment::Left => 0,
        AskAlignment::Center => free / 2,
        AskAlignment::Right => free,
    }
}

/// What [`ButtonStrip`] lays out by.
#[derive(Debug, Clone, Copy, Default)]
struct StripSpec {
    spacing: i32,
    align: AskAlignment,
}

/// The children's widest minimum and natural widths, and their count.
fn child_widths(strip: &gtk::Widget) -> (i32, i32, usize) {
    let mut child = strip.first_child();
    let (mut minimum, mut natural, mut count) = (0, 0, 0);
    while let Some(widget) = child {
        let (min, nat, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
        minimum = minimum.max(min);
        natural = natural.max(nat);
        count += 1;
        child = widget.next_sibling();
    }
    (minimum, natural, count)
}

/// The children's heights at `width`: their minimum and natural heights summed (a column) or maxed (a row).
fn child_heights(strip: &gtk::Widget, width: i32, axis: StripAxis) -> (i32, i32) {
    let mut child = strip.first_child();
    let (mut minimum, mut natural) = (0, 0);
    while let Some(widget) = child {
        let (min, nat, _, _) = widget.measure(gtk::Orientation::Vertical, width);
        match axis {
            StripAxis::Row => (minimum, natural) = (minimum.max(min), natural.max(nat)),
            StripAxis::Column => (minimum, natural) = (minimum + min, natural + nat),
        }
        child = widget.next_sibling();
    }
    (minimum, natural)
}

/// The axis and the shared button width at `available` (any width when negative).
fn strip_plan(spec: StripSpec, strip: &gtk::Widget, available: i32) -> (StripAxis, i32, usize) {
    let (minimum, natural, count) = child_widths(strip);
    if available < 0 {
        return (StripAxis::Row, natural, count);
    }
    match strip_axis(natural, count, spec.spacing, available) {
        StripAxis::Row => (StripAxis::Row, natural, count),
        StripAxis::Column => (StripAxis::Column, natural.min(available).max(minimum), count),
    }
}

fn measure_strip(spec: StripSpec, strip: &gtk::Widget, orientation: gtk::Orientation, for_size: i32) -> (i32, i32) {
    if orientation == gtk::Orientation::Horizontal {
        let (minimum, natural, count) = child_widths(strip);
        return (minimum, row_width(natural, count, spec.spacing));
    }
    let (axis, width, count) = strip_plan(spec, strip, for_size);
    let (minimum, natural) = child_heights(strip, width, axis);
    let gaps = match axis {
        StripAxis::Row => 0,
        StripAxis::Column => spec.spacing * i32::try_from(count.saturating_sub(1)).unwrap_or(0),
    };
    (minimum + gaps, natural + gaps)
}

fn allocate_strip(spec: StripSpec, strip: &gtk::Widget, width: i32, height: i32) {
    let (axis, button, count) = strip_plan(spec, strip, width);
    let used = match axis {
        StripAxis::Row => row_width(button, count, spec.spacing),
        StripAxis::Column => button,
    };
    let start = aligned_start(spec.align, width, used);
    let mut offset = 0;
    let mut child = strip.first_child();
    while let Some(widget) = child {
        let (_, natural, _, _) = widget.measure(gtk::Orientation::Vertical, button);
        let rect = match axis {
            StripAxis::Row => Rect {
                x: start + offset,
                y: 0,
                width: button,
                height,
            },
            StripAxis::Column => Rect {
                x: start,
                y: offset,
                width: button,
                height: natural,
            },
        };
        widget.size_allocate(&gtk::Allocation::new(rect.x, rect.y, rect.width, rect.height), -1);
        offset += match axis {
            StripAxis::Row => button,
            StripAxis::Column => natural,
        } + spec.spacing;
        child = widget.next_sibling();
    }
}

mod strip_imp {
    use std::cell::Cell;

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    use super::StripSpec;

    #[derive(Default)]
    pub struct ButtonStrip {
        pub(super) spec: Cell<StripSpec>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ButtonStrip {
        const NAME: &'static str = "AgtermAskButtonStrip";
        type Type = super::ButtonStrip;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ButtonStrip {
        fn dispose(&self) {
            let widget = self.obj();
            while let Some(child) = widget.first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for ButtonStrip {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (minimum, natural) =
                super::measure_strip(self.spec.get(), self.obj().upcast_ref(), orientation, for_size);
            (minimum, natural, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            super::allocate_strip(self.spec.get(), self.obj().upcast_ref(), width, height);
        }
    }
}

glib::wrapper! {
    /// The dialog's buttons: a row of equal-width buttons, or a column where the row does not fit (upstream's
    /// `AskButtonLayout` under `ViewThatFits`).
    pub struct ButtonStrip(ObjectSubclass<strip_imp::ButtonStrip>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ButtonStrip {
    fn new(spacing: i32, align: AskAlignment) -> Self {
        let strip: Self = glib::Object::builder().property("hexpand", true).build();
        strip.imp().spec.set(StripSpec { spacing, align });
        strip
    }
}

// MARK: - the dialog

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

/// What the dialog's handlers share.
struct DialogState {
    navigation: AskNavigation,
    style: AskStyle,
    destructive: Option<usize>,
    buttons: Vec<gtk::Button>,
    scroller: gtk::ScrolledWindow,
    /// An answer went out: later keys and clicks do nothing.
    done: bool,
    answer: Box<dyn Fn(Answer)>,
}

impl DialogState {
    fn finish(&mut self, answer: Answer) {
        if !self.done {
            self.done = true;
            (self.answer)(answer);
        }
    }

    fn handle(&mut self, key: AskKey) {
        match key {
            AskKey::Forward => self.navigation.move_forward(),
            AskKey::Backward => self.navigation.move_backward(),
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
        self.refresh();
    }

    /// Styles every button by whether it is highlighted, and keeps the highlighted one scrolled into view.
    fn refresh(&self) {
        let highlighted = self.navigation.highlighted();
        for (index, button) in self.buttons.iter().enumerate() {
            for class in BUTTON_STATE_CLASSES {
                button.remove_css_class(class);
            }
            let classes = button_classes(self.style, highlighted == Some(index), self.destructive == Some(index));
            for class in classes {
                button.add_css_class(class);
            }
        }
        if let Some(button) = highlighted.and_then(|index| self.buttons.get(index)) {
            modal::reveal(&self.scroller, button.upcast_ref());
        }
    }
}

/// One open dialog: its frame and the ask it shows.
pub struct Dialog {
    id: String,
    frame: ModalFrame,
    state: Rc<RefCell<DialogState>>,
}

impl Dialog {
    /// The dialog for `ask` in `look`; `answer` receives the user's answer once, `focus` a click outside the buttons
    /// (after the dialog took the keyboard back).
    #[must_use]
    pub fn new(ask: &PendingAsk, look: &Look, answer: impl Fn(Answer) + 'static, focus: impl Fn() + 'static) -> Self {
        let cell = look.cell(ask.style);
        if ask.style == AskStyle::Terminal {
            install_terminal_css(look);
        }
        let (panel, scroller, buttons) = build_panel(ask, cell);
        let dim = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dim.add_css_class(modal::DIM_CLASS);
        let rule = PanelRule::Ask {
            width_percent: ask.width,
            terminal_cell: look.terminal_cell(),
            cell,
        };
        let frame = ModalFrame::new(&dim, &panel, rule);
        frame.set_focusable(true);
        let state = Rc::new(RefCell::new(DialogState {
            navigation: AskNavigation::new(ask),
            style: ask.style,
            destructive: ask
                .destructive_id
                .as_ref()
                .and_then(|id| ask.buttons.iter().position(|button| &button.id == id)),
            buttons,
            scroller,
            done: false,
            answer: Box::new(answer),
        }));
        connect_dialog(&frame, &state, focus);
        state.borrow().refresh();
        Self {
            id: ask.id.clone(),
            frame,
            state,
        }
    }

    /// The ask shown.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The dialog's widget, which takes the keyboard.
    #[must_use]
    pub fn widget(&self) -> &ModalFrame {
        &self.frame
    }

    /// Gives the dialog the keyboard, its highlighted button scrolled into view; whether it took it. The dialog owns
    /// the state its handlers reach weakly, so a dialog taken down leaves nothing behind.
    pub fn grab_focus(&self) -> bool {
        let took = self.frame.grab_focus();
        if took && let Ok(state) = self.state.try_borrow() {
            state.refresh();
        }
        took
    }
}

/// The keys, the buttons' clicks, and a click elsewhere on the dialog.
fn connect_dialog(frame: &ModalFrame, state: &Rc<RefCell<DialogState>>, focus: impl Fn() + 'static) {
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(state);
    keys.connect_key_pressed(move |_, keyval, _, modifiers| {
        let Some(key) = ask_key(keyval, modifiers) else {
            return if modifiers.intersects(
                gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK,
            ) {
                glib::Propagation::Proceed
            } else {
                glib::Propagation::Stop
            };
        };
        if let Some(state) = weak.upgrade() {
            state.borrow_mut().handle(key);
        }
        glib::Propagation::Stop
    });
    frame.add_controller(keys);
    for (index, button) in state.borrow().buttons.iter().enumerate() {
        let weak = Rc::downgrade(state);
        button.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                state.borrow_mut().finish(Answer::Button(index));
            }
        });
    }
    let click = gtk::GestureClick::new();
    let weak = frame.downgrade();
    click.connect_pressed(move |_, _, _, _| {
        if let Some(frame) = weak.upgrade() {
            frame.grab_focus();
        }
        focus();
    });
    frame.add_controller(click);
}

/// A wrapping label.
fn text_label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .natural_wrap_mode(gtk::NaturalWrapMode::None)
        .xalign(0.0)
        .halign(gtk::Align::Fill)
        .build();
    label.add_css_class(class);
    label
}

/// The panel: the title, the message and the buttons, padded by two cells and spaced by one and a half, scrolling when
/// taller than its anchor allows (upstream's `panel` and `content`).
fn build_panel(ask: &PendingAsk, cell: f64) -> (gtk::Box, gtk::ScrolledWindow, Vec<gtk::Button>) {
    let gui = ask.style == AskStyle::Gui;
    let content = gtk::Box::new(gtk::Orientation::Vertical, modal::pixels(cell * 1.5));
    let padding = modal::pixels(cell * 2.0);
    content.set_margin_top(padding);
    content.set_margin_bottom(padding);
    content.set_margin_start(padding);
    content.set_margin_end(padding);
    let title = text_label(&ask.title, TITLE_CLASS);
    if gui {
        title.add_css_class("heading");
    }
    content.append(&title);
    if let Some(message) = ask.message.as_deref().filter(|message| !message.is_empty()) {
        let label = text_label(message, MESSAGE_CLASS);
        if gui {
            label.add_css_class("dim-label");
        }
        content.append(&label);
    }
    let strip = ButtonStrip::new(modal::pixels(cell), ask.align);
    let buttons: Vec<gtk::Button> = ask
        .buttons
        .iter()
        .map(|button| {
            let destructive = ask.destructive_id.as_ref() == Some(&button.id);
            let widget = ask_button(button, !gui && destructive, gui);
            widget.set_parent(&strip);
            widget
        })
        .collect();
    content.append(&strip);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .propagate_natural_width(true)
        .child(&content)
        .build();
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.set_overflow(gtk::Overflow::Hidden);
    panel.add_css_class(if gui { modal::PANEL_CLASS } else { TERMINAL_CLASS });
    panel.append(&scroller);
    (panel, scroller, buttons)
}

/// One button: its label wrapping, the hotkey underlined; it takes no focus, the dialog keeps the keyboard.
fn ask_button(button: &AskButton, terminal_destructive: bool, gui: bool) -> gtk::Button {
    let label = gtk::Label::builder()
        .use_markup(true)
        .label(button_markup(button, terminal_destructive))
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .natural_wrap_mode(gtk::NaturalWrapMode::None)
        .justify(gtk::Justification::Center)
        .build();
    let widget = gtk::Button::builder()
        .child(&label)
        .focusable(false)
        .focus_on_click(false)
        .build();
    if !gui {
        widget.add_css_class(BLOCK_CLASS);
    }
    widget.update_property(&[gtk::accessible::Property::Label(&button.label)]);
    widget
}

// MARK: - the terminal ask on its page

/// What a page's ask needs from its window.
pub struct AskPage<'a> {
    /// The page shows its session with nothing over it: selected, no zoom, no dashboard.
    pub shown: bool,
    /// Something else holds the keyboard: the window's picker or GUI ask, or the quick terminal.
    pub focus_blocked: bool,
    pub look: &'a Look,
    pub poster: &'a Poster,
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

/// A page's layer for its session's terminal ask, a permanent child of the page's stack.
pub struct SessionAskLayer {
    layer: gtk::Box,
    dialog: Option<Dialog>,
    wants_focus: bool,
}

impl SessionAskLayer {
    #[must_use]
    pub fn new() -> Self {
        let layer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        layer.set_visible(false);
        Self {
            layer,
            dialog: None,
            wants_focus: false,
        }
    }

    #[must_use]
    pub fn widget(&self) -> &gtk::Box {
        &self.layer
    }

    /// The dialog that should hold the keyboard in place of the terminal it covers, keyed by its ask.
    #[must_use]
    pub fn focus_target(&self) -> Option<(String, gtk::Widget)> {
        let dialog = self.dialog.as_ref().filter(|_| self.wants_focus)?;
        Some((format!("ask:{}", dialog.id()), dialog.widget().clone().upcast()))
    }

    /// Takes the dialog down.
    pub fn clear(&mut self) {
        if let Some(dialog) = self.dialog.take() {
            self.layer.remove(dialog.widget());
        }
        self.layer.set_visible(false);
        self.wants_focus = false;
    }

    fn mount(&mut self, dialog: Dialog) {
        self.clear();
        self.layer.append(dialog.widget());
        self.dialog = Some(dialog);
    }
}

impl Default for SessionAskLayer {
    fn default() -> Self {
        Self::new()
    }
}

/// Brings the page's ask layer in line with `session`'s terminal ask: a dialog for the ask while it shows (a new one
/// per ask, as upstream's view is keyed by its id), taken down otherwise.
pub fn sync_page(panes: &mut PaneView, session: &Session, page: &AskPage<'_>) {
    let scope = shown_scope(session, page.shown);
    let ask = session.ask().filter(|_| scope.is_some()).map(|ask| &ask.ask);
    let layer = panes.ask_mut();
    match ask {
        None => layer.clear(),
        Some(ask) => {
            if layer.dialog.as_ref().is_none_or(|dialog| dialog.id() != ask.id) {
                layer.mount(session_dialog(ask, session.id(), page));
            }
            layer.layer.set_visible(true);
            layer.wants_focus = wants_focus(session, page.focus_blocked);
        }
    }
    panes.place_ask(scope);
}

/// A terminal ask's dialog: its answers end `session`'s ask, a click on it selects the pane it covers.
fn session_dialog(ask: &PendingAsk, session: Uuid, page: &AskPage<'_>) -> Dialog {
    let (posting, pending) = (page.poster.clone(), ask.clone());
    let answer = move |answer: Answer| {
        let Some(result) = answer.result(&pending) else { return };
        let id = pending.id.clone();
        state::post(&posting, move |state: &mut AppState| {
            state.answer_session_ask(session, &id, result);
        });
    };
    let posting = page.poster.clone();
    let focus = move || {
        state::post(&posting, move |state: &mut AppState| state.select_ask_pane(session));
    };
    Dialog::new(ask, page.look, answer, focus)
}

// MARK: - the GUI ask over its window

/// What a GUI ask covers.
pub enum AnchorTarget {
    /// The terminal area (an ask with no anchor), this widget's bounds.
    Area(gtk::Widget),
    /// An anchored session's page, or its pane at the slot.
    Page { stack: PaneStack, pane: Option<Slot> },
}

impl AnchorTarget {
    /// The resolver of this target in the frame's coordinates, holding it weakly.
    fn resolver(&self) -> AnchorFn {
        match self {
            Self::Area(widget) => {
                let widget = widget.downgrade();
                Box::new(move |frame| modal::bounds_in(&widget.upgrade()?, frame))
            }
            Self::Page { stack, pane } => {
                let (stack, pane) = (stack.downgrade(), *pane);
                Box::new(move |frame| {
                    let stack = stack.upgrade()?;
                    let bounds = modal::bounds_in(stack.upcast_ref(), frame)?;
                    let Some(slot) = pane else { return Some(bounds) };
                    let rect = stack.pane_rect(slot)?;
                    Some(Rect {
                        x: bounds.x + rect.x,
                        y: bounds.y + rect.y,
                        ..rect
                    })
                })
            }
        }
    }
}

/// A window's layer for its GUI ask: a permanent overlay child over the window's content that, while an ask shows,
/// takes every click (upstream's catch-all behind the dialog) and holds the dialog.
pub struct WindowAsk {
    layer: gtk::Box,
    dialog: Option<Dialog>,
}

impl WindowAsk {
    #[must_use]
    pub fn new() -> Self {
        let layer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        layer.set_visible(false);
        Self { layer, dialog: None }
    }

    #[must_use]
    pub fn widget(&self) -> &gtk::Box {
        &self.layer
    }

    /// Whether a dialog shows.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// Gives the dialog the keyboard; whether one took it.
    pub fn grab_focus(&self) -> bool {
        self.dialog.as_ref().is_some_and(Dialog::grab_focus)
    }

    /// Brings the layer in line with window `window`'s GUI ask, anchored to `target` (`None`: nowhere on screen, the
    /// panel waits); whether a new dialog came up, which the caller gives the keyboard.
    pub fn sync(
        &mut self,
        ask: Option<&PendingAsk>,
        target: Option<&AnchorTarget>,
        look: &Look,
        window: Uuid,
        poster: &Poster,
    ) -> bool {
        let Some(ask) = ask else {
            if let Some(dialog) = self.dialog.take() {
                self.layer.remove(dialog.widget());
            }
            self.layer.set_visible(false);
            return false;
        };
        let fresh = self.dialog.as_ref().is_none_or(|dialog| dialog.id() != ask.id);
        if fresh {
            if let Some(dialog) = self.dialog.take() {
                self.layer.remove(dialog.widget());
            }
            let dialog = window_dialog(ask, look, window, poster);
            self.layer.append(dialog.widget());
            self.dialog = Some(dialog);
        }
        if let Some(dialog) = &self.dialog {
            let resolver = target.map_or_else(|| Box::new(|_: &gtk::Widget| None) as AnchorFn, AnchorTarget::resolver);
            dialog.widget().set_anchor(Some(resolver));
        }
        self.layer.set_visible(true);
        fresh
    }
}

impl Default for WindowAsk {
    fn default() -> Self {
        Self::new()
    }
}

/// A GUI ask's dialog: its answers end window `window`'s ask.
fn window_dialog(ask: &PendingAsk, look: &Look, window: Uuid, poster: &Poster) -> Dialog {
    let (posting, pending) = (poster.clone(), ask.clone());
    let answer = move |answer: Answer| {
        let Some(result) = answer.result(&pending) else { return };
        let id = pending.id.clone();
        state::post(&posting, move |state: &mut AppState| {
            state.answer_window_ask(window, &id, result);
        });
    };
    Dialog::new(ask, look, answer, || {})
}

#[cfg(test)]
mod tests {
    use agterm_host::fakes::FakeSurface;

    use super::*;

    fn button(id: &str, label: &str, hotkey: Option<&str>) -> AskButton {
        AskButton {
            id: id.to_owned(),
            label: label.to_owned(),
            hotkey: hotkey.map(str::to_owned),
        }
    }

    fn ask(style: AskStyle) -> PendingAsk {
        PendingAsk {
            id: "A1".to_owned(),
            title: "Deploy?".to_owned(),
            message: Some("to prod".to_owned()),
            buttons: vec![
                button("yes", "Yes", Some("y")),
                button("no", "No", Some("n")),
                button("wipe", "Wipe", None),
            ],
            default_id: None,
            destructive_id: Some("wipe".to_owned()),
            style,
            align: AskAlignment::Right,
            width: None,
            anchor: None,
        }
    }

    #[test]
    fn keys_map_as_upstreams_key_catcher() {
        let none = gdk::ModifierType::empty();
        let shift = gdk::ModifierType::SHIFT_MASK;
        assert_eq!(ask_key(gdk::Key::Tab, none), Some(AskKey::Forward));
        assert_eq!(ask_key(gdk::Key::Tab, shift), Some(AskKey::Backward));
        assert_eq!(ask_key(gdk::Key::ISO_Left_Tab, shift), Some(AskKey::Backward));
        for key in [gdk::Key::Right, gdk::Key::Down, gdk::Key::KP_Down] {
            assert_eq!(ask_key(key, none), Some(AskKey::Forward));
        }
        for key in [gdk::Key::Left, gdk::Key::Up, gdk::Key::KP_Left] {
            assert_eq!(ask_key(key, none), Some(AskKey::Backward));
        }
        assert_eq!(ask_key(gdk::Key::Return, none), Some(AskKey::Activate));
        assert_eq!(ask_key(gdk::Key::KP_Enter, none), Some(AskKey::Activate));
        assert_eq!(ask_key(gdk::Key::Escape, none), Some(AskKey::Cancel));
    }

    #[test]
    fn letters_are_hotkeys_and_modified_keys_go_on() {
        let none = gdk::ModifierType::empty();
        let shift = gdk::ModifierType::SHIFT_MASK;
        assert_eq!(ask_key(gdk::Key::y, none), Some(AskKey::Hotkey("y".to_owned())));
        assert_eq!(ask_key(gdk::Key::Y, shift), Some(AskKey::Hotkey("y".to_owned())));
        assert_eq!(ask_key(gdk::Key::_1, none), None, "only letters are hotkeys");
        assert_eq!(ask_key(gdk::Key::Cyrillic_ya, none), None, "only ASCII letters");
        for held in [
            gdk::ModifierType::CONTROL_MASK,
            gdk::ModifierType::ALT_MASK,
            gdk::ModifierType::SUPER_MASK,
        ] {
            assert_eq!(ask_key(gdk::Key::y, held), None);
            assert_eq!(ask_key(gdk::Key::Return, held), None);
        }
    }

    #[test]
    fn a_hotkey_is_underlined_in_its_label_or_appended() {
        assert_eq!(button_markup(&button("y", "Yes", Some("y")), false), "<u>Y</u>es");
        assert_eq!(button_markup(&button("s", "Yes", Some("s")), false), "Ye<u>s</u>");
        assert_eq!(button_markup(&button("k", "Yes", Some("k")), false), "Yes (<u>K</u>)");
        assert_eq!(button_markup(&button("p", "<a & b>", None), false), "&lt;a &amp; b&gt;");
        assert_eq!(
            button_markup(&button("e", "Ünder é", Some("e")), false),
            "Ünd<u>e</u>r é"
        );
        assert_eq!(
            button_markup(&button("w", "Wipe", Some("w")), true),
            "<b>! <u>W</u>ipe</b>",
            "a terminal destructive button is prefixed and bold"
        );
    }

    #[test]
    fn buttons_style_by_highlight_and_destructiveness() {
        assert_eq!(button_classes(AskStyle::Gui, true, false), ["suggested-action"]);
        assert_eq!(button_classes(AskStyle::Gui, true, true), ["destructive-action"]);
        assert_eq!(button_classes(AskStyle::Gui, false, true), [DESTRUCTIVE_CLASS]);
        assert!(button_classes(AskStyle::Gui, false, false).is_empty());
        assert_eq!(button_classes(AskStyle::Terminal, true, true), [HIGHLIGHTED_CLASS]);
        assert!(button_classes(AskStyle::Terminal, false, true).is_empty());
    }

    #[test]
    fn buttons_go_in_a_row_while_it_fits() {
        assert_eq!(row_width(80, 3, 8), 256);
        assert_eq!(row_width(80, 1, 8), 80);
        assert_eq!(row_width(80, 0, 8), 0);
        assert_eq!(strip_axis(80, 3, 8, 256), StripAxis::Row);
        assert_eq!(strip_axis(80, 3, 8, 255), StripAxis::Column);
        assert_eq!(aligned_start(AskAlignment::Left, 300, 100), 0);
        assert_eq!(aligned_start(AskAlignment::Center, 300, 100), 100);
        assert_eq!(aligned_start(AskAlignment::Right, 300, 100), 200);
        assert_eq!(
            aligned_start(AskAlignment::Right, 50, 100),
            0,
            "overflow starts at the left"
        );
    }

    #[test]
    fn the_look_counts_cells_from_the_font() {
        let look = Look {
            family: Some("Iosevka \"Term\"".to_owned()),
            size: 15.0,
            foreground: Color { r: 255, g: 0, b: 0 },
            background: Color { r: 0, g: 0, b: 0 },
        };
        assert!((look.terminal_cell() - 12.0).abs() < 1e-9, "15pt is 20px, 0.6 of it");
        assert!((look.cell(AskStyle::Gui) - 8.0).abs() < 1e-9);
        let small = Look {
            size: 6.0,
            ..look.clone()
        };
        assert!((small.terminal_cell() - 8.0).abs() < 1e-9, "never under 8");
        let css = terminal_css(&look);
        assert!(
            css.contains("font-family: \"Iosevka Term\", monospace; font-size: 15pt;"),
            "{css}"
        );
        assert!(css.contains("border: 1px solid rgba(255,0,0,0.3)"));
        assert!(
            css.contains("background: rgba(255,0,0,1); color: rgba(0,0,0,1);"),
            "inverted highlight"
        );
    }

    #[test]
    fn answers_report_the_button_or_escaped() {
        let ask = ask(AskStyle::Gui);
        let answered = Answer::Button(1).result(&ask).unwrap();
        assert_eq!(answered.result, ControlAskOutcome::Answered);
        assert_eq!(answered.id.as_deref(), Some("no"));
        assert_eq!(answered.index, Some(1));
        assert_eq!(Answer::Button(9).result(&ask), None);
        assert_eq!(Answer::Escaped.result(&ask).unwrap().result, ControlAskOutcome::Escaped);
    }

    /// What a test ask is opened over.
    #[derive(Clone, Copy)]
    enum Over {
        Session,
        Primary,
        /// A pane identity no pane carries.
        Gone,
    }

    /// A seeded store whose session (primary pane `P`) waits on `ask` opened `over`.
    fn store_with(ask: PendingAsk, over: Over) -> (agterm_core::model::WindowLibrary, Uuid, Uuid) {
        let mut library = agterm_core::model::WindowLibrary::seeded("/home/u");
        let window = library.active_window_id().unwrap();
        let store = library.store_mut(window).unwrap();
        let id = store.sessions().next().unwrap().id();
        let session = store.session_mut(id).unwrap();
        session.surface.replace(Box::new(FakeSurface::new("P")));
        let identity = match over {
            Over::Session => None,
            Over::Primary => Some(session.pane_identity),
            Over::Gone => Some(Uuid::new_v4()),
        };
        assert!(store.open_session_ask(id, ask, identity));
        (library, window, id)
    }

    #[test]
    fn a_session_ask_shows_over_its_whole_page() {
        let (mut library, window, id) = store_with(ask(AskStyle::Terminal), Over::Session);
        let store = library.store_mut(window).unwrap();
        assert_eq!(shown_scope(store.session(id).unwrap(), true), Some(AskScope::Session));
        assert_eq!(
            shown_scope(store.session(id).unwrap(), false),
            None,
            "not shown: nothing draws"
        );
        store.session_mut(id).unwrap().scratch_active = true;
        let session = store.session(id).unwrap();
        assert_eq!(
            shown_scope(session, true),
            Some(AskScope::Session),
            "a session ask covers the scratch"
        );
        assert!(wants_focus(session, false));
        assert!(!wants_focus(session, true));
        let (library, window, id) = store_with(ask(AskStyle::Terminal), Over::Gone);
        let gone = library.store(window).unwrap().session(id).unwrap();
        assert_eq!(shown_scope(gone, true), None, "a pane that is gone");
    }

    #[test]
    fn a_pane_ask_shows_over_its_pane_while_it_is_laid_out() {
        let (mut library, window, id) = store_with(ask(AskStyle::Terminal), Over::Primary);
        let store = library.store_mut(window).unwrap();
        assert_eq!(
            shown_scope(store.session(id).unwrap(), true),
            Some(AskScope::Pane(Slot::Primary))
        );
        store.session_mut(id).unwrap().scratch_active = true;
        assert_eq!(
            shown_scope(store.session(id).unwrap(), true),
            None,
            "it hides under the scratch"
        );
        store.session_mut(id).unwrap().scratch_active = false;
        store.set_split_visibility(id, true, None);
        store
            .session_mut(id)
            .unwrap()
            .split_surface
            .replace(Box::new(FakeSurface::new("S")));
        store.set_split_visibility(id, false, None);
        let session = store.session(id).unwrap();
        assert!(session.split.focused, "a new split focuses right");
        assert_eq!(shown_scope(session, true), None, "its pane is not laid out");
        assert!(!wants_focus(session, false), "the other pane holds the focus");
        store.set_split_visibility(id, true, None);
        assert_eq!(
            shown_scope(store.session(id).unwrap(), true),
            Some(AskScope::Pane(Slot::Primary))
        );
        store.set_split_focus(id, false);
        assert!(wants_focus(store.session(id).unwrap(), false));
    }

    #[test]
    fn the_dialog_navigates_and_answers_once() {
        crate::gtk_test::on_gtk_thread(dialog_navigates_and_answers_once);
    }

    fn dialog_navigates_and_answers_once() {
        let answers: Rc<RefCell<Vec<Answer>>> = Rc::default();
        let look = Look {
            family: None,
            size: 13.0,
            foreground: DEFAULT_FOREGROUND,
            background: DEFAULT_BACKGROUND,
        };
        let seen = Rc::clone(&answers);
        let ask = ask(AskStyle::Terminal);
        let dialog = Dialog::new(&ask, &look, move |answer| seen.borrow_mut().push(answer), || {});
        let state = Rc::clone(&dialog.state);
        let highlighted = |state: &Rc<RefCell<DialogState>>| state.borrow().navigation.highlighted();
        assert_eq!(highlighted(&state), Some(0), "the first non-destructive button");
        let first = state.borrow().buttons[0].clone();
        assert!(first.has_css_class(HIGHLIGHTED_CLASS));
        state.borrow_mut().handle(AskKey::Backward);
        assert_eq!(highlighted(&state), Some(2), "wraps to the last");
        assert!(!first.has_css_class(HIGHLIGHTED_CLASS));
        state.borrow_mut().handle(AskKey::Hotkey("x".to_owned()));
        assert!(answers.borrow().is_empty(), "no such hotkey");
        state.borrow_mut().handle(AskKey::Hotkey("n".to_owned()));
        state.borrow_mut().handle(AskKey::Cancel);
        assert_eq!(*answers.borrow(), [Answer::Button(1)], "the first answer only");
    }
}
