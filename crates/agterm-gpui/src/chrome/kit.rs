//! gpui-kit's theme in the chrome's colors, so the library's widgets read as the window around them: the popup,
//! dropdown and context menus, the ghost buttons' hover and pressed fills (the sidebar footer, the app menu button,
//! Settings' pickers), the tooltips, the text fields, the switches, the sliders, the color pickers and the scrollbars.
//!
//! The library paints from a theme of its own (`gpui_kit::component::Theme`), which `gpui_kit::init` leaves on its
//! light palette and nothing in it follows by itself. [`sync`] puts it on the chrome's side: dark when the chrome
//! reads dark (upstream's `GhosttyApp.terminalThemeIsDark`, which pins the AppKit-drawn chrome to the terminal theme's
//! brightness rather than the desktop's: [`agterm_core::html::theme::is_dark`] over the washed sidebar background),
//! which loads the library's palette for that side, then sets the colors [`SLOTS`] lists from the chrome over it. It
//! runs with every chrome sync (`super::sync`), so a theme set or previewed, a config reload, a settings change and a
//! desktop light/dark flip all reach the widgets, and it rewrites the theme only when what it shows differs.

use agterm_core::chrome::{ChromeColors, Rgb};
use gpui_kit::component::{Theme, ThemeColor, ThemeMode};
use gpui_kit::{App, Hsla};

use super::color;

/// The chrome text's wash over the background a hovered row or button takes (the title bar's own buttons' `0.12`).
pub const HOVER: f64 = 0.12;
/// A pressed or selected control's wash.
pub const ACTIVE: f64 = 0.18;
/// A quiet fill: a muted surface, a secondary button at rest.
pub const MUTED: f64 = 0.06;
/// A control's border and a field's outline.
pub const BORDER: f64 = 0.15;
/// A switch's track while off.
pub const TRACK: f64 = 0.25;

/// Where a library color comes from in the chrome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ink {
    /// The window body: the terminal background.
    Background,
    /// The sidebar: the background under the Settings tint's wash.
    Sidebar,
    /// The chrome text: the terminal foreground.
    Text,
    /// The chrome text at this opacity over the background, flattened to an opaque color.
    Wash(f64),
    /// The suggested action's fill (`crate::modal::ACCENT`, the alerts' default button and Settings' switches).
    Accent,
    /// Text and thumbs on the accent: white.
    OnAccent,
}

impl Ink {
    /// The color this ink resolves to in `colors`.
    #[must_use]
    pub fn resolve(self, colors: &ChromeColors) -> Hsla {
        match self {
            Self::Background => color(colors.background, 1.0),
            Self::Sidebar => color(colors.sidebar, 1.0),
            Self::Text => color(colors.foreground, 1.0),
            Self::Wash(alpha) => color(colors.background, 1.0).blend(color(colors.foreground, alpha)),
            Self::Accent => color(crate::modal::ACCENT, 1.0),
            Self::OnAccent => color(Rgb::WHITE, 1.0),
        }
    }
}

/// One of the library's colors.
pub type Slot = fn(&mut ThemeColor) -> &mut Hsla;

/// One entry of [`SLOTS`]: the library's color `field`, its name and its ink.
macro_rules! slot {
    ($field:ident, $ink:expr) => {
        (stringify!($field), |colors: &mut ThemeColor| &mut colors.$field, $ink)
    };
}

/// The library's colors the chrome sets, by name, and their inks; every other color keeps the library's palette for
/// the chrome's side (the status colors, the charts, the highlighter's).
pub const SLOTS: [(&str, Slot, Ink); 38] = [
    slot!(background, Ink::Background),
    slot!(foreground, Ink::Text),
    // the menus, the tooltips, the color picker's and the dropdowns' panels: the alerts' and palettes' panel
    slot!(popover, Ink::Background),
    slot!(popover_foreground, Ink::Text),
    // a menu row under the pointer or the keyboard, a ghost button's hover
    slot!(accent, Ink::Wash(HOVER)),
    slot!(accent_foreground, Ink::Text),
    slot!(muted, Ink::Wash(MUTED)),
    slot!(muted_foreground, Ink::Wash(super::DIM_ALPHA)),
    slot!(border, Ink::Wash(BORDER)),
    slot!(input, Ink::Wash(BORDER)),
    slot!(window_border, Ink::Wash(BORDER)),
    slot!(ring, Ink::Accent),
    slot!(caret, Ink::Text),
    slot!(secondary, Ink::Wash(MUTED)),
    slot!(secondary_hover, Ink::Wash(HOVER)),
    // a ghost button toggled on (the sidebar's flagged view)
    slot!(secondary_active, Ink::Wash(ACTIVE)),
    slot!(secondary_foreground, Ink::Text),
    slot!(button, Ink::Wash(MUTED)),
    slot!(button_hover, Ink::Wash(HOVER)),
    // a ghost button held down
    slot!(button_active, Ink::Wash(ACTIVE)),
    slot!(button_foreground, Ink::Text),
    slot!(list, Ink::Background),
    slot!(list_hover, Ink::Wash(HOVER)),
    slot!(list_active, Ink::Wash(ACTIVE)),
    slot!(list_active_border, Ink::Accent),
    slot!(primary, Ink::Accent),
    slot!(primary_hover, Ink::Accent),
    slot!(primary_active, Ink::Accent),
    slot!(primary_foreground, Ink::OnAccent),
    slot!(switch, Ink::Wash(TRACK)),
    slot!(switch_thumb, Ink::OnAccent),
    slot!(slider_bar, Ink::Accent),
    slot!(slider_thumb, Ink::OnAccent),
    slot!(title_bar, Ink::Background),
    slot!(title_bar_border, Ink::Wash(ChromeColors::SEPARATOR_ALPHA)),
    slot!(sidebar, Ink::Sidebar),
    slot!(sidebar_foreground, Ink::Text),
    slot!(sidebar_border, Ink::Wash(ChromeColors::SEPARATOR_ALPHA)),
];

/// The library's side for `colors`: dark when the washed sidebar reads dark (module docs).
#[must_use]
pub fn mode(colors: &ChromeColors) -> ThemeMode {
    let unit = |channel: u8| f64::from(channel) / 255.0;
    let Rgb { r, g, b } = colors.sidebar;
    if agterm_core::html::theme::is_dark(unit(r), unit(g), unit(b)) {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    }
}

/// Whether the library's theme shows `colors`: their side, and every color [`SLOTS`] sets.
#[must_use]
pub fn shows(colors: &ChromeColors, cx: &App) -> bool {
    let Some(theme) = cx.try_global::<Theme>() else {
        return false;
    };
    let mut shown = theme.colors;
    theme.mode == mode(colors)
        && SLOTS
            .iter()
            .all(|(_, slot, ink)| *slot(&mut shown) == ink.resolve(colors))
}

/// Puts the library's theme on `colors` (module docs); whether it changed. Nothing before `gpui_kit::init` set one up.
pub fn sync(colors: &ChromeColors, cx: &mut App) -> bool {
    if !cx.has_global::<Theme>() || shows(colors, cx) {
        return false;
    }
    let mode = mode(colors);
    if Theme::global(cx).mode != mode {
        // the library's palette for the side, which the chrome's colors then go over
        Theme::change(mode, None, cx);
    }
    Theme::update(cx, |theme| {
        for (_, slot, ink) in SLOTS {
            *slot(&mut theme.colors) = ink.resolve(colors);
        }
    });
    tracing::debug!(mode = mode.name(), background = %colors.background, "the library's theme follows the chrome");
    true
}
