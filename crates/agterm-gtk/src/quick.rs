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
//! Sizing: Wayland gives a client no position, so the panel is sized, not placed: [`quick_metrics::panel_size`] of the
//! monitor the active agterm window is on (the pointer's screen upstream, which Wayland does not expose), on every
//! show, so a settings change lands on the next summon. The zoom (`surface.zoom --target quick`) maximizes the panel,
//! filling the monitor's work area as upstream fills the screen's visible frame.
//!
//! Losing the keyboard dismisses only a panel the user summoned ([`Core::quick_lost_focus`]); the window asked to
//! close (the compositor's close) hides the panel, its shell kept.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use agterm_core::Core;
use agterm_core::pane_env;
use agterm_core::quick_metrics::{self, PanelSize};
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::deck::PaneContext;
use crate::pane::PaneSurface;
use crate::state::{self, AppState, Poster};
use crate::surface::{SurfaceSpec, TerminalSurface};
use crate::windows::Windows;

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
            let size = panel_size(core, windows);
            panel.show(size, core.quick_zoomed());
        } else {
            panel.hide();
        }
    }
}

/// The panel window and its terminal.
struct Panel {
    window: gtk::Window,
    terminal: TerminalSurface,
    /// Whether the window is shown.
    shown: bool,
    /// Whether the window is maximized for the quick zoom.
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
        let holds_key = Rc::new(Cell::new(false));
        connect_terminal(&terminal, poster);
        connect_window(&window, &holds_key, poster);
        crate::keymap_dispatch::install(&window, crate::keymap_dispatch::KeyOrigin::Quick);
        Self {
            window,
            terminal,
            shown: false,
            zoomed: false,
            holds_key,
        }
    }

    /// Shows the panel at `size`, or maximized while `zoomed`; a shown panel only follows a zoom change.
    fn show(&mut self, size: Option<(i32, i32)>, zoomed: bool) {
        if self.shown && self.zoomed == zoomed {
            return;
        }
        if zoomed {
            self.window.maximize();
        } else {
            self.window.unmaximize();
            if let Some((width, height)) = size.filter(|_| !self.shown) {
                self.window.set_default_size(width, height);
            }
        }
        self.zoomed = zoomed;
        if !self.shown {
            self.shown = true;
            self.terminal.set_deck_visible(true);
            self.window.present();
        }
        self.terminal.grab_focus();
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

/// The panel's size on the monitor of the active agterm window, else the first monitor; `None` when GTK reports no
/// monitor.
fn panel_size(core: &Core, windows: &Windows) -> Option<(i32, i32)> {
    let geometry = active_monitor(windows)?.geometry();
    let visible = PanelSize {
        width: f64::from(geometry.width()),
        height: f64::from(geometry.height()),
    };
    let size = quick_metrics::panel_size(visible, core.settings().effective_quick_terminal_size_percent());
    Some((pixels(size.width), pixels(size.height)))
}

fn active_monitor(windows: &Windows) -> Option<gdk::Monitor> {
    let display = gdk::Display::default()?;
    let under_active = windows
        .app()
        .active_window()
        .and_then(|window| window.surface())
        .and_then(|surface| display.monitor_at_surface(&surface));
    under_active.or_else(|| display.monitors().item(0).and_downcast::<gdk::Monitor>())
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
    fn panel_extents_round_and_stay_positive() {
        assert_eq!(pixels(1_007.999_999), 1008);
        assert_eq!(pixels(0.2), 1);
    }
}
