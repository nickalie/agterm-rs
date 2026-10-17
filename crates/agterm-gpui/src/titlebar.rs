//! The window's title bar (upstream's custom titlebar row, `WindowContentView+Titlebar.swift`, with its zoom and
//! dashboard variants; ported from the GTK build's `titlebar.rs`): the sidebar toggle at the sidebar's trailing edge,
//! the title's two lines at the terminal's leading edge ([`agterm_core::titlebar::compose`]), and the trailing cluster
//! of recent-sessions and attention buttons, scratch and split toggles, the dashboard, the quick terminal and the
//! custom-commands button, each gated by its Settings ▸ Interface toggle, with a divider only between two groups of two
//! or more. A terminal zoom or an open dashboard strips the bar to its title and an exit button. The app menu's button
//! ([`crate::app_menu`], the GTK header bar's primary menu) sits at the trailing end in every mode and under every
//! cover: Linux has no global menu bar, and Settings…, Quit and the Help items have no keymap action or palette row, so
//! the button is their only path.
//!
//! The bar is painted in the terminal's colors ([`crate::chrome`]): the theme's background, its foreground for the
//! text and the glyphs (dimmed to 0.6 for the subtitle and the remote host), a hairline under it in the foreground at
//! 0.1 except in the hidden mode. Its empty parts and its title behave as a title bar: a press and drag moves the
//! window, a double click maximizes or restores it, a secondary click opens the window manager's menu.
//!
//! Window decorations are the client's where the compositor lets the window draw them (GPUI's client-side
//! decorations, Wayland and X11): the bar then carries the window controls the window manager supports, on the sides
//! the desktop's button layout puts them. Under server-side decorations the window manager draws its own title bar
//! above this one, as with a GTK header bar. The hidden toolbar mode, upstream a bare drag strip under the macOS menu
//! bar, keeps a minimal bar here, as the GTK build does: the drag surface and the app menu's button, with the window
//! controls while the client decorates, and no title, toggle or trailing cluster.
//!
//! Every button posts a [`TitlebarAction`] the core runs ([`agterm_core::Core::titlebar_action`]); the
//! recent-sessions, attention and custom-commands buttons emit [`TitlebarEvent::Popover`] for the window's popovers
//! ([`crate::popovers`]) to open under the button. The glyphs are Lucide's (gpui-kit's catalog, [`Icons`]), the split and scratch glyphs traced, as
//! the GTK build's are, with their halves filled for the panes on screen.

use std::borrow::Cow;
use std::cell::Cell;
use std::rc::Rc;

use agterm_core::Core;
use agterm_core::app_actions::ActionEffect;
use agterm_core::chrome::{ChromeColors, Rgb};
use agterm_core::keymap::{BuiltinAction, Chord, Modifiers};
use agterm_core::model::AgentStatus;
use agterm_core::persist::settings::ToolbarMode;
use agterm_core::titlebar::{AttentionBell, SplitGlyph, Titlebar as Model, TitlebarAction, TitlebarComposition};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button as KitButton, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu};
use gpui_kit::{
    AnyElement, AppContext as _, AssetSource, Context, Decorations, Div, ElementId, EntityId, EventEmitter, FontWeight,
    Hsla, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _,
    Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, WindowButton,
    WindowButtonLayout, WindowControls, div, prelude::FluentBuilder as _, px, svg,
};
use uuid::Uuid;

use crate::app_menu::AppMenu;
use crate::chrome::{self, DIM_ALPHA, DIVIDER_ALPHA};
use crate::state::{AppState, Poster, post};

/// The row's height in the normal mode (title and subtitle) and the compact one (upstream's `titlebarHeight`); the
/// hidden mode's minimal bar takes the compact height.
pub const NORMAL_HEIGHT: Pixels = px(48.);
pub const COMPACT_HEIGHT: Pixels = px(30.);
/// The blocked bell's color when the settings set none (upstream's `blockedStatusColor` default, macOS
/// `systemOrange`).
pub const BLOCKED_DEFAULT: Rgb = Rgb::new(0xFF, 0x95, 0x00);
/// A disabled button's opacity (upstream's 0.35).
const DISABLED_OPACITY: f32 = 0.35;
/// A button's square and the glyph inside it.
const BUTTON: Pixels = px(24.);
const GLYPH_NORMAL: Pixels = px(18.);
const GLYPH_COMPACT: Pixels = px(16.);
/// The space between the cluster's buttons and after it (upstream's 14 between plain buttons, here between button
/// squares).
const CLUSTER_GAP: Pixels = px(8.);
const CLUSTER_TRAILING: Pixels = px(14.);
/// The remote host's widest (upstream's `RemoteHostTextLayout.ceiling`).
const HOST_CEILING: Pixels = px(240.);
/// A window control's square.
const CONTROL: Pixels = px(30.);

/// The glyphs the bar draws from gpui-kit's Lucide catalog, served to the app with gpui-kit's own component icons
/// ([`Icons`]).
pub const GLYPHS: [IconName; 14] = [
    IconName::PanelLeft,
    IconName::RotateCcwClock,
    IconName::Bell,
    IconName::LayoutGrid,
    IconName::SquareTerminal,
    IconName::CircleEllipsis,
    IconName::Minimize2,
    IconName::X,
    IconName::Cloud,
    IconName::WindowMinimize,
    IconName::WindowMaximize,
    IconName::WindowRestore,
    IconName::WindowClose,
    IconName::Menu,
];

gpui_kit::assets::icon_assets!(
    BarIcons,
    [
        PanelLeft,
        RotateCcwClock,
        Bell,
        LayoutGrid,
        SquareTerminal,
        CircleEllipsis,
        Minimize2,
        X,
        Cloud,
        WindowMinimize,
        WindowMaximize,
        WindowRestore,
        WindowClose,
        Menu,
    ]
);

/// The app's assets: the bar's glyphs, the Settings popup's, then gpui-kit's component icons.
#[derive(Debug, Clone, Copy, Default)]
pub struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = BarIcons.load(path)? {
            return Ok(Some(bytes));
        }
        match crate::settings::SettingsIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets::new("").load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut listed = BarIcons.list(path)?;
        listed.extend(crate::settings::SettingsIcons.list(path)?);
        listed.extend(gpui_kit::assets::Assets::new("").list(path)?);
        listed.sort();
        listed.dedup();
        Ok(listed)
    }
}

/// What covers the window: the bar strips down under a zoom or the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cover {
    None,
    /// A terminal zoom: the title and its exit.
    Zoom,
    /// The dashboard: its own title and its exit.
    Dashboard(String),
}

/// The popover a title-bar button opens ([`crate::popovers`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopoverKind {
    Recent,
    Attention,
    CustomCommands,
}

/// What the bar tells its window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TitlebarEvent {
    /// A popover button was clicked: its popover hangs from `anchor` (window coordinates: under the pointer, at the
    /// bar's bottom edge).
    Popover {
        kind: PopoverKind,
        anchor: gpui_kit::Point<Pixels>,
    },
}

/// Where a popover opened by a click on the bar hangs from: the pointer's x at the bar's bottom edge.
fn popover_anchor(bar: &Titlebar, window: &Window) -> gpui_kit::Point<Pixels> {
    let bottom = bar
        .shown
        .as_ref()
        .map_or(NORMAL_HEIGHT, |shown| height(shown.model.mode));
    gpui_kit::point(window.mouse_position().x, bottom)
}

/// A motion over the bar with `held` down after a press on it (`pressed`, taken): whether it starts a window move. Only
/// a motion with the primary button still held does; a press whose release the bar missed ends at the next motion.
fn starts_move(pressed: &mut bool, held: Option<MouseButton>) -> bool {
    std::mem::take(pressed) && held == Some(MouseButton::Left)
}

/// The hover text `base`, followed by `chord_label` in parentheses when there is one (upstream's `helpHint`).
#[must_use]
pub fn hint(base: &str, chord_label: Option<&str>) -> String {
    match chord_label {
        Some(label) => format!("{base} ({label})"),
        None => base.to_owned(),
    }
}

/// `chord` as a Linux accelerator label, as GTK spells one (`Shift+Ctrl+S`): the modifiers in GTK's order, a
/// character key upper-cased, a named key by its key name.
#[must_use]
pub fn chord_label(chord: &Chord) -> String {
    let mods = [
        (Modifiers::SHIFT, "Shift"),
        (Modifiers::CONTROL, "Ctrl"),
        (Modifiers::OPTION, "Alt"),
        (Modifiers::COMMAND, "Super"),
    ];
    let key = match chord.key.as_str() {
        "return" => "Return".to_owned(),
        "tab" => "Tab".to_owned(),
        "space" => "Space".to_owned(),
        "delete" => "BackSpace".to_owned(),
        "left" => "Left".to_owned(),
        "right" => "Right".to_owned(),
        "up" => "Up".to_owned(),
        "down" => "Down".to_owned(),
        other => other.to_uppercase(),
    };
    mods.into_iter()
        .filter(|(bit, _)| chord.mods.contains(*bit))
        .map(|(_, word)| word)
        .chain(std::iter::once(key.as_str()))
        .collect::<Vec<_>>()
        .join("+")
}

/// `action`'s chord label, `None` when unbound.
fn action_label(core: &Core, action: BuiltinAction) -> Option<String> {
    core.keymap().equivalent(action).map(chord_label)
}

/// The hover texts of the bar's buttons, with the chords bound now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hints {
    pub sidebar: String,
    pub recent: String,
    pub attention: String,
    pub scratch: String,
    pub split: String,
    pub dashboard: String,
    pub quick: String,
    pub custom: String,
}

impl Hints {
    fn of(core: &Core, model: &Model) -> Self {
        let label = |action| action_label(core, action);
        let with = |base: &str, action| hint(base, label(action).as_deref());
        let attention = match model.attention.map(|button| button.bell) {
            Some(AttentionBell::None) | None => "No sessions need attention",
            Some(_) => "Show sessions that need attention",
        };
        let scratch = if model.scratch.is_some_and(|scratch| scratch.active) {
            "Hide scratch terminal"
        } else {
            "Show scratch terminal"
        };
        Self {
            sidebar: with("Toggle Sidebar", BuiltinAction::ToggleSidebar),
            recent: "Recent sessions (Ctrl+Tab)".to_owned(),
            attention: with(attention, BuiltinAction::ShowAttention),
            scratch: with(scratch, BuiltinAction::ToggleScratch),
            split: model
                .split
                .map(|split| with(split.hint, split.action))
                .unwrap_or_default(),
            dashboard: with("Dashboard", BuiltinAction::Dashboard),
            quick: with("Quick Terminal", BuiltinAction::QuickTerminal),
            custom: with("Custom commands", BuiltinAction::CustomCommandPalette),
        }
    }
}

/// Everything the bar shows, as the last sync resolved it.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub model: Model,
    pub cover: Cover,
    /// A dialog holds the window, which stands the exits down.
    pub dialog_up: bool,
    pub hints: Hints,
    /// The sidebar's width while it shows beside the deck (the toggle sits at its trailing edge).
    pub sidebar: Option<f32>,
    /// The blocked bell's color.
    pub blocked: Rgb,
    pub colors: ChromeColors,
    /// The window's app menu, which the menu button opens.
    pub menu: AppMenu,
}

/// The bar's row height for `mode`: the hidden mode's minimal bar, which carries the app menu's button whoever
/// decorates the window, takes the compact one.
#[must_use]
pub fn height(mode: ToolbarMode) -> Pixels {
    match mode {
        ToolbarMode::Normal => NORMAL_HEIGHT,
        ToolbarMode::Compact | ToolbarMode::Hidden => COMPACT_HEIGHT,
    }
}

/// The window controls the bar draws on its `left` or right side: none under the window manager's decorations, else
/// the desktop's `layout` for that side, without the ones the window manager does not support.
#[must_use]
pub fn controls(
    decorations: Decorations,
    supported: WindowControls,
    layout: WindowButtonLayout,
    left: bool,
) -> Vec<WindowButton> {
    if !matches!(decorations, Decorations::Client { .. }) {
        return Vec::new();
    }
    let side = if left { layout.left } else { layout.right };
    side.into_iter()
        .flatten()
        .filter(|button| match button {
            WindowButton::Minimize => supported.minimize,
            WindowButton::Maximize => supported.maximize,
            WindowButton::Close => true,
        })
        .collect()
}

/// A traced glyph: the scratch terminal (filled while it shows) or the split's two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneGlyph {
    Split(SplitGlyph),
    Scratch(bool),
}

/// One rectangle of a traced glyph in its 16x16 slot: an outline or a fill.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub fill: bool,
}

/// The rectangles of `glyph`: the two-pane frame of `rectangle.split.*` with its shown halves filled, or the
/// scratch's `rectangle` with an inset fill (the GTK build's cairo traces).
#[must_use]
pub fn glyph_strokes(glyph: PaneGlyph) -> Vec<Stroke> {
    let (left, top, width, height) = (1.0, 3.0, 14.0, 10.0);
    let rect = |x, y, width, height, fill| Stroke {
        x,
        y,
        width,
        height,
        fill,
    };
    let mut strokes = vec![rect(left, top, width, height, false)];
    let inner = (left + 2.0, top + 2.0, width - 4.0, height - 4.0);
    match glyph {
        PaneGlyph::Scratch(active) => {
            if active {
                strokes.push(rect(inner.0, inner.1, inner.2, inner.3, true));
            }
        }
        PaneGlyph::Split(split) => {
            let stacked = matches!(split, SplitGlyph::BothHorizontal | SplitGlyph::Top | SplitGlyph::Bottom);
            let (cx, cy) = (left + width / 2.0, top + height / 2.0);
            let gap = 1.5;
            if stacked {
                strokes.push(rect(left, cy - 0.5, width, 1.0, true));
            } else {
                strokes.push(rect(cx - 0.5, top, 1.0, height, true));
            }
            let first = if stacked {
                (inner.0, inner.1, inner.2, cy - gap - inner.1)
            } else {
                (inner.0, inner.1, cx - gap - inner.0, inner.3)
            };
            let second = if stacked {
                (inner.0, cy + gap, inner.2, inner.1 + inner.3 - cy - gap)
            } else {
                (cx + gap, inner.1, inner.0 + inner.2 - cx - gap, inner.3)
            };
            let (fill_first, fill_second) = match split {
                SplitGlyph::None => (false, false),
                SplitGlyph::Both | SplitGlyph::BothHorizontal => (true, true),
                SplitGlyph::Left | SplitGlyph::Top => (true, false),
                SplitGlyph::Right | SplitGlyph::Bottom => (false, true),
            };
            for (fill, (x, y, w, h)) in [(fill_first, first), (fill_second, second)] {
                if fill {
                    strokes.push(rect(x, y, w, h, true));
                }
            }
        }
    }
    strokes
}

/// `glyph` drawn in `ink` at `size`.
fn pane_glyph(glyph: PaneGlyph, ink: Hsla, size: Pixels) -> Div {
    let scale = f32::from(size) / 16.0;
    let strokes = glyph_strokes(glyph).into_iter().map(move |stroke| {
        let part = div()
            .absolute()
            .left(px(stroke.x * scale))
            .top(px(stroke.y * scale))
            .w(px(stroke.width * scale))
            .h(px(stroke.height * scale));
        if stroke.fill {
            part.bg(ink)
        } else {
            part.border_1().border_color(ink).rounded(px(2. * scale))
        }
    });
    div().relative().size(size).flex_shrink_0().children(strokes)
}

/// A Lucide glyph in `ink` at `size`.
fn icon(name: IconName, ink: Hsla, size: Pixels) -> AnyElement {
    svg()
        .path(name.path())
        .size(size)
        .flex_shrink_0()
        .text_color(ink)
        .into_any_element()
}

/// A flat button of the bar: its id (upstream's accessibility id), glyph, whether it is enabled, and its hover text.
struct Button {
    id: &'static str,
    glyph: AnyElement,
    enabled: bool,
    hint: String,
}

/// A hover text, in the chrome's colors (the search bar's buttons' too).
pub(crate) struct Hint {
    pub(crate) text: SharedString,
    pub(crate) colors: ChromeColors,
}

impl Render for Hint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(5.))
            .border_1()
            .border_color(chrome::color(self.colors.foreground, DIVIDER_ALPHA))
            .bg(chrome::color(self.colors.background, 1.0))
            .text_color(chrome::color(self.colors.foreground, 1.0))
            .text_size(px(12.))
            .child(self.text.clone())
    }
}

/// `host` cut in the middle past `limit` characters, so an ssh target keeps both its ends (the GTK build's
/// `max_width_chars(30)` with middle ellipsizing).
#[must_use]
pub fn middle_ellipsis(host: &str, limit: usize) -> String {
    let count = host.chars().count();
    if count <= limit || limit < 3 {
        return host.to_owned();
    }
    let keep = limit - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let start: String = host.chars().take(head).collect();
    let end: String = host.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

/// The window's title bar.
pub struct Titlebar {
    window: Uuid,
    poster: Poster,
    shown: Option<Bar>,
    /// A press on the bar that may become a window move.
    pressed: bool,
    /// The app menu while it is open: it holds the keyboard, which the deck moves none under.
    menu: Rc<MenuTrack>,
}

/// Whether the app menu is open, and the popup showing it. The popup's own open-change callback runs only for a toggle,
/// so a popup dropped undismissed (its button no longer drawn) closes the menu through its release.
#[derive(Default)]
struct MenuTrack {
    open: Cell<bool>,
    popup: Cell<Option<EntityId>>,
}

impl MenuTrack {
    /// The menu opened or closed (`open`); a change posts a sync through `poster`, which holds the keyboard for the
    /// menu or gives it back. Either way a popup shown before is not this opening's, so its later release changes
    /// nothing.
    fn set_open(&self, open: bool, poster: &Poster) {
        if self.open.replace(open) != open {
            self.popup.set(None);
            post(poster, |_| {});
        }
    }

    /// Popup `popup` now shows the menu.
    fn built(&self, popup: EntityId) {
        self.popup.set(Some(popup));
    }

    /// Popup `popup` was released: the menu it showed is closed, unless another popup shows it by now.
    fn released(&self, popup: EntityId, poster: &Poster) {
        if self.popup.get() == Some(popup) {
            self.popup.set(None);
            self.set_open(false, poster);
        }
    }
}

impl EventEmitter<TitlebarEvent> for Titlebar {}

impl Titlebar {
    /// The bar of model window `window`; its buttons post through `poster`.
    #[must_use]
    pub fn new(window: Uuid, poster: Poster) -> Self {
        Self {
            window,
            poster,
            shown: None,
            pressed: false,
            menu: Rc::default(),
        }
    }

    /// Whether the app menu is open.
    #[must_use]
    pub fn menu_open(&self) -> bool {
        self.menu.open.get()
    }

    /// What the bar shows, as last synced.
    #[must_use]
    pub fn bar(&self) -> Option<&Bar> {
        self.shown.as_ref()
    }

    /// Renders the window's bar: under `cover` stripped to the title, its exit and the app menu; `dialog_up` while a
    /// dialog holds the window, which stands the exits down. An unchanged bar is not redrawn.
    pub fn sync(&mut self, core: &Core, cover: Cover, dialog_up: bool, colors: ChromeColors, cx: &mut Context<Self>) {
        let model = core.titlebar(self.window);
        let sidebar = core
            .library()
            .store(self.window)
            .map(agterm_core::model::AppStore::sidebar)
            .filter(|sidebar| sidebar.visible && cover == Cover::None)
            .map(|sidebar| {
                #[expect(clippy::cast_possible_truncation, reason = "a sidebar width fits an f32")]
                let width = sidebar.width as f32;
                width
            });
        let blocked = core
            .settings()
            .status_color_hex(AgentStatus::Blocked)
            .and_then(Rgb::from_hex)
            .unwrap_or(BLOCKED_DEFAULT);
        let bar = Bar {
            hints: Hints::of(core, &model),
            model,
            cover,
            dialog_up,
            sidebar,
            blocked,
            colors,
            menu: AppMenu::of(core, self.window),
        };
        if self.shown.as_ref() != Some(&bar) {
            self.shown = Some(bar);
            cx.notify();
        }
    }

    fn post(&self, task: impl FnOnce(&mut AppState) + Send + 'static) {
        post(&self.poster, task);
    }

    fn action(&self, action: TitlebarAction) {
        let window = self.window;
        self.post(move |state: &mut AppState| state.titlebar_action(window, action));
    }

    /// A press on the bar's empty parts or title: a double click maximizes or restores, a secondary click opens the
    /// window manager's menu, a primary press may become a move.
    fn pressed(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.button {
            MouseButton::Left if event.click_count >= 2 => {
                self.pressed = false;
                window.zoom_window();
            }
            MouseButton::Left => self.pressed = true,
            MouseButton::Right if matches!(window.window_decorations(), Decorations::Client { .. }) => {
                window.show_window_menu(event.position);
            }
            _ => {}
        }
        cx.stop_propagation();
    }

    /// A flat button: its glyph, its hover text, enabled or dimmed and inert.
    fn button(
        spec: Button,
        colors: ChromeColors,
        click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Button {
            id,
            glyph,
            enabled,
            hint,
        } = spec;
        let text: SharedString = hint.into();
        div()
            .id(id)
            // upstream's accessibility identifier, and the hover text as the button's name, for AccessKit
            .accessibility_id(id)
            .role(gpui_kit::Role::Button)
            .aria_label(text.clone())
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(BUTTON)
            .rounded(px(5.))
            .when(!enabled, |button| button.opacity(DISABLED_OPACITY))
            .when(enabled, |button| {
                button
                    .hover(move |style| style.bg(chrome::color(colors.foreground, 0.12)))
                    .on_click(cx.listener(move |bar, _, window, cx| click(bar, window, cx)))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .tooltip(move |_, cx| {
                let (text, colors) = (text.clone(), colors);
                cx.new(|_| Hint { text, colors }).into()
            })
            .child(glyph)
    }

    /// The sidebar toggle and the space before the title (upstream: the toggle at the sidebar's trailing edge while it
    /// shows, else at the leading edge, then a gap).
    fn leading(bar: &Bar, glyph: Pixels, cx: &mut Context<Self>) -> Div {
        let colors = bar.colors;
        let ink = chrome::color(colors.foreground, 1.0);
        let uncovered = bar.cover == Cover::None && bar.model.mode != ToolbarMode::Hidden;
        let toggle = (uncovered && bar.model.sidebar_toggle).then(|| {
            let spec = Button {
                id: "sidebar-toggle-button",
                glyph: icon(IconName::PanelLeft, ink, glyph),
                enabled: true,
                hint: bar.hints.sidebar.clone(),
            };
            Self::button(spec, colors, |bar, _, _| bar.action(TitlebarAction::ToggleSidebar), cx)
        });
        match bar.sidebar {
            // the sidebar's width plus its divider, so the title starts at the terminal's leading edge
            Some(width) if uncovered => div()
                .flex()
                .flex_row()
                .flex_shrink_0()
                .justify_end()
                .items_center()
                .w(px(width.max(40.)))
                .pr(px(4.))
                .children(toggle)
                .child(div().w(px(11.))),
            _ => div()
                .flex()
                .flex_row()
                .flex_shrink_0()
                .items_center()
                .pl(px(8.))
                .children(toggle)
                .child(div().w(px(if uncovered { 12. } else { 0. }))),
        }
    }

    /// The title's lines: the identity, the remote host and the compact tail, then the subtitle.
    fn label(composition: &TitlebarComposition, colors: ChromeColors, glyph: Pixels) -> Div {
        let ink = chrome::color(colors.foreground, 1.0);
        let dim = chrome::color(colors.foreground, DIM_ALPHA);
        let title = (!composition.title.is_empty()).then(|| {
            div()
                .id("titlebar-title")
                .flex_shrink_0()
                .max_w_full()
                .font_weight(FontWeight::SEMIBOLD)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(composition.title.clone())
        });
        let host = composition.host.as_ref().map(|host| {
            div()
                .id("titlebar-host")
                .flex()
                .flex_row()
                .flex_shrink_0()
                .items_center()
                .gap(px(4.))
                .when(!composition.title.is_empty(), |host| host.pl(px(6.)))
                .max_w(HOST_CEILING)
                .text_color(dim)
                .child(icon(IconName::Cloud, dim, glyph - px(2.)))
                .child(
                    div()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis_middle()
                        .child(middle_ellipsis(host, 30)),
                )
        });
        let tail = (!composition.tail.is_empty()).then(|| {
            div()
                .id("titlebar-tail")
                .flex_1()
                .min_w(px(0.))
                .font_weight(FontWeight::SEMIBOLD)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(composition.tail.clone())
        });
        let subtitle = (!composition.subtitle.is_empty()).then(|| {
            div()
                .id("titlebar-subtitle")
                .text_size(px(11.))
                .text_color(dim)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(composition.subtitle.clone())
        });
        div()
            .flex()
            .flex_col()
            .justify_center()
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden()
            .gap(px(1.))
            .text_size(px(13.))
            .text_color(ink)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .children(title)
                    .children(host)
                    .children(tail),
            )
            .children(subtitle)
    }

    /// A 1px divider between two button groups (upstream's `chromeText.opacity(0.25)`, 16 high).
    fn divider(colors: ChromeColors) -> Div {
        div()
            .w(px(1.))
            .h(px(16.))
            .flex_shrink_0()
            .bg(chrome::color(colors.foreground, DIVIDER_ALPHA))
    }

    /// The trailing cluster, each button `None` while its interface toggle hides it.
    fn cluster(bar: &Bar, glyph: Pixels, cx: &mut Context<Self>) -> Div {
        let row = div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(CLUSTER_GAP)
            .pr(CLUSTER_TRAILING);
        let row = Self::jump_buttons(row, bar, glyph, cx).when(bar.model.dividers.after_jump, |row| {
            row.child(Self::divider(bar.colors))
        });
        let row = Self::pane_buttons(row, bar, glyph, cx).when(bar.model.dividers.after_panes, |row| {
            row.child(Self::divider(bar.colors))
        });
        Self::plain_buttons(row, bar, glyph, cx)
    }

    /// Recent sessions and the attention bell (dim with nothing to jump to, tinted while a session is blocked).
    fn jump_buttons(mut row: Div, bar: &Bar, glyph: Pixels, cx: &mut Context<Self>) -> Div {
        let (model, hints, colors) = (&bar.model, &bar.hints, bar.colors);
        let ink = chrome::color(colors.foreground, 1.0);
        if let Some(recent) = model.recent {
            let spec = Button {
                id: "recent-sessions-button",
                glyph: icon(IconName::RotateCcwClock, ink, glyph),
                enabled: recent.enabled,
                hint: hints.recent.clone(),
            };
            let open = |bar: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                let anchor = popover_anchor(bar, window);
                cx.emit(TitlebarEvent::Popover {
                    kind: PopoverKind::Recent,
                    anchor,
                });
            };
            row = row.child(Self::button(spec, colors, open, cx));
        }
        if let Some(attention) = model.attention {
            let bell = if attention.bell == AttentionBell::Blocked {
                chrome::color(bar.blocked, 1.0)
            } else {
                ink
            };
            let spec = Button {
                id: "attention-button",
                glyph: icon(IconName::Bell, bell, glyph),
                enabled: attention.enabled,
                hint: hints.attention.clone(),
            };
            let open = |bar: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                let anchor = popover_anchor(bar, window);
                cx.emit(TitlebarEvent::Popover {
                    kind: PopoverKind::Attention,
                    anchor,
                });
            };
            row = row.child(Self::button(spec, colors, open, cx));
        }
        row
    }

    /// The scratch and split toggles, their glyphs tracing the panes on screen.
    fn pane_buttons(mut row: Div, bar: &Bar, glyph: Pixels, cx: &mut Context<Self>) -> Div {
        let (model, hints, colors) = (&bar.model, &bar.hints, bar.colors);
        let ink = chrome::color(colors.foreground, 1.0);
        if let Some(scratch) = model.scratch {
            let spec = Button {
                id: "scratch-toggle",
                glyph: pane_glyph(PaneGlyph::Scratch(scratch.active), ink, glyph).into_any_element(),
                enabled: scratch.enabled,
                hint: hints.scratch.clone(),
            };
            row = row.child(Self::button(
                spec,
                colors,
                |bar, _, _| bar.action(TitlebarAction::ToggleScratch),
                cx,
            ));
        }
        if let Some(split) = model.split {
            let spec = Button {
                id: "split-toggle",
                glyph: pane_glyph(PaneGlyph::Split(split.glyph), ink, glyph).into_any_element(),
                enabled: split.enabled,
                hint: hints.split.clone(),
            };
            row = row.child(Self::button(
                spec,
                colors,
                |bar, _, _| bar.action(TitlebarAction::ToggleSplit),
                cx,
            ));
        }
        row
    }

    /// The dashboard, quick terminal and custom commands buttons.
    fn plain_buttons(mut row: Div, bar: &Bar, glyph: Pixels, cx: &mut Context<Self>) -> Div {
        let (model, hints, colors) = (&bar.model, &bar.hints, bar.colors);
        let ink = chrome::color(colors.foreground, 1.0);
        let plain = [
            (
                "dashboard-toggle-button",
                IconName::LayoutGrid,
                model.dashboard,
                hints.dashboard.clone(),
                Some(TitlebarAction::Dashboard),
            ),
            (
                "quick-terminal-toggle",
                IconName::SquareTerminal,
                model.quick_terminal,
                hints.quick.clone(),
                Some(TitlebarAction::QuickTerminal),
            ),
            (
                "custom-commands-button",
                IconName::CircleEllipsis,
                model.custom_commands,
                hints.custom.clone(),
                None,
            ),
        ];
        for (id, name, state, hint, action) in plain {
            let Some(state) = state else { continue };
            let spec = Button {
                id,
                glyph: icon(name, ink, glyph),
                enabled: state.enabled,
                hint,
            };
            let click = move |bar: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                if let Some(action) = action {
                    bar.action(action);
                } else {
                    let anchor = popover_anchor(bar, window);
                    cx.emit(TitlebarEvent::Popover {
                        kind: PopoverKind::CustomCommands,
                        anchor,
                    });
                }
            };
            row = row.child(Self::button(spec, colors, click, cx));
        }
        row
    }

    /// The cover's exit button: the zoom's or the dashboard's, stood down while a dialog holds the window.
    fn exit(&self, bar: &Bar, glyph: Pixels, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let ink = chrome::color(bar.colors.foreground, 1.0);
        let window = self.window;
        let (id, name, text): (&'static str, IconName, &str) = match bar.cover {
            Cover::None => return None,
            Cover::Zoom => ("terminal-zoom-exit", IconName::Minimize2, "Exit Terminal Zoom"),
            Cover::Dashboard(_) => ("dashboard-exit", IconName::X, "Exit Dashboard"),
        };
        let zoom = bar.cover == Cover::Zoom;
        let spec = Button {
            id,
            glyph: icon(name, ink, glyph),
            enabled: !bar.dialog_up,
            hint: text.to_owned(),
        };
        let click = move |bar: &mut Self, _: &mut Window, _: &mut Context<Self>| {
            if zoom {
                bar.post(move |state: &mut AppState| state.exit_zoom(window));
            } else {
                bar.post(move |state: &mut AppState| state.exit_dashboard(window));
            }
        };
        Some(
            div()
                .pr(CLUSTER_TRAILING)
                .child(Self::button(spec, bar.colors, click, cx)),
        )
    }

    /// The app menu's button (the GTK header bar's primary menu): the menu opens under it, built from the bar's model
    /// as it opens; its closing, or its popup dropped undismissed, posts a sync, which gives the keyboard back to the
    /// shown terminal.
    fn menu_button(&self, bar: &Bar, glyph: Pixels, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bar.colors;
        let ink = chrome::color(colors.foreground, 1.0);
        let perform = crate::app_menu::perform(&self.poster, self.window);
        let titlebar = cx.entity().downgrade();
        let menu_ink = crate::app_menu::Ink {
            chord: chrome::color(colors.foreground, DIM_ALPHA),
        };
        let (track, poster) = (Rc::clone(&self.menu), self.poster.clone());
        let (built, released) = (Rc::clone(&track), poster.clone());
        KitButton::new("app-menu-button")
            .debug_selector(|| "app-menu-button".to_owned())
            .ghost()
            .tab_stop(false)
            .p_0()
            .size(BUTTON)
            .tooltip("Main Menu")
            .child(icon(IconName::Menu, ink, glyph))
            .dropdown_menu_with_anchor(
                gpui_kit::Anchor::TopRight,
                move |popup: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>| {
                    let id = cx.entity_id();
                    built.built(id);
                    let (track, poster) = (Rc::clone(&built), released.clone());
                    cx.on_release(move |_, _| track.released(id, &poster)).detach();
                    let menu = titlebar
                        .upgrade()
                        .and_then(|bar| bar.read(cx).shown.as_ref().map(|shown| shown.menu.clone()));
                    let Some(menu) = menu else {
                        return popup;
                    };
                    crate::app_menu::build(popup, &menu, &perform, menu_ink, window, cx)
                },
            )
            // a sync either way: the deck holds the keyboard while it is open and gives it back once it closes
            .on_open_change(move |opened, _, _| track.set_open(*opened, &poster))
    }

    /// The window controls of one side.
    fn window_controls(&self, buttons: Vec<WindowButton>, colors: ChromeColors, window: &Window) -> Div {
        let ink = chrome::color(colors.foreground, 1.0);
        let maximized = window.is_maximized();
        let id = self.window;
        let poster = self.poster.clone();
        let controls = buttons.into_iter().map(move |button| {
            let name = match button {
                WindowButton::Minimize => IconName::WindowMinimize,
                WindowButton::Maximize if maximized => IconName::WindowRestore,
                WindowButton::Maximize => IconName::WindowMaximize,
                WindowButton::Close => IconName::WindowClose,
            };
            let poster = poster.clone();
            div()
                .id(ElementId::Name(format!("window-{}", button.id()).into()))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .w(CONTROL)
                .h_full()
                .hover(move |style| style.bg(chrome::color(colors.foreground, 0.12)))
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |_, window, _| match button {
                    WindowButton::Minimize => window.minimize_window(),
                    WindowButton::Maximize => window.zoom_window(),
                    // through the app's close path, as the window manager's close button goes
                    WindowButton::Close => post(&poster, move |state: &mut AppState| state.close_requested(id)),
                })
                .child(icon(name, ink, GLYPH_COMPACT))
        });
        div().flex().flex_row().flex_shrink_0().h_full().children(controls)
    }
}

impl Render for Titlebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(bar) = self.shown.clone() else {
            return div().id("titlebar");
        };
        let decorations = window.window_decorations();
        let mode = bar.model.mode;
        let height = height(mode);
        let glyph = if mode == ToolbarMode::Normal {
            GLYPH_NORMAL
        } else {
            GLYPH_COMPACT
        };
        let layout = cx.button_layout().unwrap_or_else(WindowButtonLayout::linux_default);
        let supported = window.window_controls();
        let colors = bar.colors;
        let hidden = mode == ToolbarMode::Hidden;
        let label = match &bar.cover {
            Cover::Dashboard(title) => TitlebarComposition {
                title: title.clone(),
                ..TitlebarComposition::default()
            },
            _ => bar.model.label.clone(),
        };
        let leading = self.window_controls(controls(decorations, supported, layout, true), colors, window);
        let trailing = self.window_controls(controls(decorations, supported, layout, false), colors, window);
        let uncovered = bar.cover == Cover::None;
        // a translucent window's backing shows through the bar (upstream's chrome paints nothing below full opacity)
        let see_through = chrome::see_through(window, cx);
        div()
            .id("titlebar")
            .debug_selector(|| "titlebar".to_owned())
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .w_full()
            .h(height)
            .when(!see_through, |row| row.bg(chrome::color(colors.background, 1.0)))
            .text_color(chrome::color(colors.foreground, 1.0))
            .when(!hidden, |row| row.border_b_1().border_color(chrome::separator(&colors)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::pressed))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::pressed))
            .on_mouse_up(MouseButton::Left, cx.listener(|bar, _, _, _| bar.pressed = false))
            // the bar sees no release past its edge, nor a press elsewhere: either ends a press it saw
            .on_mouse_up_out(MouseButton::Left, cx.listener(|bar, _, _, _| bar.pressed = false))
            .on_mouse_down_out(cx.listener(|bar, _, _, _| bar.pressed = false))
            .on_mouse_move(cx.listener(|bar, event: &MouseMoveEvent, window, _| {
                if starts_move(&mut bar.pressed, event.pressed_button) {
                    window.start_window_move();
                }
            }))
            .child(leading)
            .when(!hidden, |row| {
                row.child(Self::leading(&bar, glyph, cx))
                    .child(Self::label(&label, colors, glyph))
                    .child(div().w(px(12.)).flex_shrink_0())
                    .when(uncovered, |row| row.child(Self::cluster(&bar, glyph, cx)))
                    .children(self.exit(&bar, glyph, cx))
            })
            .when(hidden, |row| row.child(div().flex_1()))
            .child(div().pr(px(8.)).child(self.menu_button(&bar, glyph, cx)))
            .child(trailing)
    }
}

impl AppState {
    /// A title-bar button of window `window` was used.
    pub fn titlebar_action(&mut self, window: Uuid, action: TitlebarAction) {
        if self.is_quitting() {
            return;
        }
        if let ActionEffect::Frontend(left) = self.core.titlebar_action(window, action) {
            tracing::debug!(?left, "a title-bar action left work to the frontend");
        }
        self.settle();
    }

    /// The dashboard's exit button.
    pub fn exit_dashboard(&mut self, window: Uuid) {
        if self.is_quitting() {
            return;
        }
        self.core.dashboard_close(window);
        self.settle();
    }
}

/// The glyph paths the bar draws, for the asset check.
#[must_use]
pub fn glyph_paths() -> Vec<SharedString> {
    GLYPHS.iter().map(|name| name.path()).collect()
}

/// Whether the app's assets serve `path`.
#[must_use]
pub fn serves(path: &str) -> bool {
    Icons.load(path).ok().flatten().is_some()
}

#[cfg(test)]
mod tests;
