//! The quick terminal's layer-shell host, through `gtk4-layer-shell`: a surface on the overlay layer (a fullscreen
//! window covers the top layer on wlroots compositors such as sway, and upstream's panel floats over full-screen apps),
//! taking the keyboard on demand (focus moves away from it, which is what dismisses a summoned panel), with no
//! exclusive zone, so it overlays without pushing other windows aside. It names no output: the compositor puts it on
//! its current one, where the user is (upstream's pointer-screen rule, see [`super::placement::sizing_monitor`]).

use gtk::prelude::GtkWindowExt;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use super::placement::LayerPlacement;

/// The layer surface's namespace, for compositor rules (niri's `layer-rule`, sway's `layer_effects`).
const NAMESPACE: &str = "agterm-quick-terminal";

/// Whether the compositor offers layer-shell; false on X11 and on Wayland compositors without it.
pub fn supported() -> bool {
    gtk4_layer_shell::is_supported()
}

/// Makes the not yet realized `window` a layer surface.
pub fn init(window: &gtk::Window) {
    window.init_layer_shell();
    window.set_namespace(Some(NAMESPACE));
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    window.set_exclusive_zone(0);
    window.set_monitor(None);
}

/// Anchors and sizes the layer surface as `placement` says.
pub fn apply(window: &gtk::Window, placement: LayerPlacement) {
    let filled = placement == LayerPlacement::Filled;
    window.set_anchor(Edge::Top, true);
    for edge in [Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, filled);
    }
    match placement {
        LayerPlacement::Centered {
            width,
            height,
            top_margin,
        } => {
            window.set_margin(Edge::Top, top_margin);
            window.set_default_size(width, height);
        }
        LayerPlacement::Filled => window.set_margin(Edge::Top, 0),
    }
}
