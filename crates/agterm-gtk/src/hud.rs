//! The HUD panel on a session's page (upstream's `overlayPanel` in `WindowContentView+Detail.swift`,
//! `OverlayPanelStyle`, the overlay surface factory in `agtermApp.swift`, `Ghostty/HudLinkClick.swift`).
//!
//! The panel is a passive terminal running the bundled painter (`resources/hud/hud.sh`), which re-reads the body file
//! the core writes. It sits in a framed layer that is a permanent child of the page's [`crate::split::PaneStack`],
//! added once, above every pane: each open (a new slot generation) mounts a fresh terminal into the layer and a close
//! drops it; no terminal ever changes parent. The panel takes no focus and no clicks, so the session underneath keeps
//! both; a Ctrl-click (Linux's link modifier, upstream's ⌘-click) over it is forwarded to the panel, which opens the
//! link under the pointer.
//!
//! Where the panel goes is a pure function of the slot and the bounds it is laid out in ([`PanelStyle`]).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use agterm_core::HUD_WINDOW_PADDING as WINDOW_PADDING;
use agterm_core::hud::{Band, HudPosition};
use agterm_core::model::{BackgroundWatermark, OverlaySlot, Session, WatermarkKind};
use agterm_core::pane_env::{self, SessionPane};
use agterm_core::watermark;
use agterm_ghostty::input::{Mods, MouseState};
use gtk::prelude::*;
use gtk::{gdk, glib};
use uuid::Uuid;

use crate::deck::PaneContext;
use crate::pane::PaneSurface;
use crate::split::{PanePlan, PaneView, Rect, Slot};
use crate::state::{self, AppState, Poster};
use crate::surface::{SurfaceSpec, TerminalSurface};

/// The bundled painter, written next to the state on first use.
const HELPER: &str = include_str!("../resources/hud/hud.sh");

/// The layer's style class, and the frameless variant's.
pub const LAYER_CLASS: &str = "agterm-hud";
const FRAMELESS_CLASS: &str = "agterm-hud-frameless";

/// A HUD keeps the opaque backing but drops the shadow for a stronger border and a tight radius (upstream's
/// `hudCornerRadius`, `hudBorderOpacity`). The border is an inset outline, so it takes no room from the terminal the
/// panel was sized for.
const CSS: &str = "
.agterm-hud { border-radius: 8px; outline: 1px solid alpha(white, 0.30); outline-offset: -1px; }
.agterm-hud.agterm-hud-frameless { border-radius: 0; outline: none; }
";

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

/// A cell of `family` (the default monospace face without one) at `size` points, measured with pango: a digit's
/// advance across and the line height down (upstream's `ControlServer.cellSize(family:size:)`), for a session no
/// terminal draws yet.
#[must_use]
pub fn estimate_cell(family: Option<&str>, size: f64) -> (f64, f64) {
    let context = gtk::Label::new(None).pango_context();
    let mut font = gtk::pango::FontDescription::new();
    font.set_family(family.unwrap_or("monospace"));
    #[expect(clippy::cast_possible_truncation, reason = "a point size in pango units fits an i32")]
    let units = (size * f64::from(gtk::pango::SCALE)).round() as i32;
    font.set_size(units);
    let metrics = context.metrics(Some(&font), None);
    let scale = f64::from(gtk::pango::SCALE);
    let width = f64::from(metrics.approximate_digit_width()) / scale;
    let height = match metrics.height() {
        height if height > 0 => f64::from(height) / scale,
        _ => f64::from(metrics.ascent() + metrics.descent()) / scale,
    };
    (width.max(1.0), height.max(1.0))
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
            x: rounded(center_x - panel_width / 2.0),
            y: rounded(center_y - panel_height / 2.0),
            width: rounded(panel_width).max(0),
            height: rounded(panel_height).max(0),
        }
    }
}

/// `value` rounded to whole pixels, saturating.
fn rounded(value: f64) -> i32 {
    let value = value.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX));
    #[expect(clippy::cast_possible_truncation, reason = "rounded and clamped into i32")]
    let pixels = value as i32;
    pixels
}

/// What the page's layout needs to place the panel: its style and the pane it is scoped to (`None`: the whole page).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HudPlacement {
    pub style: PanelStyle,
    pub scope: Option<Slot>,
}

// MARK: - the layer

/// The panel's framed layer on a page and the terminal it holds.
pub struct HudLayer {
    frame: gtk::Box,
    /// The slot generation the mounted terminal belongs to, and the terminal.
    panel: Option<(u64, TerminalSurface)>,
}

impl HudLayer {
    /// An empty, invisible layer; the page adds it to its stack once.
    #[must_use]
    pub fn new() -> Self {
        install_css();
        let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        frame.add_css_class(LAYER_CLASS);
        frame.set_overflow(gtk::Overflow::Hidden);
        frame.set_can_target(false);
        frame.set_can_focus(false);
        frame.set_focusable(false);
        frame.set_opacity(0.0);
        Self { frame, panel: None }
    }

    #[must_use]
    pub fn widget(&self) -> &gtk::Box {
        &self.frame
    }

    /// The mounted panel's terminal.
    #[must_use]
    pub fn terminal(&self) -> Option<&TerminalSurface> {
        self.panel.as_ref().map(|(_, terminal)| terminal)
    }

    /// Drops a terminal whose generation is not `live` (closed or replaced; the model tore it down already).
    fn retain(&mut self, live: Option<u64>) {
        if self
            .panel
            .as_ref()
            .is_some_and(|(generation, _)| Some(*generation) != live)
            && let Some((_, terminal)) = self.panel.take()
        {
            terminal.teardown();
            self.frame.remove(terminal.widget());
        }
    }

    fn mount(&mut self, generation: u64, terminal: TerminalSurface) {
        self.frame.append(terminal.widget());
        self.panel = Some((generation, terminal));
    }

    /// Shows or hides the panel; it never takes focus or clicks either way.
    fn set_visible(&self, visible: bool, frame: bool) {
        self.frame.set_opacity(if visible { 1.0 } else { 0.0 });
        if frame {
            self.frame.remove_css_class(FRAMELESS_CLASS);
        } else {
            self.frame.add_css_class(FRAMELESS_CLASS);
        }
        if let Some((_, terminal)) = &self.panel {
            if terminal.is_deck_visible() != visible {
                terminal.set_deck_visible(visible);
            }
            let widget = terminal.widget();
            widget.set_focusable(false);
            widget.set_can_target(false);
        }
    }
}

impl Default for HudLayer {
    fn default() -> Self {
        Self::new()
    }
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

/// What the panel's terminal is spawned with.
pub struct HudContext<'a> {
    pub window: Uuid,
    pub workspace: Uuid,
    pub context: &'a PaneContext,
    pub poster: &'a Poster,
}

/// Brings the page's panel in line with `session`'s slot: a closed or replaced HUD's terminal goes, a HUD without one
/// gets it (installed in the slot), and the panel shows while the page does and its pane, when scoped, is laid out.
/// `plan` is the page's layout, `None` under the dashboard, which shows no panel.
pub fn sync_page(
    panes: &mut PaneView,
    session: &mut Session,
    hud: &HudContext<'_>,
    plan: Option<&PanePlan>,
    shown: bool,
) {
    let slot = &session.overlay;
    let live = slot.hud_active().then_some(slot.generation);
    panes.hud_mut().retain(live);
    let Some(generation) = live else {
        panes.place_hud(None);
        panes.hud_mut().set_visible(false, true);
        return;
    };
    if panes.hud_mut().panel.is_none() && !session.overlay.surface.is_present() {
        let terminal = spawn(session, hud);
        panes.hud_mut().mount(generation, terminal.clone());
        session
            .overlay
            .surface
            .replace(Box::new(PaneSurface::new(terminal, String::new())));
    }
    let Some(style) = PanelStyle::of(&session.overlay) else {
        return;
    };
    let scope = session.hud_target_pane().map(Slot::of_overlay);
    // a scoped panel mounts only while its pane is laid out (upstream's `hudCanMount`)
    let laid_out = plan.is_some_and(|plan| scope.is_none_or(|slot| plan.shows(slot)));
    panes.place_hud(Some(HudPlacement { style, scope }));
    panes.hud_mut().set_visible(shown && laid_out, style.frame);
}

/// The panel's terminal: the painter in the session's directory, with the session's environment plus the body file,
/// at the HUD's font size, padded as the panel was measured, on the HUD's own background.
fn spawn(session: &Session, hud: &HudContext<'_>) -> TerminalSurface {
    let slot = &session.overlay;
    let state = slot.hud.as_ref();
    let mut env: Vec<(String, String)> = pane_env::session(&SessionPane {
        session: session.id(),
        window: Some(hud.window),
        workspace: Some(hud.workspace),
        socket_path: &hud.context.socket,
        program_version: &hud.context.program_version,
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
    let terminal = TerminalSurface::new(SurfaceSpec {
        working_directory: Some(PathBuf::from(session.effective_cwd())),
        command: slot.command.clone(),
        env,
        wait_after_command: false,
        font_size,
    });
    terminal.set_config_overlay(&config_overlay(slot.background_color.as_deref()));
    connect_panel(&terminal, session.id(), slot.generation, hud.poster);
    terminal
}

/// The panel's own config: its background, and the padding it was measured with, pinned against the user's
/// `window-padding-*` (upstream's `hudPaddingConfig`).
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
    let mut text = watermark::overlay_text(color.as_ref(), None, None, 1.0);
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

/// The painter exiting (its body file gone) closes the slot it occupies, unless a replacement took it; the panel
/// coming up measures it again with the cell libghostty really draws.
fn connect_panel(terminal: &TerminalSurface, session: Uuid, generation: u64, poster: &Poster) {
    let posting = poster.clone();
    terminal.connect_exit(move || {
        state::post(&posting, move |state: &mut AppState| {
            if state.core.overlay_exited(session, generation) {
                state.settle();
            }
        });
    });
    let posting = poster.clone();
    terminal.connect_created(move || {
        state::post(&posting, move |state: &mut AppState| {
            if state.core.refresh_hud(session) {
                state.settle();
            }
        });
    });
}

// MARK: - link clicks

/// Whether `state` holds the link modifier alone: Ctrl, libghostty's link modifier on Linux (upstream's ⌘).
#[must_use]
pub fn is_link_modifier(state: gdk::ModifierType) -> bool {
    let held = state
        & (gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::SUPER_MASK
            | gdk::ModifierType::META_MASK);
    held == gdk::ModifierType::CONTROL_MASK
}

/// Forwards Ctrl-clicks and Ctrl-hovers over the page's panel to its terminal (upstream's `HudLinkClick`), from the
/// stack's capture phase, so every other click still reaches the session underneath. `layer` reaches the page's
/// panel at event time.
pub fn connect_link_clicks(stack: &gtk::Widget, layer: impl Fn() -> Option<LayerView> + Clone + 'static) {
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_PRIMARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let pressed: std::rc::Rc<std::cell::RefCell<Option<TerminalSurface>>> = std::rc::Rc::default();
    let (find, held) = (layer.clone(), std::rc::Rc::clone(&pressed));
    click.connect_pressed(move |gesture, _, x, y| {
        let state = gesture.current_event_state();
        if !is_link_modifier(state) {
            return;
        }
        let Some((terminal, px, py)) = find().and_then(|view| view.at(x, y)) else {
            return;
        };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        terminal.passive_click(MouseState::Press, px, py, crate::input::translate_mods(state));
        *held.borrow_mut() = Some(terminal);
    });
    let (find, held) = (layer.clone(), pressed);
    click.connect_released(move |gesture, _, x, y| {
        let Some(terminal) = held.borrow_mut().take() else {
            return;
        };
        let mods = crate::input::translate_mods(gesture.current_event_state());
        let (px, py) = find()
            .and_then(|view| view.at(x, y))
            .map_or((-1.0, -1.0), |(_, px, py)| (px, py));
        terminal.passive_click(MouseState::Release, px, py, mods);
    });
    stack.add_controller(click);
    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);
    motion.connect_motion(move |controller, x, y| {
        let state = controller.current_event_state();
        let Some(view) = layer() else { return };
        match view.at(x, y).filter(|_| is_link_modifier(state)) {
            Some((terminal, px, py)) => terminal.passive_pointer(px, py, crate::input::translate_mods(state)),
            None => {
                if let Some(terminal) = view.terminal {
                    terminal.passive_pointer(-1.0, -1.0, Mods::empty());
                }
            }
        }
    });
    stack.add_controller(motion);
}

/// The page's panel as an event handler sees it.
pub struct LayerView {
    stack: glib::WeakRef<gtk::Widget>,
    frame: gtk::Box,
    terminal: Option<TerminalSurface>,
}

impl LayerView {
    /// A view of `layer` on `stack`.
    #[must_use]
    pub fn new(stack: &gtk::Widget, layer: &HudLayer) -> Self {
        Self {
            stack: stack.downgrade(),
            frame: layer.frame.clone(),
            terminal: layer.terminal().cloned(),
        }
    }

    /// The shown panel's terminal and the stack's point (`x`, `y`) in its coordinates, when the point is over it.
    fn at(&self, x: f64, y: f64) -> Option<(TerminalSurface, f64, f64)> {
        if self.frame.opacity() <= 0.0 {
            return None;
        }
        let stack = self.stack.upgrade()?;
        let terminal = self.terminal.clone()?;
        #[expect(clippy::cast_possible_truncation, reason = "pointer coordinates in a widget")]
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        let point = stack.compute_point(terminal.widget(), &point)?;
        let (px, py) = (f64::from(point.x()), f64::from(point.y()));
        let area = terminal.widget();
        let inside = px >= 0.0 && py >= 0.0 && px < f64::from(area.width()) && py < f64::from(area.height());
        inside.then_some((terminal, px, py))
    }
}

#[cfg(test)]
mod tests {
    use agterm_core::hud::HudSpec;
    use agterm_core::model::HudState;

    use super::*;

    fn style(position: HudPosition, sticky: bool) -> PanelStyle {
        PanelStyle {
            width_fraction: 0.5,
            height_fraction: 0.1,
            height_points: Some(60.0),
            edge_margin: if sticky { 0.0 } else { 0.1 },
            position,
            frame: true,
        }
    }

    fn pane() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 600,
        }
    }

    #[test]
    fn a_centered_panel_sits_in_the_middle() {
        let frame = style(HudPosition::Center, false).panel_frame(pane());
        assert_eq!(
            frame,
            Rect {
                x: 250,
                y: 270,
                width: 500,
                height: 60
            }
        );
    }

    #[test]
    fn an_anchored_panel_holds_the_edge_margin_unless_sticky() {
        let frame = style(HudPosition::TopLeft, false).panel_frame(pane());
        // across: (1000 * (0.25 - 0.1)) = 150 off center; down: 600 * ((1 - 0.1) / 2 - 0.1) = 210 up
        assert_eq!((frame.x, frame.y), (100, 60));
        let sticky = style(HudPosition::BottomRight, true).panel_frame(pane());
        assert_eq!(
            (sticky.x + sticky.width, sticky.y + sticky.height),
            (1000, 600),
            "flush with the corner"
        );
    }

    #[test]
    fn a_scoped_panel_is_placed_in_its_pane() {
        let right = Rect {
            x: 501,
            y: 0,
            width: 499,
            height: 600,
        };
        let frame = style(HudPosition::Center, false).panel_frame(right);
        assert!(frame.x > right.x && frame.x + frame.width < right.x + right.width);
    }

    #[test]
    fn the_style_follows_the_slot() {
        let mut slot = OverlaySlot {
            active: true,
            size_percent: Some(40),
            ..OverlaySlot::default()
        };
        assert_eq!(PanelStyle::of(&slot), None, "no hud");
        let mut spec = HudSpec::new("m");
        spec.sticky = true;
        spec.frame = false;
        spec.position = HudPosition::BottomCenter;
        slot.hud = Some(HudState {
            spec,
            pane_identity: None,
            file: PathBuf::from("/tmp/x"),
            height_percent: 12,
            height_points: Some(48.0),
            font_size: None,
            resized_width_percent: None,
        });
        let style = PanelStyle::of(&slot).unwrap();
        assert!((style.width_fraction - 0.4).abs() < 1e-9);
        assert_eq!(style.height_points, Some(48.0));
        assert!(style.edge_margin.abs() < f64::EPSILON && !style.frame);
        assert_eq!(style.position, HudPosition::BottomCenter);
    }

    #[test]
    fn the_config_pins_the_padding_and_paints_the_background() {
        let plain = config_overlay(None);
        assert!(plain.contains("window-padding-x = 8\n") && plain.contains("window-padding-y = 6\n"));
        assert!(!plain.contains("background ="));
        let colored = config_overlay(Some("#102030"));
        assert!(colored.contains("background = #102030\n"));
        assert!(colored.contains("window-padding-balance = true"));
    }

    #[test]
    fn only_ctrl_alone_is_the_link_modifier() {
        assert!(is_link_modifier(gdk::ModifierType::CONTROL_MASK));
        assert!(is_link_modifier(
            gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::BUTTON1_MASK
        ));
        assert!(!is_link_modifier(
            gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK
        ));
        assert!(!is_link_modifier(gdk::ModifierType::empty()));
    }

    #[test]
    fn the_helper_is_written_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = install_helper(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), HELPER);
        assert_eq!(install_helper(dir.path()), Some(path));
    }
}
