//! A caller's program over a session's page (upstream's `overlayPanel` and `paneOverlayPanel` in
//! `WindowContentView+Detail.swift`, `OverlayPanelStyle`, and the overlay surface factory in `agtermApp.swift`; port of
//! the GTK build's `overlay.rs`).
//!
//! The session-wide program runs in a terminal the deck lays out over the page ([`Place::Program`]): covering it
//! (chromeless; the panes, the divider and the scratch hide under it) or floating at its share of the page, centered,
//! over the backdrop wash ([`crate::mute`]) that takes the clicks around it. A pane overlay's terminal lies over the pane
//! it covers ([`Place::Over`]), which it hides; it is keyed by the overlay's generation, so a swap or a promotion moves
//! it with its pane. Each open mounts a fresh terminal and a close drops it.
//!
//! A shown overlay terminal takes the pointer and, when the plan says it holds the keyboard
//! ([`crate::split::OverlayFocus`]), the keyboard in place of the pane; the deck gives it back to the pane once the
//! overlay goes. A pane overlay the user clicks into moves the split's focus to its pane.
//!
//! Every overlay terminal runs the program through upstream's wrapper (`agterm_core::overlay_capture`), which leaves
//! the exit status in a file the core reads when the slot closes (`session.overlay.result`).
//!
//! A page (`--html`/`--url`) takes the same places by the same keys, its element from [`crate::html_overlay`] in place
//! of a terminal: the session-wide page covering or floating as a program does, a pane page over its pane
//! ([`OverlayPage`]). The registry owns the page; a close here only lets the element go, and every sync gives each page
//! the model's value and whether it shows.
//!
//! [`Place::Program`]: crate::split::Place::Program
//! [`Place::Over`]: crate::split::Place::Over

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use agterm_core::keymap::SurfaceOwner;
use agterm_core::link_policy::ClickOrigin;
use agterm_core::model::{BackgroundWatermark, OverlayPane, Session, WatermarkKind};
use agterm_core::overlay_capture::{self, OverlayLaunchContext};
use agterm_core::pane_env::{self, SessionPane};
use agterm_core::watermark;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, BoxShadow, Context, Entity, FocusHandle, Focusable as _, IntoElement, ParentElement as _, Styled,
    Subscription, Window, div, point, px,
};
use uuid::Uuid;

use crate::deck::Deck;
use crate::hud::HudContext;
use crate::split::{PanePlan, Slot};
use crate::state::{AppState, Poster, post};
use crate::terminal::{KeyHold, Shared, Terminal, TerminalEvent, TerminalSpec, TerminalView, ViewRole};

/// A floating program is a window hovering over the session: a wide radius and a shadow carry the separation and a
/// faint border edges it (upstream's `floatingCornerRadius`, `floatingBorderOpacity`, `floatingShadowRadius`).
const FLOATING_RADIUS: f32 = 12.;
const FLOATING_BORDER_OPACITY: f64 = 0.18;
const FLOATING_SHADOW_RADIUS: f32 = 24.;
const FLOATING_SHADOW_OPACITY: f32 = 0.45;

/// Where the program panel goes in the page: its share of the page across and down, `None` for the whole page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgramPlacement {
    pub fraction: Option<f64>,
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

/// The focus key of the session-wide program's terminal of `generation`, or of pane overlay `generation`'s.
#[must_use]
pub fn focus_key(generation: u64, pane: bool) -> String {
    if pane {
        format!("overlay-pane#{generation}")
    } else {
        format!("overlay#{generation}")
    }
}

/// Whether a deck focus key names an overlay terminal rather than a pane.
#[must_use]
pub fn is_overlay_key(key: &str) -> bool {
    key.starts_with("overlay#") || key.starts_with("overlay-pane#")
}

/// One overlay terminal on a page: the slot generation it belongs to, its focus key, the terminal and its view.
pub struct OverlayTerminal {
    generation: u64,
    key: String,
    terminal: Entity<Terminal>,
    view: Entity<TerminalView>,
    shared: Rc<Shared>,
    /// The overlay's own config overlay (its background), loaded over the app's config.
    overlay: String,
    _subscriptions: Vec<Subscription>,
}

impl OverlayTerminal {
    /// The slot generation the terminal belongs to.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The deck's focus key for it.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    #[must_use]
    pub fn terminal(&self) -> &Entity<Terminal> {
        &self.terminal
    }

    #[must_use]
    pub fn view(&self) -> &Entity<TerminalView> {
        &self.view
    }

    #[must_use]
    pub fn shared(&self) -> &Rc<Shared> {
        &self.shared
    }

    #[must_use]
    pub fn focus_handle(&self, cx: &gpui_kit::App) -> FocusHandle {
        self.view.read(cx).focus_handle(cx)
    }

    /// The app's config changed: the terminal takes it under its own overlay.
    pub fn apply_config(&self, env: &crate::deck::DeckEnv, keep_zoom: bool, cx: &mut gpui_kit::App) {
        let config = env.config_for(Some(&self.overlay));
        self.terminal.update(cx, |terminal, cx| {
            if keep_zoom {
                terminal.flip_config(config, cx);
            } else {
                terminal.reload_config(config, cx);
            }
        });
    }

    fn show(&self, shown: bool, cx: &mut gpui_kit::App) {
        self.view.update(cx, |view, cx| {
            if view.is_shown() != shown {
                view.set_shown(shown, cx);
            }
        });
    }
}

/// An HTML page over a deck page ([`crate::html_overlay`]): the slot generation it belongs to, its focus key, its id
/// and its element, which the registry owns with the page.
pub struct OverlayPage {
    generation: u64,
    key: String,
    id: Uuid,
    element: Entity<crate::html_overlay::PageView>,
    /// It shows where the plan puts it.
    shown: bool,
    _focus: Subscription,
}

impl OverlayPage {
    /// The page's element.
    #[must_use]
    pub fn element(&self) -> &Entity<crate::html_overlay::PageView> {
        &self.element
    }

    /// The page's id.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// The slot generation it belongs to.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether it shows.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    fn focus_handle(&self, cx: &gpui_kit::App) -> FocusHandle {
        self.element.read(cx).focus_handle(cx)
    }
}

/// A page's overlays: the session-wide program's terminal or page, where it shows, and the pane overlays' terminals
/// and pages.
#[derive(Default)]
pub struct OverlayLayer {
    program: Option<OverlayTerminal>,
    placement: Option<ProgramPlacement>,
    panes: Vec<OverlayTerminal>,
    program_page: Option<OverlayPage>,
    pane_pages: Vec<OverlayPage>,
}

impl OverlayLayer {
    /// The session-wide program's terminal.
    #[must_use]
    pub fn program(&self) -> Option<&OverlayTerminal> {
        self.program.as_ref()
    }

    /// Where the program shows; `None` while it does not.
    #[must_use]
    pub fn placement(&self) -> Option<ProgramPlacement> {
        self.placement
    }

    /// The terminal of pane overlay `generation`.
    #[must_use]
    pub fn pane(&self, generation: u64) -> Option<&OverlayTerminal> {
        self.panes.iter().find(|overlay| overlay.generation == generation)
    }

    /// The pane overlays' terminals, in mount order.
    #[must_use]
    pub fn panes(&self) -> &[OverlayTerminal] {
        &self.panes
    }

    /// Every overlay terminal of the page.
    pub fn terminals(&self) -> impl Iterator<Item = &OverlayTerminal> {
        self.program.iter().chain(self.panes.iter())
    }

    /// The session-wide page.
    #[must_use]
    pub fn program_page(&self) -> Option<&OverlayPage> {
        self.program_page.as_ref()
    }

    /// The page over pane overlay slot `generation`.
    #[must_use]
    pub fn pane_page(&self, generation: u64) -> Option<&OverlayPage> {
        self.pane_pages.iter().find(|page| page.generation == generation)
    }

    /// The pane overlays' pages, in mount order.
    #[must_use]
    pub fn pane_pages(&self) -> &[OverlayPage] {
        &self.pane_pages
    }

    /// Whether the overlay keyed `key` (a terminal or a page) shows.
    #[must_use]
    pub fn shows(&self, key: &str, cx: &gpui_kit::App) -> bool {
        self.terminals()
            .any(|overlay| overlay.key() == key && overlay.view().read(cx).is_shown())
            || self
                .program_page
                .iter()
                .chain(&self.pane_pages)
                .any(|page| page.key == key && page.shown)
    }

    /// The overlay `plan` gives the keyboard, if mounted (a terminal or a page): its focus key and handle.
    #[must_use]
    pub fn focus(&self, plan: &PanePlan, cx: &gpui_kit::App) -> Option<(String, FocusHandle)> {
        match plan.overlays.focus? {
            crate::split::OverlayFocus::Program => {
                let generation = plan.overlays.program?.generation;
                if let Some(program) = self.program.as_ref().filter(|program| program.generation == generation) {
                    return Some((program.key.clone(), program.focus_handle(cx)));
                }
                let page = self
                    .program_page
                    .as_ref()
                    .filter(|page| page.generation == generation)?;
                Some((page.key.clone(), page.focus_handle(cx)))
            }
            crate::split::OverlayFocus::Pane(slot) => {
                let generation = plan.overlays.pane(slot)?;
                if let Some(overlay) = self.pane(generation) {
                    return Some((overlay.key.clone(), overlay.focus_handle(cx)));
                }
                let page = self.pane_page(generation)?;
                Some((page.key.clone(), page.focus_handle(cx)))
            }
        }
    }

    /// Hides every overlay terminal and page: the page is held out of the tree (a page off screen asks nothing, and a
    /// prompt it had goes, upstream's `setOnScreen(false)`).
    pub fn hide(&mut self, cx: &mut gpui_kit::App) {
        self.placement = None;
        for overlay in self.terminals() {
            overlay.show(false, cx);
        }
        for page in self.program_page.iter_mut().chain(&mut self.pane_pages) {
            page.shown = false;
            crate::html_overlay::hide(&page.element, cx);
        }
    }

    /// Drops the program terminal unless its generation is `live`, the session-wide page unless its generation is
    /// `page`, and the pane overlays and pages not in `open`: the model tore their surfaces down already (a close, a
    /// replacement) and the registry releases the pages, so this only lets the entities go.
    fn retain(&mut self, live: Option<u64>, page: Option<u64>, open: &[u64]) {
        if self
            .program
            .as_ref()
            .is_some_and(|program| Some(program.generation) != live)
            && let Some(program) = self.program.take()
        {
            program.shared.teardown();
        }
        if self
            .program_page
            .as_ref()
            .is_some_and(|shown| Some(shown.generation) != page)
        {
            self.program_page = None;
        }
        let (kept, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.panes)
            .into_iter()
            .partition(|overlay| open.contains(&overlay.generation));
        self.panes = kept;
        for overlay in gone {
            overlay.shared.teardown();
        }
        self.pane_pages.retain(|page| open.contains(&page.generation));
    }
}

/// What spawning an overlay terminal needs from the deck.
pub struct Mount<'a, 'w> {
    pub hud: &'a HudContext<'a>,
    pub window: &'w mut Window,
}

/// Brings the page's overlays in line with `session`: a closed or replaced program's terminal goes, an open program
/// without one gets it (installed in its slot), and likewise for each pane overlay whose pane `plan` lays out; then
/// each shows where `plan` puts it while the page is `shown`.
pub fn sync_page(
    layer: &mut OverlayLayer,
    session: &mut Session,
    mount: &mut Mount<'_, '_>,
    plan: &PanePlan,
    shown: bool,
    cx: &mut Context<Deck>,
) {
    let slot = &session.overlay;
    let live = slot.program_active().then_some(slot.generation);
    let page = slot.page().map(|_| slot.generation);
    let open: Vec<u64> = [OverlayPane::Left, OverlayPane::Right]
        .into_iter()
        .filter_map(|pane| session.pane_overlay(pane).map(|overlay| overlay.generation))
        .collect();
    layer.retain(live, page, &open);
    if let Some(generation) = live
        && layer.program.is_none()
        && !session.overlay.surface.is_present()
    {
        let program = spawn(session, None, generation, mount, cx);
        let surface = program.terminal.read(cx).surface();
        layer.program = Some(program);
        session.overlay.surface.replace(Box::new(surface));
    }
    if layer.program_page.is_none() {
        layer.program_page = mount_page(session, None, mount, cx);
    }
    for pane in [OverlayPane::Left, OverlayPane::Right] {
        mount_pane(layer, session, pane, mount, plan, cx);
    }
    let cover = live.or(page);
    layer.placement = plan
        .overlays
        .program
        .filter(|program| shown && Some(program.generation) == cover)
        .map(|program| ProgramPlacement {
            fraction: program.fraction,
        });
    if let Some(program) = &layer.program {
        program.show(layer.placement.is_some(), cx);
    }
    for overlay in &layer.panes {
        overlay.show(shown && slot_of(plan, overlay.generation).is_some(), cx);
    }
    apply_pages(layer, session, plan, shown, cx);
}

/// Gives every page of `session` the model's value and whether it shows where `plan` puts it (the GTK build's page
/// `apply` at the end of its overlay sync).
fn apply_pages(layer: &mut OverlayLayer, session: &Session, plan: &PanePlan, shown: bool, cx: &mut Context<Deck>) {
    let placed = layer.placement.is_some();
    if let Some(page) = &mut layer.program_page {
        page.shown = placed;
    }
    for page in &mut layer.pane_pages {
        page.shown = shown && slot_of(plan, page.generation).is_some();
    }
    for (pane, page) in session.html_overlays() {
        let on_screen = match pane.and_then(|pane| session.pane_overlay(pane)) {
            Some(overlay) => layer.pane_page(overlay.generation).is_some_and(OverlayPage::is_shown),
            None => placed,
        };
        crate::html_overlay::apply(page, on_screen, cx);
    }
}

/// The page of the session-wide slot (`pane` `None`) or of `pane`'s slot, mounted (its element, the registry's), with
/// the deck following its focus; `None` while the slot holds no page.
fn mount_page(
    session: &Session,
    pane: Option<OverlayPane>,
    mount: &mut Mount<'_, '_>,
    cx: &mut Context<Deck>,
) -> Option<OverlayPage> {
    let (generation, html, background) = match pane {
        None => {
            let slot = &session.overlay;
            (slot.generation, slot.page()?, slot.background_color.as_deref())
        }
        Some(pane) => {
            let overlay = session.pane_overlay(pane)?;
            (
                overlay.generation,
                overlay.html.as_ref()?,
                overlay.background_color.as_deref(),
            )
        }
    };
    let element = crate::html_overlay::mount(html, background, mount.window, cx)?;
    let key = focus_key(generation, pane.is_some());
    let handle = element.read(cx).focus_handle(cx);
    let (id, focused_key) = (session.id(), key.clone());
    // the deck records the page's focus; the page itself reports a pane page's split focus (`html_page_focused`)
    let focus = cx.on_focus(&handle, mount.window, move |deck, _, cx| {
        deck.overlay_focused(id, &focused_key, None, cx);
    });
    Some(OverlayPage {
        generation,
        key,
        id: html.id,
        element,
        shown: false,
        _focus: focus,
    })
}

/// The slot pane overlay `generation` shows over by `plan`.
#[must_use]
pub fn slot_of(plan: &PanePlan, generation: u64) -> Option<Slot> {
    [Slot::Primary, Slot::Split]
        .into_iter()
        .find(|slot| plan.overlays.pane(*slot) == Some(generation))
}

/// Mounts `pane`'s overlay once `plan` lays that pane out (a pane no host lays out never gets one, as upstream's
/// surfaces wait for a backing size): a program's terminal, or a page's element.
fn mount_pane(
    layer: &mut OverlayLayer,
    session: &mut Session,
    pane: OverlayPane,
    mount: &mut Mount<'_, '_>,
    plan: &PanePlan,
    cx: &mut Context<Deck>,
) {
    let Some(overlay) = session.pane_overlay(pane) else {
        return;
    };
    let generation = overlay.generation;
    let laid_out = plan.overlays.pane(Slot::of_overlay(pane)) == Some(generation);
    if !laid_out || layer.pane(generation).is_some() || layer.pane_page(generation).is_some() {
        return;
    }
    if overlay.html.is_some() {
        if let Some(page) = mount_page(session, Some(pane), mount, cx) {
            layer.pane_pages.push(page);
        }
        return;
    }
    if session.pane_overlays.slot(pane).surface.is_present() {
        return;
    }
    let terminal = spawn(session, Some(pane), generation, mount, cx);
    let surface = terminal.terminal.read(cx).surface();
    layer.panes.push(terminal);
    session.pane_overlays.slot_mut(pane).surface.replace(Box::new(surface));
}

/// What the overlay terminal of the session-wide slot (`pane` `None`) or of `pane`'s slot runs.
struct Program {
    command: String,
    cwd: Option<String>,
    wait: bool,
    background: Option<String>,
    code_file: Option<PathBuf>,
}

impl Program {
    fn of(session: &Session, pane: Option<OverlayPane>) -> Self {
        if let Some(overlay) = pane.and_then(|pane| session.pane_overlay(pane)) {
            return Self {
                command: overlay.command.clone(),
                cwd: overlay.cwd.clone(),
                wait: overlay.wait,
                background: overlay.background_color.clone(),
                code_file: overlay.code_file.clone(),
            };
        }
        let slot = &session.overlay;
        Self {
            command: slot.command.clone().unwrap_or_default(),
            cwd: slot.cwd.clone(),
            wait: slot.wait,
            background: slot.background_color.clone(),
            code_file: slot.code_file.clone(),
        }
    }
}

/// The launch of an overlay terminal: the wrapper running the program in its directory (the caller's, else the
/// session's), with the session's environment, the command and the status file (upstream's `makeOverlaySurface`).
fn launch(session: &Session, program: &Program, hud: &HudContext<'_>) -> agterm_term::Launch {
    let session_env = pane_env::session(&SessionPane {
        session: session.id(),
        window: Some(hud.window),
        workspace: Some(hud.workspace),
        socket_path: &hud.env.context.socket,
        program_version: &hud.env.context.program_version,
        pane: None,
        pane_token: None,
    });
    let home = std::env::var("HOME").unwrap_or_default();
    let cwd = program
        .cwd
        .clone()
        .unwrap_or_else(|| session.local_working_directory(session.effective_cwd(), &home));
    let context = OverlayLaunchContext::new(&program.command, &cwd, session_env);
    let code_file = program
        .code_file
        .clone()
        .unwrap_or_else(overlay_capture::mint_code_file);
    agterm_term::Launch {
        command: Some(overlay_capture::surface_command()),
        initial_input: None,
        working_directory: Some(PathBuf::from(&context.cwd)),
        wait_after_command: program.wait,
        env: context.local_environment(&code_file, None).into_iter().collect(),
        env_remove: Vec::new(),
    }
}

/// The overlay terminal of the session-wide slot (`pane` `None`) or of `pane`'s slot, at the session's font size, on
/// the overlay's own background, its life routed to the model.
fn spawn(
    session: &Session,
    pane: Option<OverlayPane>,
    generation: u64,
    mount: &mut Mount<'_, '_>,
    cx: &mut Context<Deck>,
) -> OverlayTerminal {
    let env = mount.hud.env;
    let program = Program::of(session, pane);
    let launch = launch(session, &program, mount.hud);
    let overlay = config_overlay(program.background.as_deref());
    let config = env.config_for(Some(&overlay));
    #[expect(clippy::cast_possible_truncation, reason = "a point size fits an f32")]
    let font_size = session.font_size.map(|size| size as f32);
    let spec = TerminalSpec {
        token: String::new(),
        live: false,
        font_size,
    };
    let spawn = (env.launcher)(&launch);
    let terminal = cx.new(|cx| Terminal::new(spec, config, spawn, cx));
    let shared = Rc::clone(terminal.read(cx).shared());
    let id = session.id();
    shared.set_owner(Some(SurfaceOwner::Overlay {
        session: id,
        generation,
        pane: pane.is_some(),
    }));
    let view = cx.new(|cx| {
        let mut view = TerminalView::new(&terminal, ViewRole::Owner, cx);
        view.set_shown(false, cx);
        view
    });
    let route = Route {
        poster: env.poster.clone(),
        session: id,
        generation,
        pane,
        shared: Rc::downgrade(&shared),
        held: Rc::new(Cell::new(false)),
    };
    let events = cx.subscribe(&terminal, move |_, _, event: &TerminalEvent, _| route.event(event));
    let key = focus_key(generation, pane.is_some());
    let handle = view.read(cx).focus_handle(cx);
    let focused_key = key.clone();
    let focus = cx.on_focus(&handle, mount.window, move |deck, _, cx| {
        deck.overlay_focused(id, &focused_key, pane.map(|_| generation), cx);
    });
    OverlayTerminal {
        generation,
        key,
        terminal,
        view,
        shared,
        overlay,
        _subscriptions: vec![events, focus],
    }
}

/// Where an overlay terminal's life goes: the model, through the main loop.
struct Route {
    poster: Poster,
    session: Uuid,
    generation: u64,
    pane: Option<OverlayPane>,
    shared: std::rc::Weak<Shared>,
    /// The program's exit is held on screen until a key closes the overlay.
    held: Rc<Cell<bool>>,
}

impl Route {
    /// The program ending closes the overlay it belongs to (by generation, so a replacement or a moved pane overlay
    /// is found); a held end (`--wait`) reports the hold and closes on the next key, as libghostty's prompt did. Typing
    /// is user activity for auto-follow; a clicked link goes through the link policy as a program overlay's.
    fn event(&self, event: &TerminalEvent) {
        let (session, generation, pane) = (self.session, self.generation, self.pane);
        match event {
            TerminalEvent::Pane(agterm_term::PaneEvent::Exited(status)) if status.held => {
                post(&self.poster, move |state: &mut AppState| {
                    if state.core.replica_overlay_held(session, pane, generation) {
                        state.settle();
                    }
                });
                self.hold_until_key();
            }
            TerminalEvent::Pane(agterm_term::PaneEvent::Exited(_)) => {
                post_exit(&self.poster, session, generation, pane);
            }
            TerminalEvent::Keystroke(_) => post(&self.poster, move |state: &mut AppState| {
                state.overlay_keystroke(session);
            }),
            TerminalEvent::OpenUrl(url) => {
                let url = url.clone();
                post(&self.poster, move |state: &mut AppState| {
                    state.follow_link(&url, ClickOrigin::ProgramOverlay);
                });
            }
            _ => {}
        }
    }

    /// Holds the terminal's keys: the next press closes the overlay.
    fn hold_until_key(&self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        self.held.set(true);
        let (poster, held) = (self.poster.clone(), Rc::clone(&self.held));
        let (session, generation, pane) = (self.session, self.generation, self.pane);
        let close_on_key: KeyHold = Rc::new(move || {
            if held.replace(false) {
                post_exit(&poster, session, generation, pane);
            }
        });
        shared.hold_keys(Some(close_on_key));
    }
}

/// Closes overlay `generation` of `session` (the session-wide slot, or `pane`'s) as its program ended.
fn post_exit(poster: &Poster, session: Uuid, generation: u64, pane: Option<OverlayPane>) {
    post(poster, move |state: &mut AppState| {
        let closed = match pane {
            Some(_) => state.core.pane_overlay_exited(session, generation),
            None => state.core.overlay_exited(session, generation),
        };
        if closed {
            state.settle();
        }
    });
}

/// The program panel's element: the terminal, chromeless while it covers the page, rounded, edged and shadowed while
/// it floats; drawn only while it shows.
#[must_use]
pub fn program_element(layer: &OverlayLayer) -> Option<impl IntoElement> {
    let content: gpui_kit::AnyElement = match (&layer.program, &layer.program_page) {
        (Some(program), _) => program.view.clone().into_any_element(),
        (None, Some(page)) => page.element.clone().into_any_element(),
        (None, None) => return None,
    };
    let floating = layer.placement.is_some_and(|placement| placement.fraction.is_some());
    let border = crate::chrome::color(agterm_core::chrome::Rgb::WHITE, FLOATING_BORDER_OPACITY);
    let shadow = BoxShadow {
        color: gpui_kit::hsla(0., 0., 0., FLOATING_SHADOW_OPACITY),
        offset: point(px(0.), px(0.)),
        blur_radius: px(FLOATING_SHADOW_RADIUS),
        spread_radius: px(0.),
        inset: false,
    };
    Some(
        div()
            .size_full()
            .relative()
            .when(layer.placement.is_none(), Styled::invisible)
            .when(floating, |panel| {
                panel.rounded(px(FLOATING_RADIUS)).shadow(vec![shadow])
            })
            .child(
                div()
                    .size_full()
                    .overflow_hidden()
                    .when(floating, |panel| panel.rounded(px(FLOATING_RADIUS)))
                    .child(content),
            )
            .when(floating, |panel| {
                panel.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(FLOATING_RADIUS))
                        .border_1()
                        .border_color(border),
                )
            }),
    )
}

/// A pane overlay's element: its terminal, drawn only while it shows.
#[must_use]
pub fn pane_element(overlay: &OverlayTerminal, cx: &gpui_kit::App) -> impl IntoElement {
    let shown = overlay.view.read(cx).is_shown();
    div()
        .size_full()
        .when(!shown, Styled::invisible)
        .child(overlay.view.clone())
}

/// A pane page's element, drawn only while it shows.
#[must_use]
pub fn pane_page_element(page: &OverlayPage) -> impl IntoElement {
    div()
        .size_full()
        .when(!page.shown, Styled::invisible)
        .child(page.element.clone())
}

#[cfg(test)]
mod tests;
