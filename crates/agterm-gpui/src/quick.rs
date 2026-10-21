//! The quick terminal panel (upstream's `QuickTerminalPanel` and `QuickTerminalController` in
//! `agterm/Views/QuickTerminal.swift`, `windows.md` > Quick terminal; port of the GTK build's `quick.rs`): one panel per
//! app, a window of its own that belongs to no agterm window, holding one terminal with the quick-terminal environment
//! ([`pane_env::quick_terminal`]: no session, window or workspace ids).
//!
//! The core owns the state ([`Core::quick_visible`], [`Core::quick_zoomed`]) and the terminal's slot; this follows it
//! after every main-loop task ([`QuickPanel::sync`], from the windows' sync). The terminal is created on the first
//! show, in the active session's directory, and installed in the core; it lives here, not in a window, so hiding keeps
//! the shell alive, typable and readable (`quick.type`, `quick.text`). GPUI cannot hide a window on Linux, so hiding
//! removes the panel's window and showing opens a new one ([`view::QuickView`], with a fresh view of the terminal).
//! The shell exiting drops the panel with its terminal; the next show starts both afresh.
//!
//! Host ([`QuickHost`], chosen once per run, logged): on Wayland a wlr-layer-shell surface ([`host`]), so the panel
//! floats over every window on its output and is never tiled, as upstream's floating `NSPanel`; a compositor without
//! layer-shell (GNOME/Mutter, weston) refuses it, and the panel is an ordinary undecorated window from then on, as it is
//! on X11 (GPUI opens a layer surface there as a plain window, so the backend decides, not the open's error) and on
//! every other platform.
//!
//! Sizing: [`agterm_core::quick_metrics::panel_size`] of the panel's display, on every show, so a settings change
//! lands on the next summon. The layer surface names no output as it comes on screen, so the compositor puts it on its
//! current one, usually the focused output (a Wayland client cannot read the pointer outside its own surfaces, so that
//! stands in for upstream's pointer-screen rule). It is framed against the frontmost agterm window's display (else the
//! first) until the platform reports the display it landed on, then refitted there once by reopening it named on that
//! output ([`placement::refits_on_enter`]); the zoom (`surface.zoom --target quick`) reopens it too, anchored to every
//! edge of the output it is on, as upstream fills the screen's visible frame. The window host is sized and centered on
//! the frontmost agterm window's display, and maximizes in place for the zoom. Display bounds are logical
//! (xdg-output's, GPUI patch `gpui-pre-linux` 0002), so a fractional-scale output frames the panel right.
//!
//! Losing the keyboard dismisses only a panel the user summoned ([`Core::quick_lost_focus`]); the window manager's
//! close request hides the panel, its shell kept, and so does the compositor closing the layer surface (its output
//! went away). While the panel holds the keyboard the decks keep their hands off focus ([`holds_keyboard`]). A removed
//! window reports no blur or deactivation, so a hide tells the terminal's program the focus left itself (the next
//! window's view continues its focus reports) and has the key monitor forget its consumed keys once no agterm window
//! holds the keyboard.

use std::path::PathBuf;
use std::rc::Rc;

use agterm_app::quick_placement::{self as placement, QuickHost};
use agterm_core::Core;
use agterm_core::chrome::Rgb;
use agterm_core::link_policy::ClickOrigin;
use agterm_core::pane_env;
use gpui_kit::{AnyWindowHandle, App, AppContext as _, DisplayId, Entity, PlatformDisplay, Subscription, WindowHandle};

use crate::deck::DeckEnv;
use crate::pane::PaneContext;
use crate::state::{AppState, Poster, post};
use crate::terminal::{Shared, Terminal, TerminalEvent, TerminalSpec};

mod host;
mod view;

#[cfg(test)]
mod tests;

pub use host::{Fit, NAMESPACE, TITLE, Target, first_host, refuses_layer_shell, window_options};
pub use view::{Links, QuickView, holds_keyboard};

/// What the panel's sync reads besides the core.
pub struct QuickEnv<'a> {
    pub env: &'a DeckEnv,
    /// The application id the panel window carries, as every agterm window does.
    pub app_id: &'a str,
    /// The frontmost agterm window, whose display frames a panel coming on screen.
    pub frontmost: Option<AnyWindowHandle>,
}

/// A shown panel's frame: how it is hosted and fitted, the display it was framed against and the one it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub host: QuickHost,
    pub fit: Fit,
    /// The display the frame was computed against.
    pub sized_for: Option<DisplayId>,
    /// The display the window is on, as the platform reported it.
    pub landed: Option<DisplayId>,
    /// The layer surface was opened on a named output (a refit or a zoom), so it is not refitted again.
    pub named: bool,
}

/// What the panel does next for the core's fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing: the panel is framed as asked.
    Keep,
    /// Open the panel: it is not on screen.
    Open,
    /// Reopen the layer surface named on this display (`None`: the compositor's choice): a zoom change or the refit to
    /// the display it landed on (no runtime anchor or margin setters in GPUI).
    Reopen(Option<DisplayId>),
    /// Maximize (true) or restore the window host in place for a zoom change.
    Zoom(bool),
}

/// The step for `fit` from the shown panel's `frame` (`None`: not on screen). A size setting changed while shown waits
/// for the next show, as upstream reads it then.
#[must_use]
pub fn step(frame: Option<&Frame>, fit: Fit) -> Step {
    let Some(frame) = frame else {
        return Step::Open;
    };
    let zoom_changed = frame.fit.zoomed != fit.zoomed;
    match frame.host {
        QuickHost::Window if zoom_changed => Step::Zoom(fit.zoomed),
        QuickHost::Window => Step::Keep,
        QuickHost::LayerShell if zoom_changed => Step::Reopen(frame.landed.or(frame.sized_for)),
        QuickHost::LayerShell => match frame.landed {
            Some(landed)
                if !frame.named && placement::refits_on_enter(&landed, frame.sized_for.as_ref(), true, fit.zoomed) =>
            {
                Step::Reopen(Some(landed))
            }
            _ => Step::Keep,
        },
    }
}

/// The app's one quick terminal panel.
#[derive(Default)]
pub struct QuickPanel {
    terminal: Option<QuickTerminal>,
    shown: Option<Shown>,
    /// The host of this run, chosen at the first show: the window host for good once a compositor refused a layer
    /// surface.
    host: Option<QuickHost>,
}

/// The long-lived terminal and its route to the model.
struct QuickTerminal {
    terminal: Entity<Terminal>,
    shared: Rc<Shared>,
    /// The focus its program was told of when the panel hid it, which the next window's view continues from
    /// (`None`: the next view starts afresh).
    reported: Option<bool>,
    _events: Subscription,
}

/// The panel's window on screen.
struct Shown {
    handle: WindowHandle<QuickView>,
    frame: Frame,
    links: Rc<Links>,
}

impl QuickPanel {
    /// Brings the panel in line with the core's quick state: drops a terminal whose shell exited, starts one on the
    /// first show (installing it in the core), then shows, refits, zooms or hides the panel.
    pub fn sync(&mut self, core: &mut Core, quick: &QuickEnv<'_>, cx: &mut App) {
        if !core.quick_has_surface() && self.terminal.take().is_some() {
            tracing::info!("quick terminal: the shell exited");
            self.close(cx);
        }
        if !core.quick_visible() {
            self.close(cx);
            return;
        }
        if self.terminal.is_none() {
            let terminal = QuickTerminal::spawn(core.quick_terminal_cwd(), quick.env, cx);
            core.install_quick_surface(Box::new(terminal.terminal.read(cx).surface()));
            self.terminal = Some(terminal);
        }
        let fit = Fit {
            size_percent: core.settings().effective_quick_terminal_size_percent(),
            zoomed: core.quick_zoomed(),
        };
        self.show(fit, quick, cx);
    }

    /// The terminal's state, which its views and its Surface share; `None` before the first show or after its shell
    /// exited.
    #[must_use]
    pub fn shared(&self) -> Option<&Rc<Shared>> {
        self.terminal.as_ref().map(|terminal| &terminal.shared)
    }

    /// The panel's window while it is on screen.
    #[must_use]
    pub fn window(&self) -> Option<WindowHandle<QuickView>> {
        self.shown.as_ref().map(|shown| shown.handle)
    }

    /// The shown panel's frame.
    #[must_use]
    pub fn frame(&self) -> Option<Frame> {
        self.shown.as_ref().map(|shown| Frame {
            landed: shown.links.landed.get(),
            ..shown.frame
        })
    }

    /// The app's config changed: the terminal takes it, keeping its live font size with `keep_zoom`.
    pub fn apply_config(&self, env: &DeckEnv, keep_zoom: bool, cx: &mut App) {
        let Some(terminal) = &self.terminal else {
            return;
        };
        let config = env.config_for(None);
        terminal.terminal.update(cx, |terminal, cx| {
            if keep_zoom {
                terminal.flip_config(config, cx);
            } else {
                terminal.reload_config(config, cx);
            }
        });
    }

    /// Shows the panel framed for `fit` (module docs).
    fn show(&mut self, fit: Fit, quick: &QuickEnv<'_>, cx: &mut App) {
        match step(self.frame().as_ref(), fit) {
            Step::Keep => {}
            Step::Open => {
                let display = estimate(quick.frontmost, cx);
                self.shown = self.open(fit, display, false, quick, cx);
            }
            Step::Reopen(on) => {
                let display = on.and_then(|id| target_of(id, cx));
                if on.is_some() && display.is_none() {
                    return;
                }
                self.remove(false, cx);
                self.shown = self.open(fit, display, display.is_some(), quick, cx);
            }
            Step::Zoom(zoomed) => self.zoom_window(fit, zoomed, cx),
        }
    }

    /// Maximizes or restores the window host for the zoom, and gives the terminal the keyboard.
    fn zoom_window(&mut self, fit: Fit, zoomed: bool, cx: &mut App) {
        let Some(shown) = self.shown.as_mut() else {
            return;
        };
        shown.frame.fit = fit;
        let zoomed_in_place = shown.handle.update(cx, |view, window, cx| {
            if window.is_maximized() != zoomed {
                window.zoom_window();
            }
            view.focus_terminal(window, cx);
        });
        if zoomed_in_place.is_err() {
            tracing::debug!("quick terminal: the window is gone before its zoom");
        }
    }

    /// Opens the panel's window for `fit` on `display` (named on it when `named`), focused on the terminal; the window
    /// host takes over when the compositor refuses a layer surface. `None` (logged) when nothing opened.
    fn open(
        &mut self,
        fit: Fit,
        display: Option<Target>,
        named: bool,
        quick: &QuickEnv<'_>,
        cx: &mut App,
    ) -> Option<Shown> {
        let QuickTerminal { terminal, reported, .. } = self.terminal.as_ref()?;
        let (terminal, reported) = (terminal.clone(), *reported);
        let attempt = Opener {
            terminal: &terminal,
            reported,
            background: terminal_background(quick.env),
            poster: &quick.env.poster,
            app_id: quick.app_id,
            fit,
            display: display.as_ref(),
            named,
        };
        let (host, result) = self.open_falling_back(&attempt, cx);
        let (handle, links) = match result {
            Ok(Some(window)) => window,
            Ok(None) => {
                tracing::warn!("quick terminal: no display to show the panel on");
                return None;
            }
            Err(error) => {
                tracing::error!(%error, "quick terminal: the panel did not open");
                return None;
            }
        };
        if handle
            .update(cx, |view, window, cx| view.focus_terminal(window, cx))
            .is_err()
        {
            tracing::debug!("quick terminal: the window is gone before its focus");
        }
        tracing::info!(host = host.name(), zoomed = fit.zoomed, named, "quick terminal: shown");
        Some(Shown {
            handle,
            frame: Frame {
                host,
                fit,
                sized_for: display.map(|display| display.id),
                landed: None,
                named,
            },
            links,
        })
    }

    /// Opens `attempt` on the run's host (chosen at the first show); a compositor refusing the layer surface moves the
    /// run to the window host, which is tried at once. The host that answered, and its answer.
    fn open_falling_back(&mut self, attempt: &Opener<'_>, cx: &mut App) -> (QuickHost, gpui_kit::Result<Opened>) {
        let host = *self.host.get_or_insert_with(|| {
            let host = first_host(cx.compositor_name());
            tracing::info!(host = host.name(), "quick terminal panel");
            host
        });
        let result = attempt.open(host, cx);
        if host != QuickHost::LayerShell || !refuses_layer_shell(&result) {
            return (host, result);
        }
        tracing::info!("quick terminal: the compositor has no layer-shell; the panel is a window from now on");
        self.host = Some(QuickHost::Window);
        (QuickHost::Window, attempt.open(QuickHost::Window, cx))
    }

    /// Hides the panel: its window goes, its terminal stays.
    fn close(&mut self, cx: &mut App) {
        self.remove(true, cx);
    }

    /// Removes the panel's window, for a hide (`hiding`) or a reopen, whose new window follows at once. A removed
    /// window reports neither a blur nor a deactivation (Wayland drops it before its keyboard leave arrives), so the
    /// panel settles what they would: a hide tells the program in the terminal that the focus left, and the key
    /// monitor forgets its consumed keys unless an agterm window takes the keyboard (their releases may land in
    /// another application now).
    fn remove(&mut self, hiding: bool, cx: &mut App) {
        let Some(shown) = self.shown.take() else {
            return;
        };
        shown.links.closing.set(true);
        // the window going takes the keyboard with it; a reopen's new window takes it back as it activates
        view::set_holds(false, cx);
        let removed = shown.handle.update(cx, |view, window, cx| {
            let terminal = view.terminal();
            let reported = if hiding {
                terminal.update(cx, |terminal, _| terminal.release_focus_reports())
            } else {
                None
            };
            window.remove_window();
            reported
        });
        if let Ok(reported) = removed {
            if let Some(terminal) = self.terminal.as_mut() {
                terminal.reported = reported;
            }
        } else {
            tracing::debug!("quick terminal: the window was gone already");
        }
        crate::window::check_keyboard_left(cx);
        tracing::info!("quick terminal: hidden");
    }
}

/// An opened panel window and what it shares with the panel; `None` when there was no frame for it.
type Opened = Option<(WindowHandle<QuickView>, Rc<Links>)>;

/// One attempt at the panel's window.
struct Opener<'a> {
    terminal: &'a Entity<Terminal>,
    /// The focus the terminal's program was told of last, which the window's view continues from.
    reported: Option<bool>,
    background: Rgb,
    poster: &'a Poster,
    app_id: &'a str,
    fit: Fit,
    display: Option<&'a Target>,
    named: bool,
}

impl Opener<'_> {
    /// Opens the panel on `host`: its window and what it shares with the panel, `None` when there is no frame for it.
    fn open(&self, host: QuickHost, cx: &mut App) -> gpui_kit::Result<Opened> {
        let Some(options) = window_options(host, self.fit, self.display, self.named, self.app_id) else {
            return Ok(None);
        };
        let links = Rc::new(Links::default());
        let (terminal, background, poster) = (self.terminal.clone(), self.background, self.poster.clone());
        let (sharing, reported) = (Rc::clone(&links), self.reported);
        let handle = cx.open_window(options, move |window, cx| {
            cx.new(|cx| {
                let view = QuickView::new(&terminal, background, sharing, poster, window, cx);
                view.terminal()
                    .update(cx, |terminal, _| terminal.continue_focus_reports(reported));
                view
            })
        })?;
        Ok(Some((handle, links)))
    }
}

impl QuickTerminal {
    /// The quick terminal's terminal: a login shell in `cwd` with the quick-terminal environment, its life routed to
    /// the model.
    fn spawn(cwd: String, env: &DeckEnv, cx: &mut App) -> Self {
        let spawn = (env.launcher)(&launch(cwd, &env.context));
        let config = env.config_for(None);
        let terminal = cx.new(|cx| Terminal::new(TerminalSpec::default(), config, spawn, cx));
        let shared = Rc::clone(terminal.read(cx).shared());
        let poster = env.poster.clone();
        let events = cx.subscribe(&terminal, move |_, event: &TerminalEvent, _| route(&poster, event));
        Self {
            terminal,
            shared,
            reported: None,
            _events: events,
        }
    }
}

/// What the quick terminal runs: a login shell in `cwd` with the quick-terminal environment.
#[must_use]
pub fn launch(cwd: String, context: &PaneContext) -> agterm_term::Launch {
    agterm_term::Launch {
        command: None,
        initial_input: None,
        working_directory: Some(PathBuf::from(cwd)),
        wait_after_command: false,
        env: pane_env::quick_terminal(&context.socket, &context.program_version)
            .into_iter()
            .collect(),
        env_remove: Vec::new(),
        live: None,
    }
}

/// Routes the terminal to the model as main-loop tasks: its shell exiting (or never starting) drops the panel
/// (upstream's `handleShellExit`), a keystroke counts as user activity in the active window (upstream's `onUserInput`),
/// a clicked link goes through the link policy as a click from no session (upstream's `.quick` origin).
fn route(poster: &Poster, event: &TerminalEvent) {
    match event {
        TerminalEvent::Pane(agterm_term::PaneEvent::Exited(_)) | TerminalEvent::SpawnFailed(_) => {
            post(poster, |state: &mut AppState| {
                state.core.quick_shell_exited();
                state.settle();
            });
        }
        TerminalEvent::Keystroke(_) => post(poster, AppState::quick_keystroke),
        TerminalEvent::OpenUrl(url) => {
            let url = url.clone();
            post(poster, move |state: &mut AppState| {
                state.follow_link(&url, ClickOrigin::Quick);
            });
        }
        _ => {}
    }
}

/// The terminal's background, which the panel paints behind it (upstream re-reads it on every show).
fn terminal_background(env: &DeckEnv) -> Rgb {
    let color = env.config().theme.background;
    Rgb::new(color.r, color.g, color.b)
}

/// The display a panel coming on screen is framed against ([`placement::sizing_monitor`]): the frontmost agterm
/// window's, else the first; `None` when the platform reports none.
fn estimate(frontmost: Option<AnyWindowHandle>, cx: &mut App) -> Option<Target> {
    let under_frontmost =
        frontmost.and_then(|handle| handle.update(cx, |_, window, cx| window.display(cx)).ok().flatten());
    let first = cx.displays().into_iter().next();
    placement::sizing_monitor(None, under_frontmost, first).map(|display| target(display.as_ref()))
}

/// Display `id` as a frame target, while the platform lists it.
fn target_of(id: DisplayId, cx: &App) -> Option<Target> {
    cx.displays()
        .into_iter()
        .find(|display| display.id() == id)
        .map(|display| target(display.as_ref()))
}

fn target(display: &dyn PlatformDisplay) -> Target {
    Target {
        id: display.id(),
        bounds: display.bounds(),
    }
}
