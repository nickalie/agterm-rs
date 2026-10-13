//! The quick terminal's layer-shell host, through `gtk4-layer-shell`: a surface on the overlay layer (a fullscreen
//! window covers the top layer on wlroots compositors such as sway, and upstream's panel floats over full-screen apps),
//! taking the keyboard on demand (focus moves away from it, which is what dismisses a summoned panel), with no
//! exclusive zone, so it overlays without pushing other windows aside.
//!
//! Output: the surface always names one, and changes it only while hidden. The library unrealizes the window of a
//! surface without an output on every monitor hotplug (`gtk_layer_surface_remap` from its `items-changed` handler on
//! the display's monitors), and changing a shown surface's output does the same; unrealizing the window would tear
//! down the terminal for good (its `GtkGLArea` must stay realized, see [`crate::surface`]). Hiding destroys the layer
//! role, so a hidden surface moves without that. The panel is put on [`super::placement::sizing_monitor`]'s monitor
//! as it comes on screen ([`place`]). When its monitor goes away, our handlers run before the library's (the monitors'
//! `items-changed`, connected before the library's own in [`supported`], and the monitor's `invalidate`, ahead of the
//! library's after-handler): a shown panel is hidden, after its owner is told, and the surface moves to the first
//! monitor left ([`super::placement::output_change`]); with none left it clears while hidden, and the next monitor to
//! appear is bound before the library sees a surface without an output.

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

/// Makes the not yet realized `window` a layer surface on the first monitor. `on_lost_output` runs just before a shown
/// panel is hidden because its monitor went away.
pub fn init(window: &gtk::Window, on_lost_output: impl Fn() + 'static) {
    window.init_layer_shell();
    window.set_namespace(Some(NAMESPACE));
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    window.set_exclusive_zone(0);
    let binding = Rc::new(Binding {
        window: window.downgrade(),
        on_lost_output: Box::new(on_lost_output),
        invalidate: RefCell::new(None),
    });
    let weak = Rc::downgrade(&binding);
    window.connect_destroy(move |_| forget(&weak));
    if let Some(previous) = PANEL.with(|panel| panel.replace(Some(binding))) {
        previous.release();
    }
    keep_output(None);
}

/// The monitor the layer surface is on (or goes to when shown).
pub fn monitor(window: &gtk::Window) -> Option<gdk::Monitor> {
    LayerShell::monitor(window)
}

/// Puts the hidden `window` on `monitor` for its next show; a shown one never changes output (module docs).
pub fn place(window: &gtk::Window, monitor: &gdk::Monitor) {
    if window.is_mapped() || LayerShell::monitor(window).as_ref() == Some(monitor) {
        return;
    }
    if let Some(binding) = current() {
        binding.bind(window, Some(monitor));
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
    on_lost_output: Box<dyn Fn()>,
    invalidate: RefCell<Option<(gdk::Monitor, glib::SignalHandlerId)>>,
}

impl Binding {
    /// Binds the hidden `window` to `monitor`, watching that monitor go away.
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

/// Keeps the panel on a usable monitor after the monitors changed (`dying`: the one being invalidated): it moves only
/// when its monitor went away, hidden first when shown (module docs).
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
        tracing::info!("the quick terminal's monitor went away; hiding it");
        (binding.on_lost_output)();
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
