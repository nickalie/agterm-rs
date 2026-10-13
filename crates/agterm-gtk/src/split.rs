//! One session's panes on its deck page: the primary terminal, the split terminal and the scratch terminal, with the
//! divider between the split's panes (upstream's `sessionDetail`/`shownSplit` in `WindowContentView+Detail.swift` and
//! `SplitRatioAccessor.swift`).
//!
//! Every terminal is a permanent child of one [`PaneStack`] for its whole life: a live `GtkGLArea` must never be
//! re-parented (see `surface.rs`), so showing or hiding the split, transposing it, moving the divider, swapping the
//! panes, zooming one and covering them with the scratch only change allocations and visibility. A pane the layout
//! does not show stays mapped and allocated (at the full page size, under the shown one) at opacity 0, not targetable,
//! not focusable and occluded, as the deck's hidden pages are.
//!
//! What goes where is a pure function of the session and the window's zoom ([`PanePlan::layout`], [`needs`]), and the
//! rectangles a pure function of the plan and the page size ([`geometry`]).
//!
//! While the window's dashboard is open the page lays out its member panes at their grid cells instead
//! ([`PaneView::apply_grid`], cells from [`crate::dashboard::cell_rects`]), shown but view-only, and hides the rest.
//!
//! The HUD's layer sits above the panes, added once ([`crate::hud`]); the stack places it in the bounds it is scoped
//! to and reports the page's geometry to the model, which measures a HUD against it (upstream's `hudPaneFrames`).
//! A terminal ask's layer sits above the HUD, added once too ([`crate::ask_dialog`]), placed over the whole page or
//! the pane the ask covers.
//!
//! An HTML page in the overlay slot is laid out as a program is ([`crate::overlay`]); a pane page's panel sits where a
//! pane overlay's terminal would, and its web view takes the keyboard in place of a terminal ([`PaneView::page_focus`]).

use agterm_core::hud::{HudPaneFrame, HudPaneFrames};
use agterm_core::model::pane::{SPLIT_RATIO_DEFAULT, clamp_split_ratio};
use agterm_core::model::{OverlayPane, Session, SplitAxis, StatusPane};
use agterm_core::zoom::{SurfaceId, ZoomSurface};
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use uuid::Uuid;

use crate::ask_dialog::SessionAskLayer;
use crate::hud::{HudLayer, HudPlacement, LayerView};
use crate::overlay::{PaneOverlayTerminal, PanePage, ProgramLayer, ProgramPlacement};
use crate::state::{self, AppState, Poster};
use crate::surface::TerminalSurface;

/// The separator line between the split's panes, in pixels.
const SEPARATOR: i32 = 1;
/// The divider's grab band, centered on the separator and reaching over both panes' edges.
const GRAB_BAND: i32 = 7;

/// A position a session's terminal is laid out at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    /// The primary pane: left, or top.
    Primary,
    /// The split pane: right, or bottom.
    Split,
    /// The scratch terminal, over the panes.
    Scratch,
}

impl Slot {
    fn of(pane: StatusPane) -> Self {
        match pane {
            StatusPane::Left => Self::Primary,
            StatusPane::Right => Self::Split,
            StatusPane::Scratch => Self::Scratch,
        }
    }

    /// The slot an overlay's pane role (a HUD's or a terminal ask's) is laid out at.
    #[must_use]
    pub fn of_overlay(pane: OverlayPane) -> Self {
        match pane {
            OverlayPane::Left => Self::Primary,
            OverlayPane::Right => Self::Split,
        }
    }
}

/// What a terminal ask covers on its page (upstream's `SessionAskOverlay.coveredFrame`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskScope {
    /// The whole page.
    Session,
    /// The pane laid out at this slot.
    Pane(Slot),
}

/// Which of the split's panes the page lays out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panes {
    /// Both, beside each other with the divider between: the split is shown.
    Both,
    /// One pane, maximized: the split is hidden or absent, or the window is zoomed into it.
    Alone(Slot),
}

/// The program overlay a page shows: its slot generation and its share of the page (`None`: it covers the page).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgramShow {
    pub generation: u64,
    pub fraction: Option<f64>,
}

/// An overlay that holds the keyboard in place of a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayFocus {
    /// The session-wide program.
    Program,
    /// The overlay over the pane at this slot.
    Pane(Slot),
}

/// The overlays a page shows (upstream's `overlayPanel` and `paneOverlayPanel` gates): the program, and each laid-out
/// pane's own overlay by generation.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OverlayPlan {
    pub program: Option<ProgramShow>,
    /// The program covers the whole page: the panes, the divider and the scratch hide under it.
    pub covers: bool,
    pub primary: Option<u64>,
    pub split: Option<u64>,
    pub focus: Option<OverlayFocus>,
}

impl OverlayPlan {
    /// The generation of the pane overlay shown over the pane at `slot`.
    #[must_use]
    pub fn pane(&self, slot: Slot) -> Option<u64> {
        match slot {
            Slot::Primary => self.primary,
            Slot::Split => self.split,
            Slot::Scratch => None,
        }
    }

    /// The overlays of `session` under the window's zoom (`zoomed`, one of the session's own surfaces): a zoomed
    /// program covers the page, a zoomed pane overlay shows alone over its pane, a zoomed terminal shows bare; else
    /// the program shows at its size, and each pane laid out, uncovered, shows its own overlay. A covering program
    /// holds the keyboard, else the focused pane's overlay does unless the scratch covers it. A page in the slot is laid
    /// out as a program is (upstream's `coverOverlayActive`).
    fn of(session: &Session, zoomed: Option<ZoomSurface>, panes: Panes, scratch_covers: bool) -> Self {
        let slot = &session.overlay;
        let program = session.cover_overlay_active().then_some(slot.generation);
        let generation = |pane: OverlayPane| session.pane_overlay(pane).map(|overlay| overlay.generation);
        match zoomed {
            Some(ZoomSurface::Overlay) if program.is_some() => Self {
                program: program.map(|generation| ProgramShow {
                    generation,
                    fraction: None,
                }),
                covers: true,
                focus: Some(OverlayFocus::Program),
                ..Self::default()
            },
            Some(kind @ (ZoomSurface::OverlayLeft | ZoomSurface::OverlayRight)) => {
                let Some(pane) = kind.overlay_pane() else {
                    return Self::default();
                };
                let mut plan = Self {
                    focus: Some(OverlayFocus::Pane(Slot::of_overlay(pane))),
                    ..Self::default()
                };
                plan.set_pane(Slot::of_overlay(pane), generation(pane));
                plan
            }
            Some(_) => Self::default(),
            None => {
                let fraction = slot.size_percent.map(percent_fraction);
                let mut plan = Self {
                    program: program.map(|generation| ProgramShow { generation, fraction }),
                    covers: program.is_some() && fraction.is_none(),
                    ..Self::default()
                };
                if !plan.covers && !scratch_covers {
                    let laid_out = [OverlayPane::Left, OverlayPane::Right]
                        .into_iter()
                        .filter(|pane| lays_out(panes, Slot::of_overlay(*pane)));
                    for pane in laid_out {
                        plan.set_pane(Slot::of_overlay(pane), generation(pane));
                    }
                }
                let focused = Slot::of_overlay(session.focused_overlay_pane_role());
                plan.focus = if plan.program.is_some() {
                    Some(OverlayFocus::Program)
                } else if !scratch_covers && plan.pane(focused).is_some() {
                    Some(OverlayFocus::Pane(focused))
                } else {
                    None
                };
                plan
            }
        }
    }

    fn set_pane(&mut self, slot: Slot, generation: Option<u64>) {
        match slot {
            Slot::Primary => self.primary = generation,
            Slot::Split => self.split = generation,
            Slot::Scratch => {}
        }
    }
}

/// A percent within 1...100 as a fraction.
fn percent_fraction(percent: i64) -> f64 {
    // within 1...100 by the slot's clamp, so the conversion is exact
    #[expect(clippy::cast_precision_loss, reason = "a percent within 0...100")]
    let percent = percent.clamp(1, 100) as f64;
    percent / 100.0
}

/// How a session's page lays its terminals out, by pane token.
#[derive(Debug, Clone, PartialEq)]
pub struct PanePlan {
    /// The token of the terminal in the primary slot, laid out left or top.
    pub primary: Option<String>,
    /// The token of the terminal in the split slot, laid out right or bottom.
    pub split: Option<String>,
    pub scratch: Option<String>,
    pub axis: SplitAxis,
    /// The primary pane's fraction of the split.
    pub ratio: f64,
    pub panes: Panes,
    /// The scratch fills the page over the panes, which it hides.
    pub scratch_covers: bool,
    /// The slot that should hold keyboard focus while the page shows, unless an overlay holds it.
    pub focus: Slot,
    pub overlays: OverlayPlan,
}

impl PanePlan {
    /// The layout of `session` under the window's `zoom` (upstream's `sessionDetail`): a shown split lays both panes
    /// out, a hidden one only the focused pane, maximized; a shown scratch covers them and takes the keyboard, else
    /// the focused pane does. Zoomed into one of the session's terminals, the page shows that terminal alone.
    #[must_use]
    pub fn layout(session: &Session, zoom: Option<SurfaceId>) -> Self {
        let zoomed = zoom
            .filter(|zoom| zoom.session == session.id() && zoom.surface.is_available(session))
            .map(|zoom| zoom.surface);
        let zoomed_overlay_pane = zoomed.and_then(ZoomSurface::overlay_pane).map(Slot::of_overlay);
        let (panes, scratch_covers, focus) = match (zoomed_pane(session, zoom), zoomed_overlay_pane) {
            (_, Some(slot)) => (Panes::Alone(slot), false, slot),
            (Some(StatusPane::Scratch), None) => (unzoomed_panes(session), true, Slot::Scratch),
            (Some(pane), None) => (Panes::Alone(Slot::of(pane)), false, Slot::of(pane)),
            (None, None) if session.scratch_active => (unzoomed_panes(session), true, Slot::Scratch),
            (None, None) if session.focuses_split() => (unzoomed_panes(session), false, Slot::Split),
            (None, None) => (unzoomed_panes(session), false, Slot::Primary),
        };
        let overlays = OverlayPlan::of(session, zoomed, panes, scratch_covers);
        let token = |pane: StatusPane| {
            session
                .pane_slot(pane)
                .get()
                .map(|surface| surface.pane_token().to_owned())
        };
        Self {
            primary: token(StatusPane::Left),
            split: token(StatusPane::Right),
            scratch: token(StatusPane::Scratch),
            axis: session.split.axis,
            ratio: session.split.ratio.unwrap_or(SPLIT_RATIO_DEFAULT),
            panes,
            scratch_covers,
            focus,
            overlays,
        }
    }

    /// Whether the page shows the terminal at `slot`.
    #[must_use]
    pub fn shows(&self, slot: Slot) -> bool {
        if self.overlays.covers {
            return false;
        }
        match (slot, self.panes) {
            (Slot::Scratch, _) => self.scratch_covers,
            (_, Panes::Both) => !self.scratch_covers,
            (slot, Panes::Alone(alone)) => !self.scratch_covers && slot == alone,
        }
    }

    /// The token of the terminal at `slot`.
    #[must_use]
    pub fn token(&self, slot: Slot) -> Option<&str> {
        match slot {
            Slot::Primary => self.primary.as_deref(),
            Slot::Split => self.split.as_deref(),
            Slot::Scratch => self.scratch.as_deref(),
        }
        .filter(|token| !token.is_empty())
    }

    /// Where the terminal carrying `token` is laid out.
    #[must_use]
    pub fn slot_of(&self, token: &str) -> Option<Slot> {
        [Slot::Primary, Slot::Split, Slot::Scratch]
            .into_iter()
            .find(|slot| self.token(*slot) == Some(token))
    }

    /// The token of the terminal that should hold keyboard focus.
    #[must_use]
    pub fn focus_token(&self) -> Option<&str> {
        self.token(self.focus)
    }

    /// Whether the divider shows: both panes laid out and uncovered.
    #[must_use]
    pub fn shows_divider(&self) -> bool {
        self.panes == Panes::Both && !self.scratch_covers && !self.overlays.covers
    }
}

/// The pane `zoom` fills the window with, when it is one of `session`'s terminals.
fn zoomed_pane(session: &Session, zoom: Option<SurfaceId>) -> Option<StatusPane> {
    zoom.filter(|zoom| zoom.session == session.id())
        .and_then(|zoom| zoom.surface.pane())
}

/// The split's own arrangement: both panes while shown, else the focused one (upstream's `renders_pane`).
fn unzoomed_panes(session: &Session) -> Panes {
    if session.split.shown {
        Panes::Both
    } else if session.focuses_split() {
        Panes::Alone(Slot::Split)
    } else {
        Panes::Alone(Slot::Primary)
    }
}

/// The terminals a session's page has yet to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Needs {
    /// The split pane: it exists, is laid out (or zoomed into) and has no terminal yet. A hidden split's unfocused
    /// pane is created only once something lays it out, as upstream's deck mounts only the panes it renders.
    pub split: bool,
    /// The scratch: shown and without a terminal (a closed scratch's next show starts fresh).
    pub scratch: bool,
}

/// What [`Needs`] `session`'s page has under the window's `zoom`.
#[must_use]
pub fn needs(session: &Session, zoom: Option<SurfaceId>) -> Needs {
    let zoomed_split = zoomed_pane(session, zoom) == Some(StatusPane::Right);
    Needs {
        split: session.split.exists
            && session.split_pane_identity.is_some()
            && !session.split_surface.is_present()
            && (session.renders_pane(true) || zoomed_split),
        scratch: session.scratch_active && !session.scratch_surface.is_present(),
    }
}

/// A child's rectangle in the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub(crate) fn full(width: i32, height: i32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// The rectangle with its axes exchanged: the left/right arithmetic serves top/bottom.
    fn transposed(self) -> Self {
        Self {
            x: self.y,
            y: self.x,
            width: self.height,
            height: self.width,
        }
    }
}

/// Where the panes and the divider's grab band go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub primary: Rect,
    pub split: Rect,
    /// The grab band, while both panes are laid out.
    pub divider: Option<Rect>,
}

/// The rectangles of a `width`×`height` page: a pane alone fills it (and so does the one hidden under it); both
/// panes split the axis at `ratio` around the separator.
#[must_use]
pub fn geometry(panes: Panes, axis: SplitAxis, ratio: f64, width: i32, height: i32) -> Geometry {
    let full = Rect::full(width, height);
    if panes != Panes::Both {
        return Geometry {
            primary: full,
            split: full,
            divider: None,
        };
    }
    let (length, cross) = match axis {
        SplitAxis::LeftRight => (width, height),
        SplitAxis::TopBottom => (height, width),
    };
    let content = (length - SEPARATOR).max(0);
    let primary_length = primary_extent(content, ratio);
    let primary = Rect {
        width: primary_length,
        ..Rect::full(length, cross)
    };
    let split = Rect {
        x: primary_length + SEPARATOR,
        y: 0,
        width: content - primary_length,
        height: cross,
    };
    let divider = Rect {
        x: primary_length + SEPARATOR / 2 - GRAB_BAND / 2,
        y: 0,
        width: GRAB_BAND,
        height: cross,
    };
    let orient = |rect: Rect| match axis {
        SplitAxis::LeftRight => rect,
        SplitAxis::TopBottom => rect.transposed(),
    };
    Geometry {
        primary: orient(primary),
        split: orient(split),
        divider: Some(orient(divider)),
    }
}

/// The primary pane's share of `content` pixels at `ratio`.
fn primary_extent(content: i32, ratio: f64) -> i32 {
    let extent = (f64::from(content) * ratio.clamp(0.0, 1.0)).round();
    // within 0..=content by the clamp, so the conversion is exact
    #[expect(clippy::cast_possible_truncation, reason = "rounded and bounded by an i32")]
    let extent = extent as i32;
    extent.clamp(0, content)
}

/// The divider's ratio after a drag of `offset` pixels from `start` along a split of `length` pixels (separator
/// included), clamped into the stored bounds; `start` unchanged without a real extent.
#[must_use]
pub fn dragged_ratio(start: f64, offset: f64, length: i32) -> f64 {
    let content = f64::from(length - SEPARATOR);
    if content <= 1.0 {
        return start;
    }
    clamp_split_ratio(start + offset / content)
}

// MARK: - the container widget

/// What [`PaneStack`] allocates: the plan's layout and which child sits where.
#[derive(Default)]
struct StackLayout {
    panes: Option<Panes>,
    axis: SplitAxis,
    ratio: f64,
    primary: Option<gtk::Widget>,
    split: Option<gtk::Widget>,
    divider: Option<gtk::Widget>,
    drag: DividerDrag,
    /// The dashboard's layout while it is open: the cell index of each member terminal among `count` cells.
    grid: Option<GridCells>,
    /// The HUD's layer, and where its panel goes while one is up.
    hud: Option<gtk::Widget>,
    hud_placement: Option<HudPlacement>,
    /// The terminal ask's layer, and what it covers while one shows.
    ask: Option<gtk::Widget>,
    ask_scope: Option<AskScope>,
    /// The program overlay's backdrop and panel, and where the panel goes while one shows.
    program_backdrop: Option<gtk::Widget>,
    program_frame: Option<gtk::Widget>,
    program_placement: Option<ProgramPlacement>,
    /// The pane overlays' terminals and the pane each covers.
    pane_overlays: Vec<(gtk::Widget, Slot)>,
    /// Where the page's geometry goes: its session and the main loop.
    reports: Option<(Uuid, Poster)>,
    /// The geometry last reported.
    reported: Option<HudPaneFrames>,
}

/// A page's terminals placed on the dashboard grid.
#[derive(Debug, Default)]
struct GridCells {
    count: usize,
    cells: Vec<(gtk::Widget, usize)>,
}

/// The divider's drag: the ratio it started from while one runs, and the ratio it shows until the model has it.
#[derive(Debug, Default, Clone, Copy)]
struct DividerDrag {
    start: Option<f64>,
    ratio: Option<f64>,
    /// The divider moved since the last press, so a second press is a re-grab, not a double-click.
    moved: bool,
}

impl StackLayout {
    fn ratio(&self) -> f64 {
        self.drag.ratio.unwrap_or(self.ratio)
    }

    fn allocate(&mut self, stack: &gtk::Widget, width: i32, height: i32) {
        if let Some(grid) = &self.grid {
            allocate_grid(stack, grid, width, height);
            return;
        }
        let panes = self.panes.unwrap_or(Panes::Alone(Slot::Primary));
        let geometry = geometry(panes, self.axis, self.ratio(), width, height);
        let hud = self.hud_rect(panes, &geometry, width, height);
        let mut child = stack.first_child();
        while let Some(widget) = child {
            let rect = if Some(&widget) == self.primary.as_ref() {
                geometry.primary
            } else if Some(&widget) == self.split.as_ref() {
                geometry.split
            } else if Some(&widget) == self.divider.as_ref() {
                geometry.divider.unwrap_or_default()
            } else if Some(&widget) == self.hud.as_ref() {
                hud
            } else if Some(&widget) == self.ask.as_ref() {
                ask_rect(self.ask_scope, panes, &geometry, width, height)
            } else if Some(&widget) == self.program_frame.as_ref() {
                program_rect(self.program_placement, width, height)
            } else if let Some((_, slot)) = self.pane_overlays.iter().find(|(overlay, _)| *overlay == widget) {
                pane_rect(panes, &geometry, *slot).unwrap_or(Rect::full(width, height))
            } else {
                Rect::full(width, height)
            };
            widget.size_allocate(&gtk::Allocation::new(rect.x, rect.y, rect.width, rect.height), -1);
            child = widget.next_sibling();
        }
        self.report(page_frames(panes, &geometry, width, height));
    }

    /// The HUD panel's rectangle: in its pane while it is scoped to one laid out, else in the whole page.
    fn hud_rect(&self, panes: Panes, geometry: &Geometry, width: i32, height: i32) -> Rect {
        let Some(placement) = self.hud_placement else {
            return Rect::default();
        };
        let bounds = match placement.scope {
            Some(Slot::Primary) if lays_out(panes, Slot::Primary) => geometry.primary,
            Some(Slot::Split) if lays_out(panes, Slot::Split) => geometry.split,
            _ => Rect::full(width, height),
        };
        placement.style.panel_frame(bounds)
    }

    /// Posts the page's geometry to the model when it changed.
    fn report(&mut self, frames: HudPaneFrames) {
        if self.reported.as_ref() == Some(&frames) {
            return;
        }
        let Some((session, poster)) = self.reports.clone() else {
            return;
        };
        self.reported = Some(frames);
        state::post(&poster, move |state: &mut AppState| {
            state.report_hud_frames(session, &frames);
        });
    }
}

/// The program panel's rectangle: centered at its share of the page across and down while it floats, else the page
/// (upstream's program `OverlayPanelStyle`, always centered).
fn program_rect(placement: Option<ProgramPlacement>, width: i32, height: i32) -> Rect {
    let Some(fraction) = placement.and_then(|placement| placement.fraction) else {
        return Rect::full(width, height);
    };
    let scaled = |extent: i32| {
        let value = (f64::from(extent) * fraction.clamp(0.0, 1.0)).round();
        // within 0..=extent by the clamp, so the conversion is exact
        #[expect(clippy::cast_possible_truncation, reason = "rounded and bounded by an i32")]
        let value = value as i32;
        value
    };
    let (panel_width, panel_height) = (scaled(width), scaled(height));
    Rect {
        x: (width - panel_width) / 2,
        y: (height - panel_height) / 2,
        width: panel_width,
        height: panel_height,
    }
}

/// The terminal ask layer's rectangle: the page, or the pane it covers while that pane is laid out.
fn ask_rect(scope: Option<AskScope>, panes: Panes, geometry: &Geometry, width: i32, height: i32) -> Rect {
    match scope {
        Some(AskScope::Session) => Rect::full(width, height),
        Some(AskScope::Pane(slot)) => pane_rect(panes, geometry, slot).unwrap_or_default(),
        None => Rect::default(),
    }
}

/// The rectangle of the pane at `slot` (primary or split) while `panes` lays it out.
fn pane_rect(panes: Panes, geometry: &Geometry, slot: Slot) -> Option<Rect> {
    match slot {
        Slot::Primary if lays_out(panes, slot) => Some(geometry.primary),
        Slot::Split if lays_out(panes, slot) => Some(geometry.split),
        _ => None,
    }
}

/// Whether `panes` lays the pane at `slot` out.
fn lays_out(panes: Panes, slot: Slot) -> bool {
    panes == Panes::Both || panes == Panes::Alone(slot)
}

/// A rectangle as the model's frame.
fn frame_of(rect: Rect) -> HudPaneFrame {
    HudPaneFrame {
        x: f64::from(rect.x),
        y: f64::from(rect.y),
        width: f64::from(rect.width),
        height: f64::from(rect.height),
    }
}

/// The page's detail area and the frames of the panes it lays out, by role (upstream's pane anchors).
fn page_frames(panes: Panes, geometry: &Geometry, width: i32, height: i32) -> HudPaneFrames {
    HudPaneFrames {
        left: lays_out(panes, Slot::Primary).then(|| frame_of(geometry.primary)),
        right: lays_out(panes, Slot::Split).then(|| frame_of(geometry.split)),
        detail: (width > 0 && height > 0).then(|| frame_of(Rect::full(width, height))),
    }
}

/// Allocates the grid's member terminals at their cells and every other child at the full page, hidden under them.
fn allocate_grid(stack: &gtk::Widget, grid: &GridCells, width: i32, height: i32) {
    let rects = crate::dashboard::cell_rects(grid.count, width, height);
    let mut child = stack.first_child();
    while let Some(widget) = child {
        let rect = grid
            .cells
            .iter()
            .find(|(cell, _)| *cell == widget)
            .and_then(|(_, index)| rects.get(*index).copied())
            .unwrap_or(Rect::full(width, height));
        widget.size_allocate(&gtk::Allocation::new(rect.x, rect.y, rect.width, rect.height), -1);
        child = widget.next_sibling();
    }
}

mod imp {
    use std::cell::RefCell;

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    use super::StackLayout;

    #[derive(Default)]
    pub struct PaneStack {
        pub(super) layout: RefCell<StackLayout>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PaneStack {
        const NAME: &'static str = "AgtermPaneStack";
        type Type = super::PaneStack;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for PaneStack {
        fn dispose(&self) {
            let widget = self.obj();
            while let Some(child) = widget.first_child() {
                child.unparent();
            }
            let mut layout = self.layout.borrow_mut();
            layout.primary = None;
            layout.split = None;
            layout.divider = None;
            layout.grid = None;
            layout.hud = None;
            layout.ask = None;
            layout.program_backdrop = None;
            layout.program_frame = None;
            layout.pane_overlays.clear();
        }
    }

    impl WidgetImpl for PaneStack {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            self.layout
                .borrow_mut()
                .allocate(self.obj().upcast_ref(), width, height);
        }
    }
}

glib::wrapper! {
    /// The container of a session's terminals and divider, laying them out by its [`StackLayout`].
    pub struct PaneStack(ObjectSubclass<imp::PaneStack>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PaneStack {
    fn new() -> Self {
        glib::Object::builder()
            .property("hexpand", true)
            .property("vexpand", true)
            .build()
    }

    fn layout(&self) -> std::cell::RefMut<'_, StackLayout> {
        self.imp().layout.borrow_mut()
    }

    /// Where the pane at `slot` is laid out in the page now; `None` while it is not (or under the dashboard).
    #[must_use]
    pub fn pane_rect(&self, slot: Slot) -> Option<Rect> {
        let layout = self.imp().layout.try_borrow().ok()?;
        if layout.grid.is_some() {
            return None;
        }
        let panes = layout.panes?;
        let geometry = geometry(panes, layout.axis, layout.ratio(), self.width(), self.height());
        pane_rect(panes, &geometry, slot)
    }
}

// MARK: - the session's panes

/// One terminal of the page and the token its shell carries.
struct Pane {
    token: String,
    terminal: TerminalSurface,
}

/// A session's page content: its terminals in their [`PaneStack`], and the divider.
pub struct PaneView {
    stack: PaneStack,
    divider: gtk::Box,
    separator: gtk::Separator,
    panes: Vec<Pane>,
    /// The HUD's layer, above the panes.
    hud: std::rc::Rc<std::cell::RefCell<HudLayer>>,
    /// The terminal ask's layer, above everything.
    ask: SessionAskLayer,
    /// The program overlay's layer, above the scratch and under the HUD.
    program: ProgramLayer,
    /// The pane overlays' terminals, over the panes and under the scratch.
    pane_overlays: Vec<PaneOverlayTerminal>,
    /// The pane pages' panels, where the pane overlays' terminals go.
    pane_pages: Vec<PanePage>,
    plan: Option<PanePlan>,
    /// The page shows the dashboard's cells ([`PaneView::apply_grid`]).
    in_grid: bool,
}

impl PaneView {
    /// An empty page for `session`; the divider persists what the user drags through `poster`.
    #[must_use]
    pub fn new(session: Uuid, poster: &Poster) -> Self {
        let stack = PaneStack::new();
        // across the default axis, so the first `orient_divider` lays it along
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        let divider = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        divider.append(&separator);
        divider.set_visible(false);
        divider.set_parent(&stack);
        stack.layout().divider = Some(divider.clone().upcast());
        connect_divider(&divider, &stack, session, poster);
        let program = ProgramLayer::new();
        program.backdrop().set_parent(&stack);
        program.frame().set_parent(&stack);
        let hud = HudLayer::new();
        hud.widget().set_parent(&stack);
        let ask = SessionAskLayer::new();
        ask.widget().set_parent(&stack);
        {
            let mut layout = stack.layout();
            layout.hud = Some(hud.widget().clone().upcast());
            layout.ask = Some(ask.widget().clone().upcast());
            layout.program_backdrop = Some(program.backdrop().clone().upcast());
            layout.program_frame = Some(program.frame().clone().upcast());
            layout.reports = Some((session, poster.clone()));
        }
        let hud = std::rc::Rc::new(std::cell::RefCell::new(hud));
        let (weak, layer) = (stack.downgrade(), std::rc::Rc::downgrade(&hud));
        crate::hud::connect_link_clicks(stack.upcast_ref(), move || {
            let stack = weak.upgrade()?;
            let layer = layer.upgrade()?;
            let layer = layer.try_borrow().ok()?;
            Some(LayerView::new(stack.upcast_ref(), &layer))
        });
        let view = Self {
            stack,
            divider,
            separator,
            panes: Vec::new(),
            hud,
            ask,
            program,
            pane_overlays: Vec::new(),
            pane_pages: Vec::new(),
            plan: None,
            in_grid: false,
        };
        view.orient_divider(SplitAxis::default());
        view
    }

    /// The page widget.
    #[must_use]
    pub fn widget(&self) -> &PaneStack {
        &self.stack
    }

    /// Mounts `terminal`, carrying `token`, hidden until the next [`PaneView::apply`] shows it: the panes under the
    /// divider, the scratch over everything.
    pub fn add(&mut self, token: String, terminal: TerminalSurface, scratch: bool) {
        let widget = terminal.widget();
        if scratch {
            widget.insert_before(&self.stack, Some(self.program.backdrop()));
        } else {
            widget.insert_before(&self.stack, Some(&self.divider));
        }
        set_pane_visible(&terminal, false);
        self.panes.push(Pane { token, terminal });
    }

    /// The terminal carrying `token`.
    #[must_use]
    pub fn terminal(&self, token: &str) -> Option<&TerminalSurface> {
        self.panes
            .iter()
            .find(|pane| pane.token == token)
            .map(|pane| &pane.terminal)
    }

    /// Drops the terminals whose token `live` refuses: the model tore them down already (a closed split or scratch,
    /// a primary a promotion replaced), so this only takes their widgets out.
    pub fn retain(&mut self, live: impl Fn(&str) -> bool) {
        let (kept, gone): (Vec<Pane>, Vec<Pane>) = std::mem::take(&mut self.panes)
            .into_iter()
            .partition(|pane| live(&pane.token));
        self.panes = kept;
        for pane in gone {
            pane.terminal.teardown();
            pane.terminal.widget().unparent();
        }
    }

    /// Tears every terminal down: the session is gone.
    pub fn teardown(&self) {
        for pane in &self.panes {
            pane.terminal.teardown();
        }
        for overlay in &self.pane_overlays {
            overlay.terminal.teardown();
        }
        if let Some(terminal) = self.program.terminal() {
            terminal.teardown();
        }
        if let Some(terminal) = self.hud.borrow().terminal() {
            terminal.teardown();
        }
    }

    /// The HUD's layer.
    pub fn hud_mut(&self) -> std::cell::RefMut<'_, HudLayer> {
        self.hud.borrow_mut()
    }

    /// The program overlay's layer.
    pub fn program_mut(&mut self) -> &mut ProgramLayer {
        &mut self.program
    }

    /// The program overlay's layer.
    #[must_use]
    pub fn program(&self) -> &ProgramLayer {
        &self.program
    }

    /// Places the program panel (`None`: none shows).
    pub fn place_program(&self, placement: Option<ProgramPlacement>) {
        let mut layout = self.stack.layout();
        if layout.program_placement != placement {
            layout.program_placement = placement;
            drop(layout);
            self.stack.queue_allocate();
        }
    }

    /// The terminal of pane overlay `generation`.
    #[must_use]
    pub fn pane_overlay(&self, generation: u64) -> Option<&PaneOverlayTerminal> {
        self.pane_overlays
            .iter()
            .find(|overlay| overlay.generation == generation)
    }

    /// Mounts a pane overlay's terminal over the panes and under the scratch, hidden until it is placed.
    pub fn add_pane_overlay(&mut self, overlay: PaneOverlayTerminal) {
        overlay.terminal.widget().insert_after(&self.stack, Some(&self.divider));
        set_pane_visible(&overlay.terminal, false);
        self.pane_overlays.push(overlay);
    }

    /// The panel of pane page `generation`.
    #[must_use]
    pub fn pane_page(&self, generation: u64) -> Option<&PanePage> {
        self.pane_pages.iter().find(|page| page.generation == generation)
    }

    /// Mounts a pane page's panel where pane overlay terminals go, hidden until it is placed; it leaves the parent it
    /// had (a session that moved to another window).
    pub fn add_pane_page(&mut self, page: PanePage) {
        crate::overlay::unparent(&page.widget);
        page.widget.insert_after(&self.stack, Some(&self.divider));
        set_widget_visible(&page.widget, false);
        self.pane_pages.push(page);
    }

    /// Whether pane page `generation` shows over its pane.
    #[must_use]
    pub fn pane_page_shown(&self, generation: u64) -> bool {
        self.pane_page(generation)
            .is_some_and(|page| page.widget.can_target() && page.widget.opacity() > 0.0)
    }

    /// Drops the pane overlays whose generation is not in `live` (closed): a terminal the model tore down already, or a
    /// page's panel, which the page's release may have taken out already.
    pub fn retain_pane_overlays(&mut self, live: &[u64]) {
        let (kept, gone): (Vec<PaneOverlayTerminal>, Vec<PaneOverlayTerminal>) =
            std::mem::take(&mut self.pane_overlays)
                .into_iter()
                .partition(|overlay| live.contains(&overlay.generation));
        self.pane_overlays = kept;
        let (kept, gone_pages): (Vec<PanePage>, Vec<PanePage>) = std::mem::take(&mut self.pane_pages)
            .into_iter()
            .partition(|page| live.contains(&page.generation));
        self.pane_pages = kept;
        if gone.is_empty() && gone_pages.is_empty() {
            return;
        }
        let mut layout = self.stack.layout();
        for overlay in gone {
            let widget: gtk::Widget = overlay.terminal.widget().clone().upcast();
            layout.pane_overlays.retain(|(placed, _)| *placed != widget);
            overlay.terminal.teardown();
            widget.unparent();
        }
        for page in gone_pages {
            layout.pane_overlays.retain(|(placed, _)| *placed != page.widget);
            if page.widget.parent().as_ref() == Some(self.stack.upcast_ref()) {
                page.widget.unparent();
            }
        }
    }

    /// The pane overlays' widgets and where `plan` puts each: a terminal or a page over the pane it covers.
    fn placed_pane_overlays(&self, plan: Option<&PanePlan>) -> Vec<(gtk::Widget, Slot)> {
        let slot_of = |generation: u64| {
            [Slot::Primary, Slot::Split]
                .into_iter()
                .find(|slot| plan.is_some_and(|plan| plan.overlays.pane(*slot) == Some(generation)))
        };
        let terminals = self
            .pane_overlays
            .iter()
            .filter_map(|overlay| Some((overlay.terminal.widget().clone().upcast(), slot_of(overlay.generation)?)));
        let pages = self
            .pane_pages
            .iter()
            .filter_map(|page| Some((page.widget.clone(), slot_of(page.generation)?)));
        terminals.chain(pages).collect()
    }

    /// Shows each pane overlay over the pane `plan` puts it on, and hides the others (all of them without a plan).
    pub fn show_pane_overlays(&self, plan: Option<&PanePlan>) {
        let placed = self.placed_pane_overlays(plan);
        for overlay in &self.pane_overlays {
            let widget: gtk::Widget = overlay.terminal.widget().clone().upcast();
            let visible = placed.iter().any(|(shown, _)| *shown == widget);
            if overlay.terminal.is_deck_visible() != visible {
                set_pane_visible(&overlay.terminal, visible);
            }
        }
        for page in &self.pane_pages {
            let visible = placed.iter().any(|(shown, _)| *shown == page.widget);
            set_widget_visible(&page.widget, visible);
        }
        let mut layout = self.stack.layout();
        if layout.pane_overlays != placed {
            layout.pane_overlays = placed;
            drop(layout);
            self.stack.queue_allocate();
        }
    }

    /// The terminal ask's layer.
    pub fn ask_mut(&mut self) -> &mut SessionAskLayer {
        &mut self.ask
    }

    /// The terminal ask's layer.
    #[must_use]
    pub fn ask(&self) -> &SessionAskLayer {
        &self.ask
    }

    /// Places the terminal ask's layer over what it covers (`None`: none shows).
    pub fn place_ask(&self, scope: Option<AskScope>) {
        let mut layout = self.stack.layout();
        if layout.ask_scope != scope {
            layout.ask_scope = scope;
            drop(layout);
            self.stack.queue_allocate();
        }
    }

    /// Places the HUD's panel (`None`: none is up).
    pub fn place_hud(&self, placement: Option<HudPlacement>) {
        let mut layout = self.stack.layout();
        if layout.hud_placement != placement {
            layout.hud_placement = placement;
            drop(layout);
            self.stack.queue_allocate();
        }
    }

    /// Lays the terminals out by `plan`; with the page `shown`, the terminals it shows become visible.
    pub fn apply(&mut self, plan: PanePlan, shown: bool) {
        let leaving_grid = std::mem::take(&mut self.in_grid);
        {
            let mut layout = self.stack.layout();
            layout.grid = None;
            layout.panes = Some(plan.panes);
            layout.axis = plan.axis;
            layout.ratio = plan.ratio;
            layout.primary = self.widget_of(plan.token(Slot::Primary));
            layout.split = self.widget_of(plan.token(Slot::Split));
            if layout.drag.start.is_none() {
                layout.drag.ratio = None;
            }
        }
        self.stack.queue_allocate();
        for pane in &self.panes {
            // a pane renders hidden under its own overlay, which takes its place
            let visible = shown
                && plan
                    .slot_of(&pane.token)
                    .is_some_and(|slot| plan.shows(slot) && plan.overlays.pane(slot).is_none());
            // a grid cell was view-only: the terminal takes input again
            if leaving_grid || pane.terminal.is_deck_visible() != visible {
                set_pane_visible(&pane.terminal, visible);
            }
        }
        self.orient_divider(plan.axis);
        self.divider.set_visible(shown && plan.shows_divider());
        self.plan = Some(plan);
    }

    /// Lays the terminals carrying the tokens of `cells` out at their dashboard cell indexes among `count` cells, shown
    /// but view-only (no input, no focus, upstream's `.allowsHitTesting(false)`), and hides the others and the divider.
    pub fn apply_grid(&mut self, cells: &[(String, usize)], count: usize) {
        self.in_grid = true;
        let placed: Vec<(gtk::Widget, usize)> = cells
            .iter()
            .filter_map(|(token, index)| self.widget_of(Some(token)).map(|widget| (widget, *index)))
            .collect();
        self.stack.layout().grid = Some(GridCells { count, cells: placed });
        self.stack.queue_allocate();
        for pane in &self.panes {
            let member = cells.iter().any(|(token, _)| *token == pane.token);
            if pane.terminal.is_deck_visible() != member {
                set_pane_visible(&pane.terminal, member);
            }
            if member {
                let widget = pane.terminal.widget();
                widget.set_focusable(false);
                widget.set_can_target(false);
            }
        }
        self.divider.set_visible(false);
    }

    /// Hides every terminal and takes a terminal ask down: the session is out of the tree, held by a pending close.
    pub fn hide(&mut self) {
        for pane in &self.panes {
            if pane.terminal.is_deck_visible() {
                set_pane_visible(&pane.terminal, false);
            }
        }
        self.divider.set_visible(false);
        self.ask.clear();
        self.place_ask(None);
        self.place_program(None);
        self.program.hide();
        self.show_pane_overlays(None);
    }

    /// The token and terminal that should hold keyboard focus by the last plan.
    #[must_use]
    pub fn focus(&self) -> Option<(&str, &TerminalSurface)> {
        let plan = self.plan.as_ref()?;
        match plan.overlays.focus {
            Some(OverlayFocus::Program) => self.program.focus(),
            Some(OverlayFocus::Pane(slot)) => self
                .pane_overlay(plan.overlays.pane(slot)?)
                .map(|overlay| (overlay.key.as_str(), &overlay.terminal)),
            None => {
                let token = plan.focus_token()?;
                self.terminal(token).map(|terminal| (token, terminal))
            }
        }
    }

    /// The focus key and web view of the page that should hold keyboard focus by the last plan, in place of a
    /// terminal: the session-wide page, or the focused pane's page (upstream's `focusCover`).
    #[must_use]
    pub fn page_focus(&self) -> Option<(String, gtk::Widget)> {
        let plan = self.plan.as_ref()?;
        match plan.overlays.focus? {
            OverlayFocus::Program => self.program.page_focus(),
            OverlayFocus::Pane(slot) => {
                let page = self.pane_page(plan.overlays.pane(slot)?)?;
                crate::html_overlay::focus_target(page.id).map(|widget| (page.key.clone(), widget))
            }
        }
    }

    fn widget_of(&self, token: Option<&str>) -> Option<gtk::Widget> {
        token
            .and_then(|token| self.terminal(token))
            .map(|terminal| terminal.widget().clone().upcast())
    }

    /// Lays the separator line along the divider of `axis`, centered in the grab band, under its resize cursor.
    fn orient_divider(&self, axis: SplitAxis) {
        use gtk::{Align, Orientation};
        let (line, across, halign, valign, cursor) = match axis {
            SplitAxis::LeftRight => (
                Orientation::Vertical,
                Orientation::Horizontal,
                Align::Center,
                Align::Fill,
                "col-resize",
            ),
            SplitAxis::TopBottom => (
                Orientation::Horizontal,
                Orientation::Vertical,
                Align::Fill,
                Align::Center,
                "row-resize",
            ),
        };
        if self.separator.orientation() == line {
            return;
        }
        self.separator.set_orientation(line);
        self.separator.set_halign(halign);
        self.separator.set_valign(valign);
        self.separator.set_hexpand(true);
        self.separator.set_vexpand(true);
        self.divider.set_orientation(across);
        self.divider.set_cursor_from_name(Some(cursor));
    }
}

/// The visibility switch of a widget that is not a terminal (a page's panel): drawn, hit-tested and focusable only
/// while shown.
fn set_widget_visible(widget: &gtk::Widget, visible: bool) {
    widget.set_opacity(if visible { 1.0 } else { 0.0 });
    widget.set_can_target(visible);
    widget.set_can_focus(visible);
}

/// The one visibility switch of a pane (module docs).
fn set_pane_visible(terminal: &TerminalSurface, visible: bool) {
    terminal.widget().set_opacity(if visible { 1.0 } else { 0.0 });
    terminal.set_deck_visible(visible);
}

// MARK: - the divider

/// A drag moves the divider live and persists the ratio when it ends; a double-click on an unmoved divider restores
/// the even split (upstream's `SplitRatioAccessor`: only a drag captures the divider, the reset persists at once).
fn connect_divider(divider: &gtk::Box, stack: &PaneStack, session: Uuid, poster: &Poster) {
    let drag = gtk::GestureDrag::new();
    let weak = stack.downgrade();
    drag.connect_drag_begin(move |_, _, _| {
        if let Some(stack) = weak.upgrade() {
            drag_begin(&stack);
        }
    });
    let weak = stack.downgrade();
    drag.connect_drag_update(move |_, dx, dy| {
        if let Some(stack) = weak.upgrade() {
            drag_update(&stack, dx, dy);
        }
    });
    let weak = stack.downgrade();
    let posting = poster.clone();
    drag.connect_drag_end(move |_, _, _| {
        if let Some(ratio) = weak.upgrade().and_then(|stack| drag_end(&stack)) {
            post_ratio(&posting, session, ratio);
        }
    });
    divider.add_controller(drag);
    let click = gtk::GestureClick::new();
    let weak = stack.downgrade();
    let posting = poster.clone();
    click.connect_pressed(move |_, presses, _, _| {
        if weak.upgrade().is_some_and(|stack| is_reset(&stack, presses)) {
            post_ratio(&posting, session, SPLIT_RATIO_DEFAULT);
        }
    });
    divider.add_controller(click);
}

fn drag_begin(stack: &PaneStack) {
    let mut layout = stack.layout();
    layout.drag.start = Some(layout.ratio());
}

fn drag_update(stack: &PaneStack, dx: f64, dy: f64) {
    let (offset, length) = match stack.layout().axis {
        SplitAxis::LeftRight => (dx, stack.width()),
        SplitAxis::TopBottom => (dy, stack.height()),
    };
    let mut layout = stack.layout();
    let Some(start) = layout.drag.start else { return };
    let ratio = dragged_ratio(start, offset, length);
    if layout
        .drag
        .ratio
        .is_some_and(|current| (current - ratio).abs() < f64::EPSILON)
    {
        return;
    }
    layout.drag.ratio = Some(ratio);
    layout.drag.moved = true;
    drop(layout);
    stack.queue_allocate();
}

/// Ends the drag; the ratio to persist when it moved the divider.
fn drag_end(stack: &PaneStack) -> Option<f64> {
    let mut layout = stack.layout();
    let start = layout.drag.start.take()?;
    layout.drag.ratio.filter(|ratio| (ratio - start).abs() >= f64::EPSILON)
}

/// Whether a press is the second of a double-click on a divider that has not moved since the first.
fn is_reset(stack: &PaneStack, presses: i32) -> bool {
    let mut layout = stack.layout();
    if presses == 1 {
        layout.drag.moved = false;
        return false;
    }
    presses == 2 && !layout.drag.moved
}

fn post_ratio(poster: &Poster, session: Uuid, ratio: f64) {
    state::post(poster, move |state: &mut AppState| {
        state.set_split_ratio(session, ratio);
    });
}

#[cfg(test)]
mod tests {
    use agterm_core::model::{SessionSeed, SplitLayout};
    use agterm_core::zoom::ZoomSurface;
    use agterm_host::fakes::FakeSurface;

    use super::*;

    /// A session whose primary pane is `P`, with a split pane `S` and a scratch `X` installed as asked.
    fn session(split: Option<SplitLayout>, scratch: bool) -> Session {
        let mut session = Session::new(SessionSeed {
            initial_cwd: "/home/u".to_owned(),
            ..SessionSeed::default()
        });
        session.surface.replace(Box::new(FakeSurface::new("P")));
        if let Some(layout) = split {
            session.split = layout;
            session.split_pane_identity = Some(Uuid::new_v4());
            session.split_surface.replace(Box::new(FakeSurface::new("S")));
        }
        if scratch {
            session.scratch_active = true;
            session.scratch_surface.replace(Box::new(FakeSurface::new("X")));
        }
        session
    }

    fn split(shown: bool, focused: bool) -> SplitLayout {
        SplitLayout {
            shown,
            exists: true,
            focused,
            ..SplitLayout::default()
        }
    }

    fn zoom(session: &Session, surface: ZoomSurface) -> SurfaceId {
        SurfaceId {
            session: session.id(),
            surface,
        }
    }

    #[test]
    fn a_lone_pane_fills_the_page_and_takes_the_keyboard() {
        let plan = PanePlan::layout(&session(None, false), None);
        assert_eq!(plan.panes, Panes::Alone(Slot::Primary));
        assert!(plan.shows(Slot::Primary) && !plan.shows(Slot::Split) && !plan.shows(Slot::Scratch));
        assert_eq!(plan.focus_token(), Some("P"));
        assert!(!plan.shows_divider());
        assert!((plan.ratio - SPLIT_RATIO_DEFAULT).abs() < f64::EPSILON);
    }

    #[test]
    fn a_shown_split_lays_both_panes_out_and_focuses_the_focused_one() {
        let mut shown = session(Some(split(true, true)), false);
        shown.split.axis = SplitAxis::TopBottom;
        shown.split.ratio = Some(0.3);
        let plan = PanePlan::layout(&shown, None);
        assert_eq!(plan.panes, Panes::Both);
        assert!(plan.shows(Slot::Primary) && plan.shows(Slot::Split) && plan.shows_divider());
        assert_eq!(plan.focus_token(), Some("S"));
        assert_eq!(plan.axis, SplitAxis::TopBottom);
        assert!((plan.ratio - 0.3).abs() < f64::EPSILON);
        shown.split.focused = false;
        assert_eq!(PanePlan::layout(&shown, None).focus_token(), Some("P"));
    }

    #[test]
    fn a_hidden_split_shows_only_the_focused_pane_maximized() {
        let plan = PanePlan::layout(&session(Some(split(false, false)), false), None);
        assert_eq!(plan.panes, Panes::Alone(Slot::Primary));
        assert!(plan.shows(Slot::Primary) && !plan.shows(Slot::Split) && !plan.shows_divider());
        assert_eq!(plan.focus_token(), Some("P"));

        let plan = PanePlan::layout(&session(Some(split(false, true)), false), None);
        assert_eq!(plan.panes, Panes::Alone(Slot::Split));
        assert!(plan.shows(Slot::Split) && !plan.shows(Slot::Primary));
        assert_eq!(
            plan.focus_token(),
            Some("S"),
            "the right pane shows and keeps the keyboard"
        );
    }

    #[test]
    fn a_swap_moves_the_tokens_between_positions() {
        let mut swapped = session(Some(split(true, true)), false);
        std::mem::swap(&mut swapped.surface, &mut swapped.split_surface);
        swapped.split.focused = false;
        let plan = PanePlan::layout(&swapped, None);
        assert_eq!(plan.token(Slot::Primary), Some("S"));
        assert_eq!(plan.token(Slot::Split), Some("P"));
        assert_eq!(plan.slot_of("S"), Some(Slot::Primary));
        assert_eq!(plan.focus_token(), Some("S"), "focus followed the terminal");
    }

    #[test]
    fn the_scratch_covers_the_panes_and_takes_the_keyboard() {
        let plan = PanePlan::layout(&session(Some(split(true, true)), true), None);
        assert!(plan.scratch_covers && plan.shows(Slot::Scratch));
        assert!(!plan.shows(Slot::Primary) && !plan.shows(Slot::Split) && !plan.shows_divider());
        assert_eq!(plan.panes, Panes::Both, "the panes keep their layout under it");
        assert_eq!(plan.focus_token(), Some("X"));

        let mut hidden = session(None, true);
        hidden.scratch_active = false;
        let plan = PanePlan::layout(&hidden, None);
        assert!(!plan.shows(Slot::Scratch));
        assert_eq!(plan.focus_token(), Some("P"));
    }

    #[test]
    fn a_zoomed_pane_shows_alone() {
        let full = session(Some(split(true, false)), true);
        let plan = PanePlan::layout(&full, Some(zoom(&full, ZoomSurface::Split)));
        assert_eq!(plan.panes, Panes::Alone(Slot::Split));
        assert!(plan.shows(Slot::Split) && !plan.shows(Slot::Primary) && !plan.shows(Slot::Scratch));
        assert_eq!(plan.focus_token(), Some("S"));

        let plan = PanePlan::layout(&full, Some(zoom(&full, ZoomSurface::Primary)));
        assert!(plan.shows(Slot::Primary) && !plan.shows(Slot::Split) && !plan.shows(Slot::Scratch));
        assert_eq!(plan.focus_token(), Some("P"));

        let mut covered = session(Some(split(true, false)), true);
        covered.scratch_active = false;
        let plan = PanePlan::layout(&covered, Some(zoom(&covered, ZoomSurface::Scratch)));
        assert!(plan.shows(Slot::Scratch) && !plan.shows(Slot::Primary) && !plan.shows(Slot::Split));
        assert_eq!(plan.focus_token(), Some("X"));
    }

    /// Opens a program in `session`'s slot at `size_percent`.
    fn program(session: &mut Session, size_percent: Option<i64>) {
        let slot = &mut session.overlay;
        slot.active = true;
        slot.generation += 1;
        slot.command = Some("exec cat".to_owned());
        slot.size_percent = size_percent;
    }

    /// Opens an overlay over `pane` with `generation`.
    fn pane_overlay(session: &mut Session, pane: OverlayPane, generation: u64) {
        session.pane_overlays.slot_mut(pane).overlay = Some(agterm_core::model::PaneOverlay {
            command: "exec cat".to_owned(),
            cwd: None,
            background_color: None,
            wait: false,
            generation,
            code_file: None,
            html: None,
        });
    }

    #[test]
    fn a_covering_program_hides_the_panes_and_takes_the_keyboard() {
        let mut covered = session(Some(split(true, false)), true);
        program(&mut covered, None);
        let plan = PanePlan::layout(&covered, None);
        assert!(plan.overlays.covers);
        assert_eq!(
            plan.overlays.program,
            Some(ProgramShow {
                generation: 1,
                fraction: None
            })
        );
        assert!(!plan.shows(Slot::Primary) && !plan.shows(Slot::Split) && !plan.shows(Slot::Scratch));
        assert!(!plan.shows_divider());
        assert_eq!(plan.overlays.focus, Some(OverlayFocus::Program));
    }

    #[test]
    fn a_page_takes_the_slot_as_a_program_does() {
        use agterm_core::html::{HtmlOverlay, HtmlPageFlags, HtmlSource};
        let page = HtmlOverlay::new(
            HtmlSource::File {
                path: "/site/index.html".to_owned(),
                grant_root: None,
            },
            HtmlPageFlags::default(),
        );
        for (size_percent, fraction) in [(None, None), (Some(50), Some(0.5))] {
            let mut covered = session(Some(split(true, false)), false);
            let slot = &mut covered.overlay;
            slot.active = true;
            slot.generation = 4;
            slot.size_percent = size_percent;
            slot.html = Some(page.clone());
            let plan = PanePlan::layout(&covered, None);
            assert_eq!(
                plan.overlays.program,
                Some(ProgramShow {
                    generation: 4,
                    fraction
                })
            );
            assert_eq!(plan.overlays.covers, fraction.is_none());
            assert_eq!(plan.overlays.focus, Some(OverlayFocus::Program));
        }
    }

    #[test]
    fn a_floating_program_leaves_the_panes_lit_but_holds_the_keyboard() {
        let mut floating = session(Some(split(true, false)), false);
        program(&mut floating, Some(40));
        pane_overlay(&mut floating, OverlayPane::Right, 3);
        let plan = PanePlan::layout(&floating, None);
        assert!(!plan.overlays.covers && plan.shows(Slot::Primary) && plan.shows_divider());
        assert_eq!(plan.overlays.program.and_then(|program| program.fraction), Some(0.4));
        assert_eq!(plan.overlays.focus, Some(OverlayFocus::Program));
        assert_eq!(
            plan.overlays.pane(Slot::Split),
            Some(3),
            "a pane overlay shows beside it"
        );
    }

    #[test]
    fn a_pane_overlay_covers_its_laid_out_pane_and_takes_its_focus() {
        let mut both = session(Some(split(true, true)), false);
        pane_overlay(&mut both, OverlayPane::Right, 1);
        let plan = PanePlan::layout(&both, None);
        assert_eq!((plan.overlays.primary, plan.overlays.split), (None, Some(1)));
        assert_eq!(plan.overlays.focus, Some(OverlayFocus::Pane(Slot::Split)));
        both.split.focused = false;
        assert_eq!(
            PanePlan::layout(&both, None).overlays.focus,
            None,
            "the bare pane has focus"
        );

        let mut hidden = session(Some(split(false, false)), false);
        pane_overlay(&mut hidden, OverlayPane::Right, 2);
        assert_eq!(
            PanePlan::layout(&hidden, None).overlays.split,
            None,
            "its pane is not laid out"
        );
        let mut scratch = session(Some(split(true, true)), true);
        pane_overlay(&mut scratch, OverlayPane::Right, 2);
        let plan = PanePlan::layout(&scratch, None);
        assert_eq!(
            (plan.overlays.split, plan.overlays.focus),
            (None, None),
            "the scratch covers it"
        );
    }

    #[test]
    fn a_zoomed_overlay_fills_the_page() {
        let mut full = session(Some(split(true, false)), false);
        program(&mut full, Some(30));
        let plan = PanePlan::layout(&full, Some(zoom(&full, ZoomSurface::Overlay)));
        assert!(plan.overlays.covers);
        assert_eq!(plan.overlays.program.and_then(|program| program.fraction), None);

        let mut panes = session(Some(split(true, false)), false);
        pane_overlay(&mut panes, OverlayPane::Right, 4);
        let plan = PanePlan::layout(&panes, Some(zoom(&panes, ZoomSurface::OverlayRight)));
        assert_eq!(plan.panes, Panes::Alone(Slot::Split));
        assert_eq!(plan.overlays.focus, Some(OverlayFocus::Pane(Slot::Split)));
        let plan = PanePlan::layout(&panes, Some(zoom(&panes, ZoomSurface::Split)));
        assert_eq!(plan.overlays, OverlayPlan::default(), "a zoomed terminal shows bare");
    }

    #[test]
    fn a_floating_panel_is_centered_at_its_share() {
        let placement = Some(ProgramPlacement { fraction: Some(0.5) });
        assert_eq!(
            program_rect(placement, 1000, 600),
            Rect {
                x: 250,
                y: 150,
                width: 500,
                height: 300
            }
        );
        assert_eq!(
            program_rect(Some(ProgramPlacement { fraction: None }), 10, 20),
            Rect::full(10, 20)
        );
    }

    #[test]
    fn another_sessions_zoom_or_an_overlay_kind_changes_nothing() {
        let full = session(Some(split(true, false)), false);
        let other = Some(SurfaceId {
            session: Uuid::new_v4(),
            surface: ZoomSurface::Split,
        });
        assert_eq!(PanePlan::layout(&full, other), PanePlan::layout(&full, None));
        assert_eq!(
            PanePlan::layout(&full, Some(zoom(&full, ZoomSurface::Overlay))),
            PanePlan::layout(&full, None)
        );
    }

    #[test]
    fn the_split_terminal_is_created_once_laid_out() {
        let mut fresh = session(None, false);
        assert_eq!(needs(&fresh, None), Needs::default());
        fresh.split = split(true, true);
        fresh.split_pane_identity = Some(Uuid::new_v4());
        assert!(needs(&fresh, None).split);
        fresh.split_surface.replace(Box::new(FakeSurface::new("S")));
        assert!(!needs(&fresh, None).split, "it has one");

        let mut restored = session(None, false);
        restored.split = split(false, false);
        restored.split_pane_identity = Some(Uuid::new_v4());
        assert!(!needs(&restored, None).split, "a hidden split's unfocused pane waits");
        restored.split.focused = true;
        assert!(
            !needs(&restored, None).split,
            "focused but never laid out: the primary shows"
        );
        assert!(
            needs(&restored, Some(zoom(&restored, ZoomSurface::Split))).split,
            "a zoom lays it out"
        );
        restored.split.shown = true;
        assert!(needs(&restored, None).split);
    }

    #[test]
    fn the_scratch_terminal_is_created_while_shown_without_one() {
        let mut scratch = session(None, false);
        scratch.scratch_active = true;
        assert!(needs(&scratch, None).scratch);
        scratch.scratch_surface.replace(Box::new(FakeSurface::new("X")));
        assert!(!needs(&scratch, None).scratch);
    }

    #[test]
    fn a_lone_pane_and_the_one_under_it_fill_the_page() {
        let geometry = geometry(Panes::Alone(Slot::Split), SplitAxis::LeftRight, 0.3, 800, 600);
        assert_eq!(geometry.primary, Rect::full(800, 600));
        assert_eq!(geometry.split, Rect::full(800, 600));
        assert_eq!(geometry.divider, None);
    }

    #[test]
    fn both_panes_split_the_axis_at_the_ratio() {
        let geometry = geometry(Panes::Both, SplitAxis::LeftRight, 0.5, 801, 600);
        assert_eq!(
            geometry.primary,
            Rect {
                x: 0,
                y: 0,
                width: 400,
                height: 600
            }
        );
        assert_eq!(
            geometry.split,
            Rect {
                x: 401,
                y: 0,
                width: 400,
                height: 600
            }
        );
        let divider = geometry.divider.unwrap();
        assert!(
            divider.x < 400 && divider.x + divider.width > 401,
            "the band straddles the separator"
        );

        let geometry = geometry_top_bottom();
        assert_eq!(
            geometry.primary,
            Rect {
                x: 0,
                y: 0,
                width: 800,
                height: 150
            }
        );
        assert_eq!(
            geometry.split,
            Rect {
                x: 0,
                y: 151,
                width: 800,
                height: 450
            }
        );
        assert_eq!(geometry.divider.unwrap().width, 800);
    }

    fn geometry_top_bottom() -> Geometry {
        geometry(Panes::Both, SplitAxis::TopBottom, 0.25, 800, 601)
    }

    #[test]
    fn a_degenerate_page_never_allocates_a_negative_size() {
        let geometry = geometry(Panes::Both, SplitAxis::LeftRight, 0.9, 0, 0);
        assert!(geometry.primary.width >= 0 && geometry.split.width >= 0);
    }

    #[test]
    fn a_drag_moves_the_ratio_within_its_bounds() {
        assert!((dragged_ratio(0.5, 100.0, 1001) - 0.6).abs() < 1e-9);
        assert!((dragged_ratio(0.5, -10_000.0, 1001) - 0.05).abs() < 1e-9);
        assert!((dragged_ratio(0.5, 10_000.0, 1001) - 0.95).abs() < 1e-9);
        assert!((dragged_ratio(0.4, 50.0, 1) - 0.4).abs() < f64::EPSILON);
    }
}
