//! The panel's window (the GTK build's `quick/layer.rs` and its window host): a wlr-layer-shell surface where the
//! compositor has one, else an ordinary undecorated window, and the frame each takes on its display
//! ([`ngterm_app::quick_placement`]).
//!
//! The layer surface (Wayland only, behind `cfg(target_os = "linux")`): on the overlay layer (a fullscreen window
//! covers the top layer on wlroots compositors such as sway, and upstream's panel floats over full-screen apps), taking
//! the keyboard on demand (focus moving away from it is what dismisses a summoned panel), with no exclusive zone, so it
//! overlays without pushing other windows aside. Centered, it is anchored to the top edge only, so the compositor
//! centers it across, with the top margin that centers it down; zoomed, it is anchored to all four edges at size zero,
//! which takes the output's usable area. It names no output as it comes on screen, so the compositor puts it on its
//! current output (niri gives no keyboard to an on-demand surface on an output it does not focus); a refit names the
//! output it landed on.
//!
//! The window host (GNOME/Mutter and weston, which have no layer-shell; X11, where GPUI opens a layer-shell window as a
//! plain one; every other platform): undecorated (client decorations, no title bar of ours), on a transparent
//! background, sized on its display and centered there where the platform lets a client place a window (X11; Wayland
//! gives a client no position). Its zoom is a maximize.

use gpui_kit::{
    Bounds, DisplayId, Pixels, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowDecorations,
    WindowKind, WindowOptions, point, px, size,
};
#[cfg(target_os = "linux")]
use ngterm_app::quick_placement::LayerPlacement;
use ngterm_app::quick_placement::{self as placement, QuickHost};
use ngterm_core::quick_metrics::PanelSize;

/// The layer surface's namespace, for compositor rules (niri's `layer-rule`, sway's `layer_effects`), as the GTK
/// build names it.
pub const NAMESPACE: &str = "ngterm-quick-terminal";

/// The panel window's title (X11's `WM_NAME`, Wayland's toplevel title on the window host).
pub const TITLE: &str = "Quick Terminal";

/// The panel's size where no display is known: the built-in size's cap (`quick_metrics::DEFAULT_MAX_SIZE`).
const NO_DISPLAY: (i32, i32) = (1100, 700);

/// What the panel's frame follows besides its display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    /// The user's size setting (`None`: the built-in default).
    pub size_percent: Option<i64>,
    /// The quick zoom.
    pub zoomed: bool,
}

/// The display a frame is computed against: its id, and its bounds in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub id: DisplayId,
    pub bounds: Bounds<Pixels>,
}

impl Target {
    /// The display's extent, as the placement takes it.
    #[must_use]
    pub fn extent(&self) -> PanelSize {
        PanelSize {
            width: f64::from(f32::from(self.bounds.size.width)),
            height: f64::from(f32::from(self.bounds.size.height)),
        }
    }
}

/// The host the panel tries first: a layer surface on Wayland, where the compositor may have layer-shell (a
/// compositor without it refuses the surface, and the panel falls back then); the window host on X11, where GPUI would
/// open a layer surface as a plain window, and on every other platform.
#[must_use]
pub fn first_host(compositor: &str) -> QuickHost {
    QuickHost::select(cfg!(target_os = "linux") && compositor == "Wayland")
}

/// Whether opening a layer surface failed because the compositor has no layer-shell: the panel takes the window host
/// from then on.
#[must_use]
pub fn refuses_layer_shell<T>(opened: &gpui_kit::Result<T>) -> bool {
    #[cfg(target_os = "linux")]
    {
        opened.as_ref().err().is_some_and(|error| {
            error
                .downcast_ref::<gpui_kit::layer_shell::LayerShellNotSupportedError>()
                .is_some()
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = opened;
        false
    }
}

/// The window options of the panel on `host` for `fit` on `display` (`None`: no display is known), the layer surface
/// named on it only when `named`; `None` when a centered layer surface has no display to be framed on.
#[must_use]
pub fn window_options(
    host: QuickHost,
    fit: Fit,
    display: Option<&Target>,
    named: bool,
    app_id: &str,
) -> Option<WindowOptions> {
    let base = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(TITLE.into()),
            ..TitlebarOptions::default()
        }),
        focus: true,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some(app_id.to_owned()),
        window_decorations: Some(WindowDecorations::Client),
        ..WindowOptions::default()
    };
    match host {
        QuickHost::LayerShell => layer_options(base, fit, display, named),
        QuickHost::Window => Some(window_host_options(base, fit, display)),
    }
}

/// The layer surface's options ([`placement::layer_placement`]); the window host's off Linux.
#[cfg(target_os = "linux")]
fn layer_options(base: WindowOptions, fit: Fit, display: Option<&Target>, named: bool) -> Option<WindowOptions> {
    use gpui_kit::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
    let (anchor, margin, extent) =
        match placement::layer_placement(display.map(Target::extent), fit.size_percent, fit.zoomed)? {
            LayerPlacement::Centered {
                width,
                height,
                top_margin,
            } => (
                Anchor::TOP,
                Some((pixels(top_margin), px(0.), px(0.), px(0.))),
                size(pixels(width), pixels(height)),
            ),
            // a zero size along an axis anchored at both edges takes the whole axis
            LayerPlacement::Filled => (
                Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                None,
                size(px(0.), px(0.)),
            ),
        };
    Some(WindowOptions {
        kind: WindowKind::LayerShell(LayerShellOptions {
            namespace: NAMESPACE.to_owned(),
            layer: Layer::Overlay,
            anchor,
            exclusive_zone: Some(px(0.)),
            exclusive_edge: None,
            margin,
            keyboard_interactivity: KeyboardInteractivity::OnDemand,
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: extent,
        })),
        display_id: display.filter(|_| named).map(|display| display.id),
        ..base
    })
}

#[cfg(not(target_os = "linux"))]
fn layer_options(base: WindowOptions, fit: Fit, display: Option<&Target>, _named: bool) -> Option<WindowOptions> {
    Some(window_host_options(base, fit, display))
}

/// The window host's options: the panel's size on `display`, centered on it, maximized while zoomed.
fn window_host_options(base: WindowOptions, fit: Fit, display: Option<&Target>) -> WindowOptions {
    let (width, height) = display.map_or(NO_DISPLAY, |display| {
        placement::panel_pixels(display.extent(), fit.size_percent)
    });
    let extent = size(pixels(width), pixels(height));
    let bounds = display.map_or(
        Bounds {
            origin: point(px(0.), px(0.)),
            size: extent,
        },
        |display| Bounds::centered_at(display.bounds.center(), extent),
    );
    WindowOptions {
        kind: WindowKind::Normal,
        window_bounds: Some(if fit.zoomed {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        }),
        display_id: display.map(|display| display.id),
        ..base
    }
}

/// A whole-pixel extent as GPUI's logical pixels; a panel extent is far below `u16::MAX`.
fn pixels(extent: i32) -> Pixels {
    px(u16::try_from(extent.max(0)).map_or(f32::from(u16::MAX), f32::from))
}
