//! The window chrome in the terminal's colors (ported from the GTK build's `chrome.rs`, commit `fd5349f`; upstream's
//! `WindowAppearance.sync`, `WindowContentView`'s `titlebarHairline`, `sidebarDivider` and `sidebarTintWash`, and the
//! sidebar rows' selection pill): the window body and the title bar take the theme's background in every toolbar
//! mode, the sidebar the same background under the Settings tint's wash, never the title strip; the chrome text takes
//! the theme's foreground, the title-bar hairline (not in the hidden mode) and the sidebar divider that foreground at
//! low opacity, and a selected sidebar row the theme's selection colors. [`agterm_core::chrome`] resolves the colors.
//!
//! The resolved colors are one GPUI global ([`Chrome`]) every window, its title bar and its sidebar paint from.
//! [`sync`] runs on every windows sync after any config reload, so every appearance change upstream re-applies on (a
//! theme set or previewed, a config reload, a settings change, a desktop light/dark flip) reaches it; it replaces the
//! global only when a color changed.
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

/// Paints the chrome in the colors the terminals and the settings resolve to now; whether they changed.
pub fn sync(core: &Core, config: &TerminalConfig, cx: &mut App) -> bool {
    let colors = colors(config, core.settings());
    if cx.try_global::<Chrome>().is_some_and(|shown| shown.0 == colors) {
        return false;
    }
    tracing::debug!(background = %colors.background, sidebar = %colors.sidebar, "chrome colors applied");
    cx.set_global(Chrome(colors));
    true
}

/// The chrome's colors now; the default config's before the first sync.
#[must_use]
pub fn current(cx: &App) -> ChromeColors {
    cx.try_global::<Chrome>().map_or_else(
        || colors(&TerminalConfig::default(), &AppSettings::default()),
        |chrome| chrome.0,
    )
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
