//! A caller's program over a session's page (upstream's `overlayPanel` and `paneOverlayPanel` in
//! `WindowContentView+Detail.swift`, `OverlayPanelStyle`, and the overlay surface factory in `agtermApp.swift`).
//!
//! The session-wide program runs in a framed layer that is a permanent child of the page's
//! [`crate::split::PaneStack`], added once: covering the page (chromeless, the panes and the scratch hidden under it)
//! or floating at its size over a backdrop that absorbs the clicks around it and mutes the session behind. A pane
//! overlay's terminal is a child of the stack itself, laid out over the pane it covers, which it hides; it is keyed by
//! the overlay's generation, so a swap or a promotion moves it with its pane. Each open mounts a fresh terminal and a
//! close drops it: no terminal ever changes parent.
//!
//! A page (`--html`/`--url`) takes the same places by the same keys, its panel from [`crate::html_overlay`] in place
//! of a terminal: the session-wide page in the program frame, covering or floating like a program, a pane page over
//! its pane. The registry owns the panel; a close here only takes it out of its parent.
//!
//! Every overlay terminal runs the program through upstream's wrapper (`agterm_core::overlay_capture`), which leaves
//! the exit status in a file the core reads when the terminal is torn down.

use std::path::PathBuf;

use agterm_core::model::{BackgroundWatermark, OverlayPane, Session, WatermarkKind};
use agterm_core::overlay_capture::{self, OverlayLaunchContext};
use agterm_core::pane_env::{self, SessionPane};
use agterm_core::watermark;
use gtk::gdk;
use gtk::prelude::*;
use uuid::Uuid;

use crate::hud::HudContext;
use crate::pane::PaneSurface;
use crate::split::{PanePlan, PaneView, Slot};
use crate::state::{self, AppState, Poster};
use crate::surface::{self, SurfaceSpec, TerminalSurface};

/// The program panel's style class, the floating variant's, and the backdrop's.
const PANEL_CLASS: &str = "agterm-overlay";
const FLOATING_CLASS: &str = "agterm-overlay-floating";
const BACKDROP_CLASS: &str = "agterm-overlay-backdrop";

/// A floating program is a window hovering over the session: a wide radius and a shadow carry the separation and a
/// faint border edges it (upstream's `floatingCornerRadius`, `floatingBorderOpacity`, `floatingShadowRadius`); the
/// backdrop washes the session it leaves visible. A full overlay is chromeless.
const CSS: &str = "
.agterm-overlay.agterm-overlay-floating {
  border-radius: 12px;
  outline: 1px solid alpha(white, 0.18);
  outline-offset: -1px;
  box-shadow: 0 0 24px alpha(black, 0.45);
}
.agterm-overlay-backdrop { background-color: alpha(black, 0.2); }
";

/// Where the program panel goes in the page: its share of the page across and down, `None` for the whole page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgramPlacement {
    pub fraction: Option<f64>,
}

/// What the program panel holds: the program's terminal, or a page's panel (owned by [`crate::html_overlay`]).
enum PanelContent {
    Terminal(TerminalSurface),
    Page { id: Uuid, widget: gtk::Widget },
}

/// The program panel's layer on a page and the terminal or page it holds.
pub struct ProgramLayer {
    backdrop: gtk::Box,
    frame: gtk::Box,
    /// The slot generation the mounted content belongs to, its focus key, and the content.
    panel: Option<(u64, String, PanelContent)>,
}

impl ProgramLayer {
    /// An empty, invisible layer; the page adds its two widgets to its stack once, the backdrop under the panel.
    #[must_use]
    pub fn new() -> Self {
        install_css();
        let backdrop = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        backdrop.add_css_class(BACKDROP_CLASS);
        let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        frame.add_css_class(PANEL_CLASS);
        frame.set_overflow(gtk::Overflow::Hidden);
        for widget in [&backdrop, &frame] {
            widget.set_can_target(false);
            widget.set_can_focus(false);
            widget.set_focusable(false);
            widget.set_opacity(0.0);
        }
        // the frame's `can-focus` and `can-target` gate its terminal (GTK keeps focus out of a non-focusable subtree
        // and picking skips an untargetable widget's whole subtree): `show` lets the keyboard and the pointer in while
        // the panel shows; the frame itself never takes focus (`focusable` stays false)
        Self {
            backdrop,
            frame,
            panel: None,
        }
    }

    #[must_use]
    pub fn backdrop(&self) -> &gtk::Box {
        &self.backdrop
    }

    #[must_use]
    pub fn frame(&self) -> &gtk::Box {
        &self.frame
    }

    /// The mounted terminal, with its focus key.
    #[must_use]
    pub fn focus(&self) -> Option<(&str, &TerminalSurface)> {
        match &self.panel {
            Some((_, key, PanelContent::Terminal(terminal))) => Some((key.as_str(), terminal)),
            _ => None,
        }
    }

    /// The mounted page's web view, with its focus key.
    #[must_use]
    pub fn page_focus(&self) -> Option<(String, gtk::Widget)> {
        match &self.panel {
            Some((_, key, PanelContent::Page { id, .. })) => {
                crate::html_overlay::focus_target(*id).map(|widget| (key.clone(), widget))
            }
            _ => None,
        }
    }

    /// The mounted terminal.
    #[must_use]
    pub fn terminal(&self) -> Option<&TerminalSurface> {
        self.focus().map(|(_, terminal)| terminal)
    }

    /// Drops content whose generation is not `live` (closed or replaced): a terminal the model tore down already, or
    /// a page's panel, which the page's release may have taken out already.
    fn retain(&mut self, live: Option<u64>) {
        if self
            .panel
            .as_ref()
            .is_none_or(|(generation, _, _)| Some(*generation) == live)
        {
            return;
        }
        match self.panel.take() {
            Some((_, _, PanelContent::Terminal(terminal))) => {
                terminal.teardown();
                self.frame.remove(terminal.widget());
            }
            Some((_, _, PanelContent::Page { widget, .. }))
                if widget.parent().as_ref() == Some(self.frame.upcast_ref()) =>
            {
                self.frame.remove(&widget);
            }
            _ => {}
        }
    }

    fn mount(&mut self, generation: u64, terminal: TerminalSurface) {
        self.frame.append(terminal.widget());
        self.panel = Some((
            generation,
            format!("overlay#{generation}"),
            PanelContent::Terminal(terminal),
        ));
    }

    /// Mounts page `id`'s panel, taking it from where it sat (a session that moved to another window).
    fn mount_page(&mut self, generation: u64, id: Uuid, widget: gtk::Widget) {
        unparent(&widget);
        self.frame.append(&widget);
        self.panel = Some((generation, format!("html#{id}"), PanelContent::Page { id, widget }));
    }

    /// Hides the panel and its backdrop.
    pub fn hide(&self) {
        self.show(None);
    }

    /// Shows the panel (floating or covering) or hides it: a shown panel's terminal takes the keyboard and the
    /// pointer, a floating panel's backdrop takes the clicks around it.
    fn show(&self, placement: Option<ProgramPlacement>) {
        let floating = placement.is_some_and(|placement| placement.fraction.is_some());
        self.frame.set_opacity(if placement.is_some() { 1.0 } else { 0.0 });
        self.frame.set_can_focus(placement.is_some());
        self.frame.set_can_target(placement.is_some());
        if floating {
            self.frame.add_css_class(FLOATING_CLASS);
        } else {
            self.frame.remove_css_class(FLOATING_CLASS);
        }
        self.backdrop.set_opacity(if floating { 1.0 } else { 0.0 });
        self.backdrop.set_can_target(floating);
        if let Some((_, _, PanelContent::Terminal(terminal))) = &self.panel {
            let visible = placement.is_some();
            if terminal.is_deck_visible() != visible {
                terminal.set_deck_visible(visible);
            }
        }
    }
}

/// Takes `widget` out of its parent, if any.
pub fn unparent(widget: &gtk::Widget) {
    if let Some(parent) = widget.parent() {
        match parent.downcast_ref::<gtk::Box>() {
            Some(container) => container.remove(widget),
            None => widget.unparent(),
        }
    }
}

impl Default for ProgramLayer {
    fn default() -> Self {
        Self::new()
    }
}

/// One pane overlay's terminal on a page, keyed by the overlay's generation.
pub struct PaneOverlayTerminal {
    pub generation: u64,
    pub key: String,
    pub terminal: TerminalSurface,
}

/// One pane page's panel on a page, keyed by the overlay's generation; the panel is the page registry's.
pub struct PanePage {
    pub generation: u64,
    pub key: String,
    pub id: Uuid,
    pub widget: gtk::Widget,
}

thread_local! {
    static CSS_INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn install_css() {
    if CSS_INSTALLED.with(std::cell::Cell::get) {
        return;
    }
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    CSS_INSTALLED.with(|installed| installed.set(true));
}

// MARK: - the page's sync

/// Brings the page's overlays in line with `session`: a closed or replaced program's terminal goes, an open program
/// without one gets it (installed in its slot), and likewise for each pane overlay whose pane is laid out; then each
/// shows where `plan` puts it while the page is `shown`. `plan` is `None` under the dashboard, which shows none.
pub fn sync_page(
    panes: &mut PaneView,
    session: &mut Session,
    context: &HudContext<'_>,
    plan: Option<&PanePlan>,
    shown: bool,
) {
    let live = sync_program(panes, session, context);
    let open: Vec<u64> = [OverlayPane::Left, OverlayPane::Right]
        .into_iter()
        .filter_map(|pane| session.pane_overlay(pane).map(|overlay| overlay.generation))
        .collect();
    panes.retain_pane_overlays(&open);
    for pane in [OverlayPane::Left, OverlayPane::Right] {
        mount_pane_overlay(panes, session, pane, context, plan);
    }
    let placement = plan
        .filter(|_| shown)
        .and_then(|plan| plan.overlays.program)
        .filter(|program| Some(program.generation) == live)
        .map(|program| ProgramPlacement {
            fraction: program.fraction,
        });
    panes.place_program(placement);
    panes.program_mut().show(placement);
    panes.show_pane_overlays(plan.filter(|_| shown));
    for (pane, page) in session.html_overlays() {
        let on_screen = match pane.and_then(|pane| session.pane_overlay(pane)) {
            Some(overlay) => panes.pane_page_shown(overlay.generation),
            None => placement.is_some(),
        };
        crate::html_overlay::apply(page, on_screen);
    }
}

/// Brings the session-wide slot's content in line with `session`: a closed or replaced program's terminal or page goes,
/// an open program without a terminal gets one (installed in the slot), an open page its panel. The live slot
/// generation, `None` while neither a program nor a page holds the slot.
fn sync_program(panes: &mut PaneView, session: &mut Session, context: &HudContext<'_>) -> Option<u64> {
    let slot = &session.overlay;
    let program = slot.program_active().then_some(slot.generation);
    let page = slot
        .page()
        .map(|page| (slot.generation, page.clone(), slot.background_color.clone()));
    let live = program.or(page.as_ref().map(|(generation, _, _)| *generation));
    panes.program_mut().retain(live);
    if let Some(generation) = program
        && panes.program().panel.is_none()
        && !session.overlay.surface.is_present()
    {
        let terminal = spawn(session, None, context);
        connect(&terminal, session.id(), generation, None, context.poster);
        panes.program_mut().mount(generation, terminal.clone());
        session
            .overlay
            .surface
            .replace(Box::new(PaneSurface::new(terminal, String::new())));
    }
    if let Some((generation, page, background)) = page
        && panes.program().panel.is_none()
        && let Some(widget) = crate::html_overlay::mount(&page, background.as_deref(), context.poster)
    {
        panes.program_mut().mount_page(generation, page.id, widget);
    }
    live
}

/// Mounts `pane`'s overlay once the plan lays that pane out (a pane no host lays out never gets one, as upstream's
/// surfaces wait for a backing size): a program's terminal, or a page's panel.
fn mount_pane_overlay(
    panes: &mut PaneView,
    session: &mut Session,
    pane: OverlayPane,
    context: &HudContext<'_>,
    plan: Option<&PanePlan>,
) {
    let Some(overlay) = session.pane_overlay(pane) else {
        return;
    };
    let generation = overlay.generation;
    let laid_out = plan.is_some_and(|plan| plan.overlays.pane(Slot::of_overlay(pane)) == Some(generation));
    if !laid_out || panes.pane_overlay(generation).is_some() || panes.pane_page(generation).is_some() {
        return;
    }
    if let Some(page) = &overlay.html {
        if let Some(widget) = crate::html_overlay::mount(page, overlay.background_color.as_deref(), context.poster) {
            panes.add_pane_page(PanePage {
                generation,
                key: format!("html#{}", page.id),
                id: page.id,
                widget,
            });
        }
        return;
    }
    if session.pane_overlays.slot(pane).surface.is_present() {
        return;
    }
    let terminal = spawn(session, Some(pane), context);
    connect(&terminal, session.id(), generation, Some(pane), context.poster);
    panes.add_pane_overlay(PaneOverlayTerminal {
        generation,
        key: format!("overlay-pane#{generation}"),
        terminal: terminal.clone(),
    });
    session
        .pane_overlays
        .slot_mut(pane)
        .surface
        .replace(Box::new(PaneSurface::new(terminal, String::new())));
}

/// The overlay terminal of the session-wide slot (`pane` `None`) or of `pane`'s slot: the wrapper running the
/// program in its directory (the caller's, else the session's), with the session's environment, the command and the
/// status file, at the session's font size, on the overlay's own background (upstream's `makeOverlaySurface`).
fn spawn(session: &Session, pane: Option<OverlayPane>, context: &HudContext<'_>) -> TerminalSurface {
    let (command, cwd, wait, background, code_file) =
        if let Some(overlay) = pane.and_then(|pane| session.pane_overlay(pane)) {
            (
                overlay.command.clone(),
                overlay.cwd.clone(),
                overlay.wait,
                overlay.background_color.clone(),
                overlay.code_file.clone(),
            )
        } else {
            let slot = &session.overlay;
            (
                slot.command.clone().unwrap_or_default(),
                slot.cwd.clone(),
                slot.wait,
                slot.background_color.clone(),
                slot.code_file.clone(),
            )
        };
    let session_env = pane_env::session(&SessionPane {
        session: session.id(),
        window: Some(context.window),
        workspace: Some(context.workspace),
        socket_path: &context.context.socket,
        program_version: &context.context.program_version,
        pane: None,
        pane_token: None,
    });
    let home = std::env::var("HOME").unwrap_or_default();
    let cwd = cwd.unwrap_or_else(|| session.local_working_directory(session.effective_cwd(), &home));
    let launch = OverlayLaunchContext::new(&command, &cwd, session_env);
    let code_file = code_file.unwrap_or_else(overlay_capture::mint_code_file);
    #[expect(clippy::cast_possible_truncation, reason = "a point size fits an f32")]
    let font_size = session.font_size.map(|size| size as f32);
    let terminal = TerminalSurface::new(SurfaceSpec {
        working_directory: Some(PathBuf::from(&launch.cwd)),
        command: Some(overlay_capture::surface_command()),
        env: launch.local_environment(&code_file, None).into_iter().collect(),
        wait_after_command: wait,
        font_size,
    });
    let config = config_overlay(background.as_deref());
    if !config.is_empty() {
        terminal.set_config_overlay(&config);
    }
    terminal
}

/// The overlay terminal's own config: its `#rrggbb` background, else nothing (the theme's).
#[must_use]
pub fn config_overlay(background: Option<&str>) -> String {
    let color = background.map(|hex| BackgroundWatermark {
        kind: WatermarkKind::Color,
        image_path: None,
        text: None,
        color_hex: Some(hex.to_owned()),
        opacity: None,
        fit: None,
        position: None,
        repeats: None,
    });
    watermark::overlay_text(color.as_ref(), None, None, 1.0)
}

/// Routes the overlay terminal's life to the model: its program ending closes the overlay it belongs to (by
/// generation, so a replacement or a moved pane overlay is found), a click into a pane overlay moves the split's focus
/// to its pane, and typing is user activity for auto-follow.
fn connect(terminal: &TerminalSurface, session: Uuid, generation: u64, pane: Option<OverlayPane>, poster: &Poster) {
    let posting = poster.clone();
    terminal.connect_exit(move || {
        state::post(&posting, move |state: &mut AppState| {
            let closed = match pane {
                Some(_) => state.core.pane_overlay_exited(session, generation),
                None => state.core.overlay_exited(session, generation),
            };
            if closed {
                state.settle();
            }
        });
    });
    if pane.is_some() {
        let posting = poster.clone();
        let key = terminal.key();
        terminal.connect_focus_in(move || {
            state::post(&posting, move |state: &mut AppState| {
                let current = surface::lookup(key)
                    .is_some_and(|terminal| terminal.widget().has_focus() && terminal.is_deck_visible());
                if current && state.core.pane_overlay_focused(session, generation) {
                    state.settle();
                }
            });
        });
    }
    let posting = poster.clone();
    terminal.connect_keystroke(move |_| {
        state::post(&posting, move |state: &mut AppState| {
            state.core.note_user_activity(session);
            state.arm_auto_follow();
        });
    });
    terminal.connect_open_url(|url| {
        crate::ghostty_app::follow_link(
            url,
            agterm_core::persist::settings::LinkOpenMode::Browser,
            agterm_core::link_policy::ClickOrigin::ProgramOverlay,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_paints_only_a_given_background() {
        assert!(config_overlay(None).is_empty());
        assert!(config_overlay(Some("#203040")).contains("background = #203040"));
    }
}
