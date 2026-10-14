//! The quick terminal's host and placement decisions, pure so they test without a display or a compositor; each
//! frontend applies them to its own panel.

use agterm_core::quick_metrics::{self, PanelSize};

/// What hosts the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickHost {
    /// A wlr-layer-shell surface: it floats over every window on its output and is never tiled, the Linux
    /// counterpart of upstream's floating, all-spaces `NSPanel`.
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

/// The layer-shell placement for the quick zoom (`zoomed`) or the size setting on a monitor measuring `monitor`: a
/// zoomed panel fills its output and needs no monitor; an unzoomed one without a monitor keeps its frame (`None`).
#[must_use]
pub fn layer_placement(monitor: Option<PanelSize>, size_percent: Option<i64>, zoomed: bool) -> Option<LayerPlacement> {
    if zoomed {
        return Some(LayerPlacement::Filled);
    }
    let monitor = monitor?;
    let (width, height) = panel_pixels(monitor, size_percent);
    let top_margin = ((pixels(monitor.height) - height) / 2).max(0);
    Some(LayerPlacement::Centered {
        width,
        height,
        top_margin,
    })
}

/// The panel's size in pixels on a monitor measuring `monitor` ([`quick_metrics::panel_size`]).
#[must_use]
pub fn panel_pixels(monitor: PanelSize, size_percent: Option<i64>) -> (i32, i32) {
    let size = quick_metrics::panel_size(monitor, size_percent);
    (pixels(size.width), pixels(size.height))
}

/// The monitor the panel is framed on: `landed`, the one a shown layer surface is on (as the compositor reported it),
/// else the frontmost agterm window's, else the first. The compositor chooses the layer surface's output, usually the
/// focused one (a Wayland client cannot read the pointer outside its own surfaces, so that stands in for upstream's
/// pointer-screen rule); until it reports where the surface landed, the frontmost agterm window's monitor is the
/// estimate.
#[must_use]
pub fn sizing_monitor<T>(landed: Option<T>, frontmost: Option<T>, first: Option<T>) -> Option<T> {
    landed.or(frontmost).or(first)
}

/// Whether a layer panel that entered `entered` is refitted to it: only while shown and not zoomed (a zoomed panel
/// fills whatever output it is on), and only when its frame was computed against another monitor (`sized_for`, the
/// estimate when the compositor chose another output).
#[must_use]
pub fn refits_on_enter<T: PartialEq>(entered: &T, sized_for: Option<&T>, shown: bool, zoomed: bool) -> bool {
    shown && !zoomed && sized_for != Some(entered)
}

/// What becomes of the hidden layer surface's output (`bound`) as the panel comes on screen: it is cleared, so the
/// compositor puts the surface on its current output. Hidden, the surface has no layer role, so clearing the output
/// does not remap it.
#[must_use]
pub fn output_on_show<T>(bound: Option<&T>) -> OutputChange<T> {
    match bound {
        Some(_) => OutputChange::Move(None),
        None => OutputChange::Keep,
    }
}

/// What becomes of the layer surface's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputChange<T> {
    /// The output stays as it is.
    Keep,
    /// The surface moves to this output; `None` clears it, so the compositor chooses.
    Move(Option<T>),
}

/// The output change for a surface bound to `bound` when the monitors changed (or the panel was just hidden) and
/// `usable` are the monitors left: it stays while its monitor is usable, else it moves to the first usable one. A
/// surface without an output always moves (the library unrealizes an output-less surface on every monitor change), so
/// a shown panel, which never names one, is hidden first (the frontend's layer-shell host).
#[must_use]
pub fn output_change<T: Clone + PartialEq>(bound: Option<&T>, usable: &[T]) -> OutputChange<T> {
    match bound {
        Some(bound) if usable.contains(bound) => OutputChange::Keep,
        None if usable.is_empty() => OutputChange::Keep,
        _ => OutputChange::Move(usable.first().cloned()),
    }
}

/// A panel extent in whole pixels, as a toolkit takes it.
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
            layer_placement(Some(LARGE), None, false),
            Some(LayerPlacement::Centered {
                width: 1100,
                height: 700,
                top_margin: 370
            })
        );
    }

    #[test]
    fn a_set_percentage_sizes_and_centers_the_panel() {
        let monitor = PanelSize {
            width: 1920.0,
            height: 1080.0,
        };
        assert_eq!(
            layer_placement(Some(monitor), Some(50), false),
            Some(LayerPlacement::Centered {
                width: 960,
                height: 540,
                top_margin: 270
            })
        );
    }

    #[test]
    fn the_zoomed_panel_fills_its_output_with_or_without_a_monitor() {
        assert_eq!(
            layer_placement(Some(LARGE), Some(50), true),
            Some(LayerPlacement::Filled)
        );
        assert_eq!(layer_placement(None, Some(50), true), Some(LayerPlacement::Filled));
    }

    #[test]
    fn an_unzoomed_panel_without_a_monitor_keeps_its_frame() {
        assert_eq!(layer_placement(None, None, false), None);
    }

    #[test]
    fn the_top_margin_never_goes_negative() {
        let tiny = PanelSize {
            width: 1.0,
            height: 1.0,
        };
        let Some(LayerPlacement::Centered { top_margin, .. }) = layer_placement(Some(tiny), Some(90), false) else {
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
    fn a_landed_panel_is_framed_on_its_own_monitor_else_the_frontmost_windows() {
        assert_eq!(
            sizing_monitor(Some("landed"), Some("frontmost"), Some("first")),
            Some("landed")
        );
        assert_eq!(
            sizing_monitor(None, Some("frontmost"), Some("first")),
            Some("frontmost")
        );
        assert_eq!(sizing_monitor(None, None, Some("first")), Some("first"));
        assert_eq!(sizing_monitor::<&str>(None, None, None), None);
    }

    #[test]
    fn a_shown_panel_is_refitted_when_it_lands_off_its_estimate() {
        assert!(refits_on_enter(&"b", Some(&"a"), true, false));
        assert!(refits_on_enter(&"b", None, true, false));
        assert!(!refits_on_enter(&"a", Some(&"a"), true, false));
    }

    #[test]
    fn a_hidden_or_zoomed_panel_is_not_refitted_on_enter() {
        assert!(!refits_on_enter(&"b", Some(&"a"), false, false));
        assert!(!refits_on_enter(&"b", Some(&"a"), true, true));
    }

    #[test]
    fn the_output_is_cleared_as_the_panel_comes_on_screen() {
        assert_eq!(output_on_show(Some(&"a")), OutputChange::Move(None));
        assert_eq!(output_on_show::<&str>(None), OutputChange::Keep);
    }

    #[test]
    fn a_shown_output_less_panel_moves_on_any_monitor_change() {
        // a monitor plugged in or out under the shown panel (no output): it is hidden and bound to the first left
        assert_eq!(output_change(None, &["a", "b"]), OutputChange::Move(Some("a")));
        assert_eq!(output_change(None, &["a"]), OutputChange::Move(Some("a")));
    }

    #[test]
    fn the_output_stays_while_its_monitor_is_usable() {
        assert_eq!(output_change(Some(&"a"), &["b", "a"]), OutputChange::Keep);
        assert_eq!(output_change::<&str>(None, &[]), OutputChange::Keep);
    }

    #[test]
    fn a_lost_or_missing_output_moves_to_the_first_monitor_left() {
        assert_eq!(output_change(Some(&"a"), &["b", "c"]), OutputChange::Move(Some("b")));
        assert_eq!(output_change(None, &["b"]), OutputChange::Move(Some("b")));
        assert_eq!(output_change(Some(&"a"), &[]), OutputChange::Move(None));
    }
}
