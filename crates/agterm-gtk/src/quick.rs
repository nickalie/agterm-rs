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
//! compositor has one, so the panel floats over every window on its output and is never tiled, as upstream's floating
//! `NSPanel`; elsewhere (GNOME/Mutter, X11) an ordinary undecorated window the compositor places like any other.
//!
//! Sizing: [`agterm_core::quick_metrics::panel_size`] of the panel's monitor ([`placement::sizing_monitor`]), on every
//! show, so a settings change lands on the next summon. Coming on screen, the layer surface is put on the active agterm
//! window's monitor (else the first: a Wayland client cannot read the pointer outside its own surfaces) and centered
//! there ([`placement::layer_placement`]); while shown it never changes output, so a zoom change is fitted to the
//! monitor it is on. The window is only sized (Wayland gives a client no position), against the active agterm window's
//! monitor. The zoom (`surface.zoom --target quick`) fills the output's usable area: the layer surface anchors to every
//! edge, the window maximizes, as upstream fills the screen's visible frame.
//!
//! Losing the keyboard dismisses only a panel the user summoned ([`Core::quick_lost_focus`]); the window asked to
//! close (the compositor's close) hides the panel, its shell kept, and so does the layer panel's monitor going away
//! ([`layer`]).

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use agterm_core::Core;
use agterm_core::pane_env;
use agterm_core::quick_metrics::PanelSize;
use gtk::prelude::*;
use gtk::{gdk, glib};

use placement::QuickHost;

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
            panel.show(windows, fit);
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
    /// Whether the panel was shown and not hidden since. The layer host may have hidden the window already (its
    /// monitor went away), so the panel is on screen only while the window is visible too.
    shown: bool,
    /// Whether the window fills its output for the quick zoom.
    zoomed: bool,
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
        let holds_key = Rc::new(Cell::new(false));
        if host == QuickHost::LayerShell {
            layer::init(&window, lost_output_handler(&holds_key, poster));
        }
        connect_terminal(&terminal, poster);
        connect_window(&window, &holds_key, poster);
        crate::keymap_dispatch::install(&window, crate::keymap_dispatch::KeyOrigin::Quick);
        Self {
            window,
            terminal,
            host,
            shown: false,
            zoomed: false,
            holds_key,
        }
    }

    /// Shows the panel framed on its monitor (see the module docs) as `fit` says; a panel on screen only follows a
    /// zoom change.
    fn show(&mut self, windows: &Windows, fit: Fit) {
        let on_screen = self.shown && self.window.is_visible();
        if on_screen && self.zoomed == fit.zoomed {
            return;
        }
        let placed = if on_screen && self.host == QuickHost::LayerShell {
            layer::monitor(&self.window)
        } else {
            None
        };
        let monitor = panel_monitor(windows, placed);
        let framed = match self.host {
            QuickHost::LayerShell => self.frame_layer(monitor.as_ref(), fit, on_screen),
            QuickHost::Window => {
                self.frame_window(monitor.as_ref().map(monitor_size), fit, on_screen);
                true
            }
        };
        if !framed {
            return;
        }
        self.zoomed = fit.zoomed;
        if !on_screen {
            self.shown = true;
            self.terminal.set_deck_visible(true);
            self.window.present();
        }
        self.terminal.grab_focus();
    }

    /// Frames the layer host on `monitor`, putting it there first when it is coming on screen; false when there is no
    /// monitor to show it on (GTK cannot map a layer surface without one).
    fn frame_layer(&self, monitor: Option<&gdk::Monitor>, fit: Fit, on_screen: bool) -> bool {
        if !on_screen {
            let Some(monitor) = monitor else {
                tracing::warn!("no monitor to show the quick terminal on");
                return false;
            };
            layer::place(&self.window, monitor);
        }
        if let Some(placement) = placement::layer_placement(monitor.map(monitor_size), fit.size_percent, fit.zoomed) {
            layer::apply(&self.window, placement);
        }
        true
    }

    /// Frames the window host: maximized while zoomed, else sized on `monitor` when it is coming on screen.
    fn frame_window(&self, monitor: Option<PanelSize>, fit: Fit, on_screen: bool) {
        if fit.zoomed {
            self.window.maximize();
            return;
        }
        self.window.unmaximize();
        if let Some(monitor) = monitor.filter(|_| !on_screen) {
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

/// What the layer host does just before it hides a shown panel whose monitor went away: the panel no longer holds the
/// keyboard, and the core hides it as a dismissal, its shell kept.
fn lost_output_handler(holds_key: &Rc<Cell<bool>>, poster: &Poster) -> impl Fn() + 'static {
    let holds_key = Rc::clone(holds_key);
    let posting = poster.clone();
    move || {
        holds_key.set(false);
        state::post(&posting, |state: &mut AppState| {
            state.core.dismiss_quick_terminal();
            state.settle();
        });
    }
}

/// The monitor the panel is framed on ([`placement::sizing_monitor`]): `placed`, else the one the active agterm window
/// is on, else the first; `None` when GTK reports no monitor.
fn panel_monitor(windows: &Windows, placed: Option<gdk::Monitor>) -> Option<gdk::Monitor> {
    let display = gdk::Display::default()?;
    let under_active = windows
        .app()
        .active_window()
        .and_then(|window| window.surface())
        .and_then(|surface| display.monitor_at_surface(&surface));
    let first = display.monitors().item(0).and_downcast::<gdk::Monitor>();
    placement::sizing_monitor(placed, under_active, first)
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
}
