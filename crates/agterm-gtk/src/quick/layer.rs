//! The quick terminal's layer-shell host, through `gtk4-layer-shell`: a surface on the overlay layer (a fullscreen
//! window covers the top layer on wlroots compositors such as sway, and upstream's panel floats over full-screen apps),
//! taking the keyboard on demand (focus moves away from it, which is what dismisses a summoned panel), with no
//! exclusive zone, so it overlays without pushing other windows aside.
//!
//! Output: shown, the surface names none, so the compositor puts it on its current output (usually the focused one,
//! the stand-in for upstream's pointer-screen rule); hidden, it always names one. The library unrealizes the window of
//! a surface without an output on every monitor change, mapped or not (`gtk_layer_surface_remap` from its
//! `items-changed` handler on the display's monitors), and changing a shown surface's output does the same;
//! unrealizing the window would tear down the terminal for good (its `GtkGLArea` must stay realized, see
//! [`crate::surface`]). Hiding destroys the layer role, so a hidden surface changes output without a remap: the output
//! is cleared just before the panel comes on screen ([`release_output`]) and bound again as soon as it is hidden
//! ([`bind_hidden`]). On a monitor change our handlers run before the library's (the monitors' `items-changed`,
//! connected before the library's own in [`supported`], and the bound monitor's `invalidate`, ahead of the library's
//! after-handler): a shown panel, which names no output, is hidden on any change, after its owner is told, and bound
//! to the first monitor left before the library sees it; a hidden one moves only when its monitor went away
//! ([`super::placement::output_change`]); with none left it clears while hidden, and the next monitor to appear is
//! bound before the library sees a surface without an output.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use super::placement::{self, LayerPlacement, OutputChange};

/// The layer surface's namespace, for compositor rules (niri's `layer-rule`, sway's `layer_effects`).
const NAMESPACE: &str = "agterm-quick-terminal";

thread_local! {
    /// The layer panel whose output is kept (one per app).
    static PANEL: RefCell<Option<Rc<Binding>>> = const { RefCell::new(None) };
}

/// Whether the compositor offers layer-shell; false on X11 and on Wayland compositors without it. Watches the
/// monitors first, so our handler precedes the one the library connects in its first query (module docs); this is
/// the app's only entry into the library before [`init`].
pub fn supported() -> bool {
    watch_monitors();
    gtk4_layer_shell::is_supported()
}

/// Makes the not yet realized `window` a layer surface, bound to the first monitor while hidden. `on_forced_hide` runs
/// just before a shown panel is hidden because the monitors changed under it.
pub fn init(window: &gtk::Window, on_forced_hide: impl Fn() + 'static) {
    window.init_layer_shell();
    window.set_namespace(Some(NAMESPACE));
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    window.set_exclusive_zone(0);
    let binding = Rc::new(Binding {
        window: window.downgrade(),
        on_forced_hide: Box::new(on_forced_hide),
        invalidate: RefCell::new(None),
    });
    let weak = Rc::downgrade(&binding);
    window.connect_destroy(move |_| forget(&weak));
    if let Some(previous) = PANEL.with(|panel| panel.replace(Some(binding))) {
        previous.release();
    }
    keep_output(None);
}

/// Clears the hidden `window`'s output just before it is shown, so the compositor puts it on its current output
/// ([`placement::output_on_show`]); a shown one keeps its output (module docs).
pub fn release_output(window: &gtk::Window) {
    if window.is_mapped() {
        return;
    }
    let OutputChange::Move(next) = placement::output_on_show(LayerShell::monitor(window).as_ref()) else {
        return;
    };
    if let Some(binding) = current() {
        binding.bind(window, next.as_ref());
    }
}

/// Binds the just hidden `window` to a monitor, so no monitor change can unrealize it (module docs).
pub fn bind_hidden(window: &gtk::Window) {
    if !window.is_mapped() {
        keep_output(None);
    }
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

/// The panel's layer surface and the `invalidate` handler on the monitor it is bound to.
struct Binding {
    window: glib::WeakRef<gtk::Window>,
    on_forced_hide: Box<dyn Fn()>,
    invalidate: RefCell<Option<(gdk::Monitor, glib::SignalHandlerId)>>,
}

impl Binding {
    /// Binds the hidden `window` to `monitor` (`None`: the compositor chooses on show), watching that monitor go away.
    fn bind(&self, window: &gtk::Window, monitor: Option<&gdk::Monitor>) {
        self.release();
        window.set_monitor(monitor);
        if let Some(monitor) = monitor {
            let handler = monitor.connect_invalidate(|monitor| keep_output(Some(monitor)));
            *self.invalidate.borrow_mut() = Some((monitor.clone(), handler));
        }
    }

    /// Stops watching the bound monitor.
    fn release(&self) {
        if let Some((monitor, handler)) = self.invalidate.take() {
            monitor.disconnect(handler);
        }
    }
}

/// The binding of the live panel, if any.
fn current() -> Option<Rc<Binding>> {
    PANEL.with(|panel| panel.borrow().clone())
}

/// Drops the binding of a destroyed panel window, unless a newer panel replaced it.
fn forget(binding: &Weak<Binding>) {
    let Some(binding) = binding.upgrade() else {
        return;
    };
    let dropped = PANEL.with(|panel| {
        let mut panel = panel.borrow_mut();
        let ours = panel.as_ref().is_some_and(|live| Rc::ptr_eq(live, &binding));
        if ours { panel.take() } else { None }
    });
    if let Some(dropped) = dropped {
        dropped.release();
    }
}

/// Connects the monitors' `items-changed` handler, once per process.
fn watch_monitors() {
    thread_local! {
        static WATCHING: Cell<bool> = const { Cell::new(false) };
    }
    if WATCHING.with(|watching| watching.replace(true)) {
        return;
    }
    if let Some(display) = gdk::Display::default() {
        display.monitors().connect_items_changed(|_, _, _, _| keep_output(None));
    }
}

/// Keeps the panel on a usable monitor after the monitors changed (`dying`: the one being invalidated) or it was just
/// hidden: a hidden panel moves only when its monitor went away or it has none; a shown one, which names no output, is
/// hidden first (module docs).
fn keep_output(dying: Option<&gdk::Monitor>) {
    let Some(binding) = current() else {
        return;
    };
    let Some(window) = binding.window.upgrade() else {
        return;
    };
    let usable = usable_monitors(dying);
    let bound = LayerShell::monitor(&window);
    let OutputChange::Move(next) = placement::output_change(bound.as_ref(), &usable) else {
        return;
    };
    if window.is_mapped() {
        tracing::info!("the monitors changed under the quick terminal; hiding it");
        (binding.on_forced_hide)();
        window.set_visible(false);
    }
    binding.bind(&window, next.as_ref());
}

/// The display's valid monitors, without `dying`.
fn usable_monitors(dying: Option<&gdk::Monitor>) -> Vec<gdk::Monitor> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|position| monitors.item(position).and_downcast::<gdk::Monitor>())
        .filter(|monitor| monitor.is_valid() && Some(monitor) != dying)
        .collect()
}
