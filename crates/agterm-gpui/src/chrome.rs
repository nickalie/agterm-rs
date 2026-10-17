//! The window chrome in the terminal's colors (ported from the GTK build's `chrome.rs`, commit `fd5349f`; upstream's
//! `WindowAppearance.sync`, `WindowContentView`'s `titlebarHairline`, `sidebarDivider` and `sidebarTintWash`, and the
//! sidebar rows' selection pill): the window body and the title bar take the theme's background in every toolbar
//! mode, the sidebar the same background under the Settings tint's wash, never the title strip; the chrome text takes
//! the theme's foreground, the title-bar hairline (not in the hidden mode) and the sidebar divider that foreground at
//! low opacity, and a selected sidebar row the theme's selection colors. [`agterm_core::chrome`] resolves the colors.
//!
//! The resolved colors are one GPUI global ([`Chrome`]) every window, its title bar and its sidebar paint from: the
//! sidebar's tint, divider and selected-row pill go through [`separator`] and [`selected_row`] here
//! (`sidebar::Look`), the title-bar hairline spans the window, over the sidebar's column too.
//! [`sync`] runs on every windows sync after any config reload, so every appearance change upstream re-applies on (a
//! theme set or previewed, a config reload, a settings change, a desktop light/dark flip) reaches it; it replaces the
//! global only when a color changed.
//!
//! gpui-kit's widgets (the menus, the buttons' hover, the tooltips, the fields, the switches, the sliders, the color
//! pickers) paint from the library's own theme, which [`kit`] keeps on the chrome's light or dark side and colors:
//! [`sync`] brings it along on every run, whether the chrome changed or not.
//!
//! The windows draw their own decorations where the compositor lets them (client-side decorations, Wayland and X11):
//! [`resize_edges`] gives such a window its resize handles along the edges it may be resized at.

use agterm_core::Core;
use agterm_core::chrome::{ChromeColors, Rgb, SelectionColors};
use agterm_core::persist::AppSettings;
use gpui_kit::{
    App, CursorStyle, Decorations, Global, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    Pixels, ResizeEdge, Styled as _, Tiling, Window, div, px,
};

use crate::terminal::{Pick, TerminalConfig};

pub mod kit;

/// The opacity of the chrome text the title bar's group dividers draw with (upstream's `chromeText.opacity(0.25)`).
pub const DIVIDER_ALPHA: f64 = 0.25;
/// The opacity of the title bar's dimmed text: the subtitle and the remote host (upstream's `chromeText.opacity(0.6)`).
pub const DIM_ALPHA: f64 = 0.6;
/// The width of a resize handle along a client-decorated window's edge.
pub const RESIZE_EDGE: Pixels = px(5.);

/// The chrome's colors, as the app's global.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chrome(pub ChromeColors);

impl Global for Chrome {}

/// GPUI's color for `color` at `alpha` (0...1).
#[must_use]
pub fn color(color: Rgb, alpha: f64) -> Hsla {
    crate::mute::color(color, alpha)
}

/// The chrome for the terminals' `config` and the `settings`' sidebar tint.
#[must_use]
pub fn colors(config: &TerminalConfig, settings: &AppSettings) -> ChromeColors {
    let rgb = |color: agterm_term::Rgb| Rgb::new(color.r, color.g, color.b);
    // a selection color that follows the cell is no fixed color: upstream's hex parse leaves it unset
    let fixed = |pick: Option<Pick>| match pick {
        Some(Pick::Color(color)) => Some(rgb(color)),
        _ => None,
    };
    ChromeColors::new(
        Some(rgb(config.theme.background)),
        Some(rgb(config.theme.foreground)),
        SelectionColors {
            background: fixed(config.palette.selection_background),
            foreground: fixed(config.palette.selection_foreground),
        },
        settings.effective_sidebar_shift(),
    )
}

/// The window translucency Settings ▸ Appearance ▸ Window asks for (upstream's `WindowAppearance.Chrome`): the
/// background opacity, whether to blur behind it, and the sidebar tint's signed wash
/// ([`agterm_core::chrome::sidebar_shift_amount`]), which a see-through sidebar paints alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Translucency {
    pub opacity: f64,
    pub blur: bool,
    pub sidebar_shift: f64,
}

impl Global for Translucency {}

impl Translucency {
    /// Opaque, unblurred: the default and anything the settings leave at full opacity.
    pub const OPAQUE: Self = Self {
        opacity: 1.0,
        blur: false,
        sidebar_shift: 0.0,
    };

    /// What `settings` ask for.
    #[must_use]
    pub fn of(settings: &AppSettings) -> Self {
        let opacity = settings
            .background_opacity
            .filter(|opacity| opacity.is_finite())
            .map_or(1.0, |opacity| opacity.clamp(0.0, 1.0));
        Self {
            opacity,
            blur: opacity < 1.0 && settings.background_blur.is_some_and(|blur| blur > 0),
            sidebar_shift: agterm_core::chrome::sidebar_shift_amount(settings.effective_sidebar_shift()),
        }
    }

    /// The opacity a window renders at: the setting, except in full screen, which presents opaque (upstream's
    /// `WindowAppearance.sync` forces native full screen opaque without touching the setting).
    #[must_use]
    pub fn effective(self, fullscreen: bool) -> f64 {
        if fullscreen { 1.0 } else { self.opacity }
    }

    /// The platform background a window asks for: transparent below full opacity, blurred behind when the blur is on
    /// (the compositor's blur protocol, KDE's on Wayland and X11; nothing where it has none).
    #[must_use]
    pub fn appearance(self, fullscreen: bool) -> gpui_kit::WindowBackgroundAppearance {
        if self.effective(fullscreen) >= 1.0 {
            gpui_kit::WindowBackgroundAppearance::Opaque
        } else if self.blur {
            gpui_kit::WindowBackgroundAppearance::Blurred
        } else {
            gpui_kit::WindowBackgroundAppearance::Transparent
        }
    }
}

/// The window translucency now; opaque before the first sync.
#[must_use]
pub fn translucency(cx: &App) -> Translucency {
    cx.try_global::<Translucency>().copied().unwrap_or(Translucency::OPAQUE)
}

/// Whether `window` renders translucent now: then its body alone paints the backing ([`backing`]) and the title bar,
/// the sidebar (its tint wash aside) and the deck paint nothing over it, as upstream's chrome paints nothing below
/// full opacity and its terminals draw transparent (`AppSettings::ghostty_config_lines_for`).
#[must_use]
pub fn see_through(window: &Window, cx: &App) -> bool {
    translucency(cx).effective(window.is_fullscreen()) < 1.0
}

/// The window body's backing: the terminal background at the window's effective opacity (upstream's
/// `window.backgroundColor = background.withAlphaComponent(opacity)`).
#[must_use]
pub fn backing(colors: &ChromeColors, window: &Window, cx: &App) -> Hsla {
    color(colors.background, translucency(cx).effective(window.is_fullscreen()))
}

/// The sidebar's paint: its washed background when opaque; see-through, only the tint's wash over the backing
/// (upstream's `sidebarTintWash`), nothing at the neutral strength.
#[must_use]
pub fn sidebar_fill(colors: &ChromeColors, see_through: bool, shift: f64) -> Hsla {
    if !see_through {
        return color(colors.sidebar, 1.0);
    }
    let wash = if shift < 0.0 { Rgb::WHITE } else { Rgb::BLACK };
    color(wash, shift.abs())
}

/// Paints the chrome in the colors the terminals and the settings resolve to now, the library's widgets too
/// ([`kit::sync`]), and keeps the window translucency the settings ask for; whether the chrome's colors changed.
pub fn sync(core: &Core, config: &TerminalConfig, cx: &mut App) -> bool {
    let translucency = Translucency::of(core.settings());
    if cx.try_global::<Translucency>() != Some(&translucency) {
        cx.set_global(translucency);
        cx.refresh_windows();
    }
    let colors = colors(config, core.settings());
    kit::sync(&colors, cx);
    if cx.try_global::<Chrome>().is_some_and(|shown| shown.0 == colors) {
        return false;
    }
    tracing::debug!(background = %colors.background, sidebar = %colors.sidebar, "chrome colors applied");
    cx.set_global(Chrome(colors));
    true
}

/// The chrome's colors before the first sync: the default config's under the default settings.
#[must_use]
pub fn default_colors() -> ChromeColors {
    colors(&TerminalConfig::default(), &AppSettings::default())
}

/// The chrome's colors now; [`default_colors`] before the first sync.
#[must_use]
pub fn current(cx: &App) -> ChromeColors {
    cx.try_global::<Chrome>().map_or_else(default_colors, |chrome| chrome.0)
}

/// The title-bar hairline and the sidebar divider: the chrome text at 0.1.
#[must_use]
pub fn separator(colors: &ChromeColors) -> Hsla {
    color(colors.foreground, ChromeColors::SEPARATOR_ALPHA)
}

/// A selected sidebar row's pill and text: the selection background, dimmed while the window is not the key one,
/// else upstream's soft white wash.
#[must_use]
pub fn selected_row(colors: &ChromeColors, key_window: bool) -> (Hsla, Hsla) {
    let pill = match (colors.selection_background, key_window) {
        (Some(selection), true) => color(selection, 1.0),
        (Some(selection), false) => color(selection, ChromeColors::INACTIVE_SELECTION_ALPHA),
        (None, true) => color(Rgb::WHITE, ChromeColors::WASH_ALPHA),
        (None, false) => color(Rgb::WHITE, ChromeColors::INACTIVE_WASH_ALPHA),
    };
    (pill, color(colors.selection_foreground, 1.0))
}

/// Where a window decorated by the client may be resized: every edge and corner not tiled against another window or
/// the screen; none while it is maximized or full screen, or decorated by the compositor.
#[must_use]
pub fn resize_zones(decorations: Decorations, filled: bool) -> Vec<ResizeEdge> {
    let Decorations::Client { tiling } = decorations else {
        return Vec::new();
    };
    if filled {
        return Vec::new();
    }
    let Tiling {
        top,
        left,
        right,
        bottom,
    } = tiling;
    [
        (ResizeEdge::Top, !top),
        (ResizeEdge::Bottom, !bottom),
        (ResizeEdge::Left, !left),
        (ResizeEdge::Right, !right),
        (ResizeEdge::TopLeft, !top && !left),
        (ResizeEdge::TopRight, !top && !right),
        (ResizeEdge::BottomLeft, !bottom && !left),
        (ResizeEdge::BottomRight, !bottom && !right),
    ]
    .into_iter()
    .filter_map(|(edge, free)| free.then_some(edge))
    .collect()
}

/// The pointer over a resize handle at `edge`.
fn resize_cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

/// The element id of the resize handle at `edge`.
fn resize_id(edge: ResizeEdge) -> &'static str {
    match edge {
        ResizeEdge::Top => "resize-top",
        ResizeEdge::Bottom => "resize-bottom",
        ResizeEdge::Left => "resize-left",
        ResizeEdge::Right => "resize-right",
        ResizeEdge::TopLeft => "resize-top-left",
        ResizeEdge::TopRight => "resize-top-right",
        ResizeEdge::BottomLeft => "resize-bottom-left",
        ResizeEdge::BottomRight => "resize-bottom-right",
    }
}

/// The resize handles of `window` (over the whole window, drawn last): a press on one hands the resize to the
/// compositor. Nothing when the window may not be resized from its edges ([`resize_zones`]).
#[must_use]
pub fn resize_edges(window: &Window) -> impl IntoElement {
    let filled = window.is_maximized() || window.is_fullscreen();
    let zones = resize_zones(window.window_decorations(), filled);
    let edge = RESIZE_EDGE;
    let corner = RESIZE_EDGE * 3.;
    let handles = zones.into_iter().map(move |zone| {
        let handle = div().id(resize_id(zone)).absolute().cursor(resize_cursor(zone));
        let handle = match zone {
            ResizeEdge::Top => handle.top_0().left_0().right_0().h(edge),
            ResizeEdge::Bottom => handle.bottom_0().left_0().right_0().h(edge),
            ResizeEdge::Left => handle.top_0().bottom_0().left_0().w(edge),
            ResizeEdge::Right => handle.top_0().bottom_0().right_0().w(edge),
            ResizeEdge::TopLeft => handle.top_0().left_0().size(corner),
            ResizeEdge::TopRight => handle.top_0().right_0().size(corner),
            ResizeEdge::BottomLeft => handle.bottom_0().left_0().size(corner),
            ResizeEdge::BottomRight => handle.bottom_0().right_0().size(corner),
        };
        handle.on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.start_window_resize(zone);
        })
    });
    div().absolute().inset_0().children(handles)
}

#[cfg(test)]
mod tests;
