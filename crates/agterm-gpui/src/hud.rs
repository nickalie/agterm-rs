//! The HUD panel on a session's page (upstream's `overlayPanel` in `WindowContentView+Detail.swift`,
//! `OverlayPanelStyle`, the overlay surface factory in `agtermApp.swift`, `Ghostty/HudLinkClick.swift`; port of the GTK
//! build's `hud.rs`). Captions are HUDs too (`ngtermctl caption`): sticky, frameless panels.
//!
//! The panel is a passive terminal ([`ViewRole::Passive`]) running the bundled painter (`resources/hud/hud.sh`), which
//! re-reads the body file the core writes. The deck lays it out above every pane of its page ([`Place::Hud`]): each
//! open (a new slot generation) mounts a fresh terminal and a close drops it. The panel takes no focus and no clicks,
//! so the session underneath keeps both; a Ctrl-click (Linux's link modifier, upstream's ⌘-click) over it opens the
//! panel's link under the pointer and goes no further, and a Ctrl-hover over a link shows the pointing hand and the
//! link's underline. A terminal ask drawn above the panel keeps both gestures (it takes the pointer).
//!
//! Where the panel goes is a pure function of the slot and the bounds it is laid out in ([`PanelStyle`]).
//!
//! [`Place::Hud`]: crate::split::Place::Hud

use std::cell::Cell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use agterm_core::HUD_WINDOW_PADDING as WINDOW_PADDING;
use agterm_core::hud::{Band, HudPosition};
use agterm_core::link_policy::ClickOrigin;
use agterm_core::model::{BackgroundWatermark, OverlaySlot, Session, WatermarkKind};
use agterm_core::pane_env::{self, SessionPane};
use agterm_core::watermark;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, CursorStyle, DispatchPhase, Entity, HitboxBehavior, IntoElement, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Styled, Subscription, TextSystem, canvas, div, px,
};
use uuid::Uuid;

use crate::deck::DeckEnv;
use crate::split::{PanePlan, Rect, Slot};
use crate::state::{AppState, post};
use crate::terminal::{Shared, Terminal, TerminalEvent, TerminalSpec, TerminalView, ViewRole};

/// The bundled painter, written next to the state on first use.
const HELPER: &str = include_str!("../resources/hud/hud.sh");

/// A HUD keeps the opaque backing but drops the shadow for a stronger border and a tight radius (upstream's
/// `hudCornerRadius`, `hudBorderOpacity`).
const CORNER_RADIUS: f32 = 8.;
const BORDER_OPACITY: f64 = 0.30;

/// Writes the painter under `state_dir` (rewritten when it differs) and returns its path; `None` when it could not be
/// written, which leaves `session.hud.open` refused as upstream refuses a build without the helper.
#[must_use]
pub fn install_helper(state_dir: &Path) -> Option<PathBuf> {
    let directory = state_dir.join("hud");
    let path = directory.join("hud.sh");
    if std::fs::read_to_string(&path).is_ok_and(|current| current == HELPER) {
        return Some(path);
    }
    let written = std::fs::create_dir_all(&directory).and_then(|()| std::fs::write(&path, HELPER));
    match written {
        Ok(()) => Some(path),
        Err(error) => {
            tracing::error!(path = %path.display(), %error, "could not write the hud painter");
            None
        }
    }
}

/// The cell estimate a HUD over a session no terminal draws yet is sized with (upstream's
/// `ControlServer.cellSize(family:size:)`): a cell of `family` (the terminal's default face without one) at `size`
/// points, measured as the terminal measures its own, in logical pixels.
#[must_use]
pub fn cell_estimator(text: Arc<TextSystem>) -> agterm_core::CellEstimator {
    Box::new(move |family, size| {
        let mut font = agterm_core::ghostty_config::Font::default();
        if let Some(family) = family.filter(|family| !family.trim().is_empty()) {
            font.family = vec![family.to_owned()];
        }
        let installed = text.all_font_names();
        let fonts = crate::terminal::Fonts::resolve(&font, &|name| {
            installed.binary_search_by(|known| known.as_str().cmp(name)).is_ok()
        });
        let font_id = text.resolve_font(&fonts.regular);
        #[expect(clippy::cast_possible_truncation, reason = "a point size fits an f32")]
        let metrics = crate::terminal::cell_metrics(&text, font_id, size as f32, 1.0);
        (
            f64::from(metrics.cell_width).max(1.0),
            f64::from(metrics.cell_height).max(1.0),
        )
    })
}

/// The HUD's sizing and placement in the bounds it is laid out in (upstream's `OverlayPanelStyle` for a HUD).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelStyle {
    /// The pane fraction the panel takes across.
    pub width_fraction: f64,
    /// The pane fraction it takes down, when no height in points was measured.
    pub height_fraction: f64,
    /// The measured height, which wins over the fraction.
    pub height_points: Option<f64>,
    /// The pane fraction held clear at an anchored edge; none for a sticky panel.
    pub edge_margin: f64,
    pub position: HudPosition,
    /// The border and rounding; a frameless panel keeps only the backing.
    pub frame: bool,
}

impl PanelStyle {
    /// The style of the HUD in `slot`; `None` without one.
    #[must_use]
    pub fn of(slot: &OverlaySlot) -> Option<Self> {
        let hud = slot.hud.as_ref().filter(|_| slot.hud_active())?;
        // a percent is at most 100, so the conversion is exact
        #[expect(clippy::cast_precision_loss, reason = "a percent within 0...100")]
        let percent = |value: i64| value as f64 / 100.0;
        Some(Self {
            width_fraction: slot.size_percent.map_or(1.0, percent),
            height_fraction: percent(hud.height_percent),
            height_points: hud.height_points,
            edge_margin: if hud.spec.sticky {
                0.0
            } else {
                percent(HudPosition::EDGE_MARGIN_PERCENT)
            },
            position: hud.spec.position,
            frame: hud.spec.frame,
        })
    }

    fn panel_height(&self, pane_height: f64) -> f64 {
        self.height_points.unwrap_or(pane_height * self.height_fraction)
    }

    /// One axis' travel off center: half the room left after the panel and its edge margin, signed by the band.
    fn offset(&self, extent: f64, fraction: f64, band: Band) -> f64 {
        let free = (extent * ((1.0 - fraction) / 2.0 - self.edge_margin)).max(0.0);
        match band {
            Band::Middle => 0.0,
            Band::Leading => -free,
            Band::Trailing => free,
        }
    }

    /// The panel's rectangle in `pane` (upstream's `panelFrame(in:)`).
    #[must_use]
    pub fn panel_frame(&self, pane: Rect) -> Rect {
        let (width, height) = (f64::from(pane.width), f64::from(pane.height));
        let panel_width = width * self.width_fraction;
        let panel_height = self.panel_height(height);
        let center_x =
            f64::from(pane.x) + width / 2.0 + self.offset(width, self.width_fraction, self.position.horizontal_band());
        let center_y = f64::from(pane.y)
            + height / 2.0
            + self.offset(height, panel_height / height.max(1.0), self.position.vertical_band());
        Rect {
            x: crate::modal::pixels(center_x - panel_width / 2.0),
            y: crate::modal::pixels(center_y - panel_height / 2.0),
            width: crate::modal::pixels(panel_width).max(0),
            height: crate::modal::pixels(panel_height).max(0),
        }
    }
}

/// What the page's layout needs to place the panel: its style and the pane it is scoped to (`None`: the whole page).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HudPlacement {
    pub style: PanelStyle,
    pub scope: Option<Slot>,
}

/// The panel's own config: its background at the window's `opacity` (baked in at open, upstream's
/// `applyOverlayBackgroundColor`), and the padding it was measured with, pinned against the user's `window-padding-*`
/// (upstream's `hudPaddingConfig`).
#[must_use]
pub fn config_overlay(background: Option<&str>, opacity: f64) -> String {
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
    let mut text = watermark::overlay_text(color.as_ref(), None, None, opacity);
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let _ = write!(
        text,
        "window-padding-x = {}\nwindow-padding-y = {}\nwindow-padding-balance = true\n",
        WINDOW_PADDING.0, WINDOW_PADDING.1
    );
    text
}

/// Whether `modifiers` hold the link modifier alone: Ctrl, Ghostty's link modifier on Linux (upstream's ⌘).
#[must_use]
pub fn is_link_modifier(modifiers: &Modifiers) -> bool {
    modifiers.control && !modifiers.shift && !modifiers.alt && !modifiers.platform && !modifiers.function
}

// MARK: - the panel on its page

/// The page's HUD: the slot generation its terminal belongs to, the terminal and its passive view.
pub struct HudPanel {
    generation: u64,
    terminal: Entity<Terminal>,
    view: Entity<TerminalView>,
    shared: Rc<Shared>,
    /// The panel's own config overlay (its background and padding), loaded over the app's config.
    overlay: String,
    /// A Ctrl-hover shows the pointing hand.
    hand: Rc<Cell<bool>>,
    _events: Subscription,
}

impl HudPanel {
    #[must_use]
    pub fn terminal(&self) -> &Entity<Terminal> {
        &self.terminal
    }

    #[must_use]
    pub fn view(&self) -> &Entity<TerminalView> {
        &self.view
    }

    /// The slot generation the panel belongs to.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn teardown(&self) {
        self.shared.teardown();
    }

    /// The app's config changed: the panel takes it under its own overlay, keeping the size it was measured at
    /// (upstream's `keepStartFontSize`: a reload restates the panel's own size).
    pub fn apply_config(&self, env: &DeckEnv, cx: &mut gpui_kit::App) {
        let config = env.config_for(Some(&self.overlay));
        self.terminal
            .update(cx, |terminal, cx| terminal.flip_config(config, cx));
    }
}

/// The page's HUD state the deck keeps: the panel, where it goes and whether it shows.
#[derive(Default)]
pub struct HudLayer {
    panel: Option<HudPanel>,
    placement: Option<HudPlacement>,
    visible: bool,
}

impl HudLayer {
    /// The mounted panel.
    #[must_use]
    pub fn panel(&self) -> Option<&HudPanel> {
        self.panel.as_ref()
    }

    /// Where the panel goes, while there is one to place.
    #[must_use]
    pub fn placement(&self) -> Option<HudPlacement> {
        self.placement
    }

    /// Whether the panel shows.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Drops a terminal whose generation is not `live` (closed or replaced; the model tore it down already).
    fn retain(&mut self, live: Option<u64>) {
        if self.panel.as_ref().is_some_and(|panel| Some(panel.generation) != live)
            && let Some(panel) = self.panel.take()
        {
            panel.teardown();
        }
    }
}

/// What the panel's terminal is spawned with.
pub struct HudContext<'a> {
    pub window: Uuid,
    pub workspace: Uuid,
    pub env: &'a Rc<DeckEnv>,
}

/// Brings the page's panel in line with `session`'s slot: a closed or replaced HUD's terminal goes, a HUD without one
/// gets it (installed in the slot), and the panel shows while the page does and its pane, when scoped, is laid out.
/// `plan` is the page's layout.
pub fn sync_page(
    layer: &mut HudLayer,
    session: &mut Session,
    hud: &HudContext<'_>,
    plan: Option<&PanePlan>,
    shown: bool,
    cx: &mut Context<crate::deck::Deck>,
) {
    let slot = &session.overlay;
    let live = slot.hud_active().then_some(slot.generation);
    layer.retain(live);
    let Some(generation) = live else {
        layer.placement = None;
        layer.visible = false;
        return;
    };
    if layer.panel.is_none() && !session.overlay.surface.is_present() {
        let panel = spawn(session, generation, hud, cx);
        let surface = panel.terminal.read(cx).surface();
        layer.panel = Some(panel);
        session.overlay.surface.replace(Box::new(surface));
    }
    let Some(style) = PanelStyle::of(&session.overlay) else {
        return;
    };
    let scope = session.hud_target_pane().map(Slot::of_overlay);
    // a scoped panel mounts only while its pane is laid out (upstream's `hudCanMount`)
    let laid_out = plan.is_some_and(|plan| scope.is_none_or(|slot| plan.shows(slot)));
    layer.placement = Some(HudPlacement { style, scope });
    layer.visible = shown && laid_out;
    if let Some(panel) = &layer.panel {
        let visible = layer.visible;
        panel.view.update(cx, |view, cx| view.set_shown(visible, cx));
    }
}

/// The panel's terminal: the painter in the session's directory, with the session's environment plus the body file,
/// at the HUD's font size, padded as the panel was measured, on the HUD's own background.
fn spawn(session: &Session, generation: u64, hud: &HudContext<'_>, cx: &mut Context<crate::deck::Deck>) -> HudPanel {
    let slot = &session.overlay;
    let state = slot.hud.as_ref();
    let mut env: Vec<(String, String)> = pane_env::session(&SessionPane {
        session: session.id(),
        window: Some(hud.window),
        workspace: Some(hud.workspace),
        socket_path: &hud.env.context.socket,
        program_version: &hud.env.context.program_version,
        pane: None,
        pane_token: None,
    })
    .into_iter()
    .collect();
    if let Some(state) = state {
        env.push((
            agterm_core::hud::FILE_ENV_KEY.to_owned(),
            state.file.to_string_lossy().into_owned(),
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a point size within the HUD's 6...72 range"
    )]
    let font_size = state.and_then(|state| state.font_size).map(|size| size as f32);
    let launch = agterm_term::Launch {
        command: slot.command.clone(),
        initial_input: None,
        working_directory: Some(PathBuf::from(session.effective_cwd())),
        wait_after_command: false,
        env,
        env_remove: Vec::new(),
        live: None,
    };
    let overlay = config_overlay(
        slot.background_color.as_deref(),
        crate::chrome::translucency(cx).opacity,
    );
    let config = hud.env.config_for(Some(&overlay));
    let spec = TerminalSpec {
        token: String::new(),
        live: false,
        font_size,
    };
    let spawn = (hud.env.launcher)(&launch);
    let terminal = cx.new(|cx| Terminal::new(spec, config, spawn, cx));
    let view = cx.new(|cx| TerminalView::new(&terminal, ViewRole::Passive, cx));
    let (poster, id) = (hud.env.poster.clone(), session.id());
    let events = cx.subscribe(&terminal, move |_, _, event: &TerminalEvent, _| {
        panel_event(&poster, id, generation, event);
    });
    let shared = Rc::clone(terminal.read(cx).shared());
    HudPanel {
        generation,
        terminal,
        view,
        shared,
        overlay,
        hand: Rc::new(Cell::new(false)),
        _events: events,
    }
}

/// The painter exiting (its body file gone) closes the slot it occupies, unless a replacement took it; the panel
/// coming up, and each change of its cell after (a config reload's font), measures it again with the cell really drawn
/// (upstream's `onGridChange`); a link the panel opens goes through the link policy as a HUD's.
fn panel_event(poster: &crate::state::Poster, session: Uuid, generation: u64, event: &TerminalEvent) {
    match event {
        TerminalEvent::Pane(agterm_term::PaneEvent::Exited(_)) => post(poster, move |state: &mut AppState| {
            if state.core.overlay_exited(session, generation) {
                state.settle();
            }
        }),
        TerminalEvent::Spawned | TerminalEvent::FontSize => post(poster, move |state: &mut AppState| {
            if state.core.refresh_hud(session) {
                state.settle();
            }
        }),
        TerminalEvent::OpenUrl(url) => {
            let url = url.clone();
            post(poster, move |state: &mut AppState| {
                state.follow_link(&url, ClickOrigin::Hud);
            });
        }
        _ => {}
    }
}

/// The panel's element: the passive terminal in its frame (an inset hairline and a tight radius, none for a frameless
/// panel), with the link gestures over it; drawn only while it shows.
#[must_use]
pub fn element(layer: &HudLayer) -> Option<impl IntoElement> {
    let panel = layer.panel.as_ref()?;
    let placement = layer.placement?;
    let frame = placement.style.frame;
    let border = crate::chrome::color(agterm_core::chrome::Rgb::WHITE, BORDER_OPACITY);
    Some(
        div()
            .size_full()
            .relative()
            .overflow_hidden()
            .when(frame, |panel| panel.rounded(px(CORNER_RADIUS)))
            .when(!layer.visible, Styled::invisible)
            .child(panel.view.clone())
            .when(frame, |panel| {
                panel.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(CORNER_RADIUS))
                        .border_1()
                        .border_color(border),
                )
            })
            .when(layer.visible, |panel_element| {
                panel_element.child(link_input(panel.view.clone(), Rc::clone(&panel.hand)))
            }),
    )
}

/// The Ctrl-click and Ctrl-hover over the panel (upstream's `HudLinkClick`): a hitbox that takes no pointer from what
/// lies under it, its pointing hand while the link modifier alone hovers a link, and a Ctrl-press over it that opens
/// the panel's link there and stops (a terminal ask over the panel occludes the hitbox and keeps both).
fn link_input(view: Entity<TerminalView>, hand: Rc<Cell<bool>>) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            if hand.get() && hitbox.is_hovered(window) {
                window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
            }
            let (pressing, over) = (view.clone(), hitbox.clone());
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture
                    || event.button != MouseButton::Left
                    || !is_link_modifier(&event.modifiers)
                    || !over.is_hovered(window)
                {
                    return;
                }
                cx.stop_propagation();
                pressing.update(cx, |view, cx| {
                    view.open_link_under(event.position, bounds, event.modifiers, cx);
                });
            });
            let (hovering, over, hand) = (view.clone(), hitbox, Rc::clone(&hand));
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let live = over.is_hovered(window) && is_link_modifier(&event.modifiers);
                let link = hovering.update(cx, |view, cx| {
                    view.hover_link_under(live.then_some(event.position), bounds, event.modifiers, cx)
                });
                if hand.replace(link) != link {
                    window.refresh();
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

#[cfg(test)]
mod tests;
