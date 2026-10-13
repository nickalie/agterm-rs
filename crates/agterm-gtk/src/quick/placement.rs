//! The quick terminal's host and placement decisions, pure so they test without a display or a compositor.

use agterm_core::quick_metrics::{self, PanelSize};

/// What hosts the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickHost {
    /// A wlr-layer-shell surface: it floats over every window on the compositor's current output and is never tiled,
    /// the Linux counterpart of upstream's floating, all-spaces `NSPanel`.
    LayerShell,
    /// An ordinary undecorated window, where the compositor offers no layer-shell (GNOME/Mutter, X11): it is placed
    /// like any other window.
    Window,
}

impl QuickHost {
    /// The layer-shell host wherever the compositor supports it, else the window.
    #[must_use]
    pub fn select(layer_shell_supported: bool) -> Self {
        if layer_shell_supported {
            Self::LayerShell
        } else {
            Self::Window
        }
    }

    /// The host's name in the log.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::LayerShell => "layer-shell",
            Self::Window => "window",
        }
    }
}

/// Where a layer-shell panel sits on its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerPlacement {
    /// Anchored to the top edge only, so the compositor centers it horizontally, `top_margin` down so it is centered
    /// vertically too: upstream's frame (`QuickTerminalController.targetFrame`), centered on the screen at the size
    /// `QuickTerminalMetrics` gives.
    Centered { width: i32, height: i32, top_margin: i32 },
    /// Anchored to all four edges: the zoomed panel fills the output's usable area, as upstream's fills the screen's
    /// visible frame.
    Filled,
}

/// The layer-shell placement on a monitor measuring `monitor`.
#[must_use]
pub fn layer_placement(monitor: PanelSize, size_percent: Option<i64>, zoomed: bool) -> LayerPlacement {
    if zoomed {
        return LayerPlacement::Filled;
    }
    let (width, height) = panel_pixels(monitor, size_percent);
    let top_margin = ((pixels(monitor.height) - height) / 2).max(0);
    LayerPlacement::Centered {
        width,
        height,
        top_margin,
    }
}

/// The panel's size in pixels on a monitor measuring `monitor` ([`quick_metrics::panel_size`]).
#[must_use]
pub fn panel_pixels(monitor: PanelSize, size_percent: Option<i64>) -> (i32, i32) {
    let size = quick_metrics::panel_size(monitor, size_percent);
    (pixels(size.width), pixels(size.height))
}

/// The monitor the panel is sized against: the one the compositor put it on, once known, else the active agterm
/// window's, else the first. A Wayland client cannot read the pointer outside its own surfaces, so upstream's
/// pointer-screen rule is the compositor's to apply (the layer surface names no output); the active window's monitor
/// is the estimate until the panel lands.
#[must_use]
pub fn sizing_monitor<T>(placed: Option<T>, active_window: Option<T>, first: Option<T>) -> Option<T> {
    placed.or(active_window).or(first)
}

/// A panel extent as GTK takes it.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a share of a monitor's extent, rounded, fits an i32 by far"
)]
fn pixels(extent: f64) -> i32 {
    extent.round().max(1.0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const LARGE: PanelSize = PanelSize {
        width: 2560.0,
        height: 1440.0,
    };

    #[test]
    fn the_layer_shell_host_is_taken_wherever_the_compositor_supports_it() {
        assert_eq!(QuickHost::select(true), QuickHost::LayerShell);
        assert_eq!(QuickHost::select(false), QuickHost::Window);
        assert_eq!(QuickHost::LayerShell.name(), "layer-shell");
        assert_eq!(QuickHost::Window.name(), "window");
    }

    #[test]
    fn the_panel_is_centered_at_the_metrics_size() {
        assert_eq!(
            layer_placement(LARGE, None, false),
            LayerPlacement::Centered {
                width: 1100,
                height: 700,
                top_margin: 370
            }
        );
    }

    #[test]
    fn a_set_percentage_sizes_and_centers_the_panel() {
        let monitor = PanelSize {
            width: 1920.0,
            height: 1080.0,
        };
        assert_eq!(
            layer_placement(monitor, Some(50), false),
            LayerPlacement::Centered {
                width: 960,
                height: 540,
                top_margin: 270
            }
        );
    }

    #[test]
    fn the_zoomed_panel_fills_the_output() {
        assert_eq!(layer_placement(LARGE, Some(50), true), LayerPlacement::Filled);
    }

    #[test]
    fn the_top_margin_never_goes_negative() {
        let tiny = PanelSize {
            width: 1.0,
            height: 1.0,
        };
        let LayerPlacement::Centered { top_margin, .. } = layer_placement(tiny, Some(90), false) else {
            panic!("an unzoomed panel is centered");
        };
        assert_eq!(top_margin, 0);
    }

    #[test]
    fn panel_extents_round_and_stay_positive() {
        assert_eq!(pixels(1_007.999_999), 1008);
        assert_eq!(pixels(0.2), 1);
        assert_eq!(panel_pixels(LARGE, Some(70)), (1792, 1008));
    }

    #[test]
    fn the_panel_is_sized_on_its_own_monitor_once_placed() {
        assert_eq!(
            sizing_monitor(Some("placed"), Some("active"), Some("first")),
            Some("placed")
        );
        assert_eq!(sizing_monitor(None, Some("active"), Some("first")), Some("active"));
        assert_eq!(sizing_monitor(None, None, Some("first")), Some("first"));
        assert_eq!(sizing_monitor::<&str>(None, None, None), None);
    }
}
