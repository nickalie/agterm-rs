//! The quick terminal panel (upstream's `QuickTerminalPanel` and `QuickTerminalController` views in
//! `agterm/Views/QuickTerminal.swift`, `windows.md` > Quick terminal): one panel per app, a separate undecorated
//! window that belongs to no agterm window, holding one terminal with the quick-terminal environment
//! ([`pane_env::quick_terminal`]: no session, window or workspace ids).
//!
//! The core owns the state ([`Core::quick_visible`], [`Core::quick_zoomed`]) and the terminal's slot; this follows it
//! after every commit. The panel and its terminal are created on the first show, in the active session's directory,
//! and installed in the core; hiding the panel only hides its window, which keeps the `GtkGLArea` realized, so the shell
//! stays alive and typable (a live area must never be unrealized or re-parented). Hidden, the terminal is occluded and
//! its GPU resources released, as a hidden deck page (upstream's `deckOnScreen = false` on `orderOut`). The shell
//! exiting drops the panel with its terminal; the next show builds both afresh.
//!
//! Host ([`placement::QuickHost`], chosen once per panel, logged): a wlr-layer-shell surface ([`layer`]) where the
//! compositor has one, so the panel floats over every window on the current output and is never tiled, as upstream's
//! floating `NSPanel`; elsewhere (GNOME/Mutter, X11) an ordinary undecorated window the compositor places like any
//! other.
//!
//! Sizing: [`agterm_core::quick_metrics::panel_size`] of the panel's monitor, on every show, so a settings change lands
//! on the next summon. The layer surface names no output and the compositor puts it on its current one, so it is sized
//! against the active agterm window's monitor until it lands, then re-fitted to the monitor it entered; it is centered
//! there ([`placement::layer_placement`]). The window is only sized (Wayland gives a client no position), against the
//! active agterm window's monitor. The zoom (`surface.zoom --target quick`) fills the output's usable area: the layer
//! surface anchors to every edge, the window maximizes, as upstream fills the screen's visible frame.
//!
//! Losing the keyboard dismisses only a panel the user summoned ([`Core::quick_lost_focus`]); the window asked to
//! close (the compositor's close) hides the panel, its shell kept.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use agterm_core::Core;
use agterm_core::pane_env;
use agterm_core::quick_metrics::PanelSize;
use gtk::prelude::*;
use gtk::{gdk, glib};

use placement::{LayerPlacement, QuickHost};

use crate::deck::PaneContext;
use crate::pane::PaneSurface;
use crate::state::{self, AppState, Poster};
use crate::surface::{SurfaceSpec, TerminalSurface};
use crate::windows::Windows;

mod layer;
mod placement;

/// The panel's style: a rounded frame with upstream's faint white border (12 pt radius, white at 18%) over a
/// transparent window, so the corners show what is behind.
const PANEL_CSS: &str = "
window.agterm-quick-terminal { background: transparent; }
.agterm-quick-terminal-frame {
  border-radius: 12px;
  border: 1px solid alpha(white, 0.18);
  background-color: @view_bg_color;
}
";

/// The app's one quick terminal panel; built on the first show.
#[derive(Default)]
pub struct QuickPanel {
    panel: Option<Panel>,
}

impl QuickPanel {
    /// Brings the panel in line with the core's quick state, after every commit: drops a panel whose shell exited,
    /// builds one on the first show (installing its terminal in the core), then shows, hides and zooms it.
    pub fn sync(&mut self, core: &mut Core, windows: &Windows, poster: &Poster) {
        if !core.quick_has_surface()
            && let Some(panel) = self.panel.take()
        {
            panel.window.destroy();
        }
        let visible = core.quick_visible();
        if visible && self.panel.is_none() {
            let panel = Panel::build(core, windows, poster);
            core.install_quick_surface(Box::new(PaneSurface::new(panel.terminal.clone(), String::new())));
            self.panel = Some(panel);
        }
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        if visible {
            let fit = Fit {
                size_percent: core.settings().effective_quick_terminal_size_percent(),
                zoomed: core.quick_zoomed(),
            };
            panel.show(active_monitor_size(windows), fit);
        } else {
            panel.hide();
        }
    }
}

/// What the panel's frame follows besides its monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fit {
    /// The user's size setting (`None`: the built-in default).
    size_percent: Option<i64>,
    /// The quick zoom.
    zoomed: bool,
}

/// The panel window and its terminal.
struct Panel {
    window: gtk::Window,
    terminal: TerminalSurface,
    host: QuickHost,
    /// Whether the window is shown.
    shown: bool,
    /// Whether the window fills its output for the quick zoom.
    zoomed: bool,
    /// The fit of the last show, read when the layer surface lands on a monitor.
    fit: Rc<Cell<Fit>>,
    /// Whether the panel holds the keyboard (upstream's `holdsKey`): only losing it after holding it is a focus loss,
    /// not the window going inactive because it was hidden.
    holds_key: Rc<Cell<bool>>,
}

impl Panel {
    fn build(core: &Core, windows: &Windows, poster: &Poster) -> Self {
        install_css();
        let terminal = TerminalSurface::new(surface_spec(core.quick_terminal_cwd(), windows.context()));
        let frame = gtk::Frame::builder()
            .name("quick-terminal")
            .css_classes(["agterm-quick-terminal-frame"])
            .overflow(gtk::Overflow::Hidden)
            .child(terminal.widget())
            .build();
        let window = gtk::Window::builder()
            .application(windows.app())
            .title("Quick Terminal")
            .decorated(false)
            .css_classes(["agterm-quick-terminal"])
            .child(&frame)
            .build();
        let host = QuickHost::select(layer::supported());
        tracing::info!(host = host.name(), "quick terminal panel");
        let fit = Rc::new(Cell::new(Fit {
            size_percent: None,
            zoomed: false,
        }));
        if host == QuickHost::LayerShell {
            layer::init(&window);
            refit_on_landing(&window, &fit);
        }
        let holds_key = Rc::new(Cell::new(false));
        connect_terminal(&terminal, poster);
        connect_window(&window, &holds_key, poster);
        crate::keymap_dispatch::install(&window, crate::keymap_dispatch::KeyOrigin::Quick);
        Self {
            window,
            terminal,
            host,
            shown: false,
            zoomed: false,
            fit,
            holds_key,
        }
    }

    /// Shows the panel framed on `monitor` (the estimate, see the module docs) as `fit` says; a shown panel only
    /// follows a zoom change.
    fn show(&mut self, monitor: Option<PanelSize>, fit: Fit) {
        if self.shown && self.zoomed == fit.zoomed {
            return;
        }
        match self.host {
            QuickHost::LayerShell => {
                self.fit.set(fit);
                if let Some(placement) = monitor_placement(monitor, fit) {
                    layer::apply(&self.window, placement);
                }
            }
            QuickHost::Window => self.frame_window(monitor, fit),
        }
        self.zoomed = fit.zoomed;
        if !self.shown {
            self.shown = true;
            self.terminal.set_deck_visible(true);
            self.window.present();
        }
        self.terminal.grab_focus();
    }

    /// Frames the window host: maximized while zoomed, else sized on `monitor` when it is coming on screen.
    fn frame_window(&self, monitor: Option<PanelSize>, fit: Fit) {
        if fit.zoomed {
            self.window.maximize();
            return;
        }
        self.window.unmaximize();
        if let Some(monitor) = monitor.filter(|_| !self.shown) {
            let (width, height) = placement::panel_pixels(monitor, fit.size_percent);
            self.window.set_default_size(width, height);
        }
    }

    /// Hides the panel: its window goes, its terminal stays realized, occluded and without GPU resources.
    fn hide(&mut self) {
        if !self.shown {
            return;
        }
        self.shown = false;
        self.holds_key.set(false);
        self.terminal.set_deck_visible(false);
        self.window.set_visible(false);
    }
}

/// What the quick terminal runs: a login shell in `cwd` with the quick-terminal environment.
fn surface_spec(cwd: String, context: &PaneContext) -> SurfaceSpec {
    SurfaceSpec {
        working_directory: Some(PathBuf::from(cwd)),
        command: None,
        env: pane_env::quick_terminal(&context.socket, &context.program_version)
            .into_iter()
            .collect(),
        wait_after_command: false,
        font_size: None,
    }
}

/// Routes the terminal to the model as main-loop tasks: its shell exiting drops the panel (upstream's
/// `handleShellExit`), and a keystroke counts as user activity in the active window (upstream's `onUserInput`). A
/// clicked link takes the quick origin through the terminal's own default (`TerminalSurface::open_url`).
fn connect_terminal(terminal: &TerminalSurface, poster: &Poster) {
    let posting = poster.clone();
    terminal.connect_exit(move || {
        state::post(&posting, |state: &mut AppState| {
            state.core.quick_shell_exited();
            state.settle();
        });
    });
    let posting = poster.clone();
    terminal.connect_keystroke(move |_| {
        state::post(&posting, |state: &mut AppState| {
            state.core.note_quick_input();
            state.arm_auto_follow();
        });
    });
}

/// Routes the window to the model: a close request hides the panel instead of destroying it, and losing the keyboard
/// after holding it dismisses a panel the user summoned.
fn connect_window(window: &gtk::Window, holds_key: &Rc<Cell<bool>>, poster: &Poster) {
    let posting = poster.clone();
    window.connect_close_request(move |_| {
        state::post(&posting, |state: &mut AppState| {
            state.core.dismiss_quick_terminal();
            state.settle();
        });
        glib::Propagation::Stop
    });
    let posting = poster.clone();
    let holds_key = Rc::clone(holds_key);
    window.connect_is_active_notify(move |window| {
        if window.is_active() {
            holds_key.set(true);
        } else if holds_key.replace(false) {
            state::post(&posting, |state: &mut AppState| {
                state.core.quick_lost_focus();
                state.settle();
            });
        }
    });
}

/// Registers [`PANEL_CSS`] on the default display, once per process.
fn install_css() {
    thread_local! {
        static INSTALLED: Cell<bool> = const { Cell::new(false) };
    }
    if INSTALLED.with(|installed| installed.replace(true)) {
        return;
    }
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(PANEL_CSS);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

/// The layer placement on `monitor` for `fit`; a zoomed panel needs no monitor, an unzoomed one without a monitor keeps
/// its frame.
fn monitor_placement(monitor: Option<PanelSize>, fit: Fit) -> Option<LayerPlacement> {
    if fit.zoomed {
        return Some(LayerPlacement::Filled);
    }
    monitor.map(|monitor| placement::layer_placement(monitor, fit.size_percent, false))
}

/// Re-fits the layer surface to each monitor it lands on: the compositor picks the output, which may not be the one
/// the panel was sized against.
fn refit_on_landing(window: &gtk::Window, fit: &Rc<Cell<Fit>>) {
    let fit = Rc::clone(fit);
    window.connect_realize(move |window| {
        let Some(surface) = window.surface() else {
            return;
        };
        let window = window.downgrade();
        let fit = Rc::clone(&fit);
        surface.connect_enter_monitor(move |_, monitor| {
            if let Some(window) = window.upgrade().filter(gtk::Window::is_visible)
                && let Some(placement) = monitor_placement(Some(monitor_size(monitor)), fit.get())
            {
                layer::apply(&window, placement);
            }
        });
    });
}

/// The size of the monitor the active agterm window is on, else of the first monitor; `None` when GTK reports no
/// monitor.
fn active_monitor_size(windows: &Windows) -> Option<PanelSize> {
    let display = gdk::Display::default()?;
    let under_active = windows
        .app()
        .active_window()
        .and_then(|window| window.surface())
        .and_then(|surface| display.monitor_at_surface(&surface));
    let first = display.monitors().item(0).and_downcast::<gdk::Monitor>();
    placement::sizing_monitor(None, under_active, first).map(|monitor| monitor_size(&monitor))
}

fn monitor_size(monitor: &gdk::Monitor) -> PanelSize {
    let geometry = monitor.geometry();
    PanelSize {
        width: f64::from(geometry.width()),
        height: f64::from(geometry.height()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quick_terminal_runs_a_login_shell_with_the_quick_environment() {
        let context = PaneContext {
            socket: "/run/agterm.sock".to_owned(),
            program_version: "0.36.0".to_owned(),
        };
        let spec = surface_spec("/home/u".to_owned(), &context);
        assert_eq!(spec.working_directory, Some(PathBuf::from("/home/u")));
        assert_eq!(spec.command, None);
        assert!(!spec.wait_after_command);
        let expected: Vec<(String, String)> = pane_env::quick_terminal("/run/agterm.sock", "0.36.0")
            .into_iter()
            .collect();
        assert_eq!(spec.env, expected);
        for name in [
            "AGTERM_SESSION_ID",
            "AGTERM_WINDOW_ID",
            "AGTERM_WORKSPACE_ID",
            "AGTERM_PANE_ID",
        ] {
            assert!(spec.env.iter().all(|(key, _)| key != name), "{name}");
        }
    }

    #[test]
    fn a_zoomed_layer_panel_fills_its_output_without_a_monitor() {
        let fit = Fit {
            size_percent: Some(50),
            zoomed: true,
        };
        assert_eq!(monitor_placement(None, fit), Some(LayerPlacement::Filled));
    }

    #[test]
    fn an_unzoomed_layer_panel_without_a_monitor_keeps_its_frame() {
        let fit = Fit {
            size_percent: None,
            zoomed: false,
        };
        assert_eq!(monitor_placement(None, fit), None);
        let monitor = PanelSize {
            width: 1920.0,
            height: 1080.0,
        };
        assert_eq!(
            monitor_placement(Some(monitor), fit),
            Some(placement::layer_placement(monitor, None, false))
        );
    }
}
