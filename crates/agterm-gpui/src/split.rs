//! One session's panes on its deck page: the primary terminal, the split terminal and the scratch terminal, with the
//! divider between the split's panes (upstream's `sessionDetail`/`shownSplit` in `WindowContentView+Detail.swift` and
//! `SplitRatioAccessor.swift`; ported from the GTK build's `split.rs`).
//!
//! Every terminal of a page keeps its view for its whole life: showing or hiding the split, transposing it, moving the
//! divider, swapping the panes, zooming one and covering them with the scratch only change where each view is laid out
//! and whether it is drawn. A pane the layout does not show is still laid out (at the full page size, under the shown
//! one), so its pane starts and follows the size, but is not drawn, hit-tested or focused, as the deck's hidden pages
//! are.
//!
//! What goes where is a pure function of the session and the window's zoom ([`PanePlan::layout`], [`needs`]), and the
//! rectangles a pure function of the plan and the page size ([`geometry`]). [`PageElement`] is the page's container:
//! it lays each child out at the rectangle its [`Place`] names in the page's bounds, as the GTK build's `PaneStack`
//! allocates, and reports the page's geometry to the model, which measures a HUD against it (upstream's
//! `hudPaneFrames`).
//!
//! The divider is a grab band over the separator: a drag moves it live and persists the ratio when it ends, a
//! double-click on an unmoved divider restores the even split.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use agterm_core::hud::{HudPaneFrame, HudPaneFrames};
use agterm_core::model::pane::{SPLIT_RATIO_DEFAULT, clamp_split_ratio};
use agterm_core::model::{OverlayPane, Session, SplitAxis, StatusPane};
use agterm_core::zoom::{SurfaceId, ZoomSurface};
use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, DispatchPhase, Element, ElementId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, MouseMoveEvent, MouseUpEvent, Pixels, Point, Style, Window, point, px, relative, size,
};

/// The separator line between the split's panes, in pixels.
pub const SEPARATOR: i32 = 1;
/// The divider's grab band, centered on the separator and reaching over both panes' edges.
pub const GRAB_BAND: i32 = 7;

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
    #[must_use]
    pub fn of(pane: StatusPane) -> Self {
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

    /// Whether the terminal carrying `token` is drawn while the page shows: laid out, uncovered, and not under its own
    /// pane overlay, which takes its place.
    #[must_use]
    pub fn draws(&self, token: &str) -> bool {
        self.slot_of(token)
            .is_some_and(|slot| self.shows(slot) && self.overlays.pane(slot).is_none())
    }

    /// Whether the divider shows: both panes laid out and uncovered.
    #[must_use]
    pub fn shows_divider(&self) -> bool {
        self.panes == Panes::Both && !self.scratch_covers && !self.overlays.covers
    }

    /// The pane the inactive-pane mute washes, or its overlay (upstream's `paneDim`): the unfocused one of a shown,
    /// uncovered split, unless a floating program's backdrop washes the page already.
    #[must_use]
    pub fn muted(&self) -> Option<Slot> {
        if !self.shows_divider() || self.backdrop() {
            return None;
        }
        match self.focus {
            Slot::Primary => Some(Slot::Split),
            Slot::Split => Some(Slot::Primary),
            Slot::Scratch => None,
        }
    }

    /// Whether a floating program washes the page behind it (upstream's `OverlayPanelStyle.backdrop`).
    #[must_use]
    pub fn backdrop(&self) -> bool {
        self.overlays.program.is_some_and(|program| program.fraction.is_some())
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

/// A child's rectangle in the page, in whole logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    #[must_use]
    pub fn full(width: i32, height: i32) -> Self {
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

    /// Whether the rectangle covers no pixel.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
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

/// The separator line's rectangle inside the divider's grab band of `geometry`, along `axis`.
#[must_use]
pub fn separator_rect(geometry: &Geometry, axis: SplitAxis) -> Option<Rect> {
    let band = geometry.divider?;
    Some(match axis {
        SplitAxis::LeftRight => Rect {
            x: geometry.primary.x + geometry.primary.width,
            width: SEPARATOR,
            ..band
        },
        SplitAxis::TopBottom => Rect {
            y: geometry.primary.y + geometry.primary.height,
            height: SEPARATOR,
            ..band
        },
    })
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

/// The program panel's rectangle: centered at its share of the page across and down while it floats, else the page
/// (upstream's program `OverlayPanelStyle`, always centered).
#[must_use]
pub fn program_rect(fraction: Option<f64>, width: i32, height: i32) -> Rect {
    let Some(fraction) = fraction else {
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
#[must_use]
pub fn ask_rect(scope: Option<AskScope>, panes: Panes, geometry: &Geometry, width: i32, height: i32) -> Rect {
    match scope {
        Some(AskScope::Session) => Rect::full(width, height),
        Some(AskScope::Pane(slot)) => pane_rect(panes, geometry, slot).unwrap_or_default(),
        None => Rect::default(),
    }
}

/// The rectangle of the pane at `slot` (primary or split) while `panes` lays it out.
#[must_use]
pub fn pane_rect(panes: Panes, geometry: &Geometry, slot: Slot) -> Option<Rect> {
    match slot {
        Slot::Primary if lays_out(panes, slot) => Some(geometry.primary),
        Slot::Split if lays_out(panes, slot) => Some(geometry.split),
        _ => None,
    }
}

/// Whether `panes` lays the pane at `slot` out.
#[must_use]
pub fn lays_out(panes: Panes, slot: Slot) -> bool {
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
#[must_use]
pub fn page_frames(panes: Panes, geometry: &Geometry, width: i32, height: i32) -> HudPaneFrames {
    HudPaneFrames {
        left: lays_out(panes, Slot::Primary).then(|| frame_of(geometry.primary)),
        right: lays_out(panes, Slot::Split).then(|| frame_of(geometry.split)),
        detail: (width > 0 && height > 0).then(|| frame_of(Rect::full(width, height))),
    }
}

// MARK: - the page's layout

/// Where [`PageElement`] lays a child out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Place {
    /// The whole page.
    Full,
    /// The terminal laid out at this slot: the pane's rectangle, the whole page for the scratch.
    Pane(Slot),
    /// Over the pane at this slot while it is laid out, else the whole page (a lead cover, a wash).
    Over(Slot),
    /// The divider's grab band; nothing without one.
    Divider,
    /// The separator line inside the band; nothing without one.
    Separator,
    /// The HUD's panel, placed by its style in its scoped pane (nothing while that pane is not laid out) or the page.
    Hud(crate::hud::HudPlacement),
}

impl Place {
    pub(crate) fn rect(self, panes: Panes, geometry: &Geometry, axis: SplitAxis, width: i32, height: i32) -> Rect {
        let full = Rect::full(width, height);
        match self {
            Self::Full | Self::Pane(Slot::Scratch) => full,
            Self::Pane(Slot::Primary) => geometry.primary,
            Self::Pane(Slot::Split) => geometry.split,
            Self::Over(slot) => pane_rect(panes, geometry, slot).unwrap_or(full),
            Self::Divider => geometry.divider.unwrap_or_default(),
            Self::Separator => separator_rect(geometry, axis).unwrap_or_default(),
            Self::Hud(placement) => match placement.scope {
                Some(slot) => pane_rect(panes, geometry, slot)
                    .map(|pane| placement.style.panel_frame(pane))
                    .unwrap_or_default(),
                None => placement.style.panel_frame(full),
            },
        }
    }
}

/// The divider's drag: the ratio and pointer position it started from while one runs, the ratio it shows until the
/// model has it, and whether it moved since the last press (a second press is then a re-grab, not a double-click).
#[derive(Debug, Default, Clone, Copy)]
pub struct DividerDrag {
    pub start: Option<(f64, Point<Pixels>)>,
    pub ratio: Option<f64>,
    pub moved: bool,
}

/// A page's live layout state, shared by its [`PageElement`] each frame and the divider's handlers.
#[derive(Debug, Default)]
pub struct PageState {
    pub drag: RefCell<DividerDrag>,
    /// The page's size at the last layout, whole logical pixels.
    pub size: Cell<(i32, i32)>,
    /// The geometry last reported to the model.
    pub reported: Cell<Option<HudPaneFrames>>,
}

impl PageState {
    /// Starts a drag of the divider from `ratio` at pointer `position`; a press resets `moved`.
    pub fn begin_drag(&self, ratio: f64, position: Point<Pixels>) {
        let mut drag = self.drag.borrow_mut();
        drag.start = Some((ratio, position));
        drag.moved = false;
    }

    /// The pointer moved to `position` while a drag runs along `axis`: the ratio it shows; whether it changed.
    pub fn drag_to(&self, position: Point<Pixels>, axis: SplitAxis) -> bool {
        let mut drag = self.drag.borrow_mut();
        let Some((start, from)) = drag.start else {
            return false;
        };
        let (width, height) = self.size.get();
        let (offset, length) = match axis {
            SplitAxis::LeftRight => (f64::from(f32::from(position.x - from.x)), width),
            SplitAxis::TopBottom => (f64::from(f32::from(position.y - from.y)), height),
        };
        let ratio = dragged_ratio(start, offset, length);
        if drag.ratio.is_some_and(|current| (current - ratio).abs() < f64::EPSILON) {
            return false;
        }
        drag.ratio = Some(ratio);
        drag.moved = true;
        true
    }

    /// Ends the drag: the ratio to persist when it moved the divider.
    pub fn end_drag(&self) -> Option<f64> {
        let mut drag = self.drag.borrow_mut();
        let (start, _) = drag.start.take()?;
        drag.ratio.filter(|ratio| (ratio - start).abs() >= f64::EPSILON)
    }

    /// Whether a press with `clicks` is the second of a double-click on a divider that has not moved since the first.
    #[must_use]
    pub fn is_reset(&self, clicks: usize) -> bool {
        clicks == 2 && !self.drag.borrow().moved
    }

    /// Whether a drag runs.
    #[must_use]
    pub fn dragging(&self) -> bool {
        self.drag.borrow().start.is_some()
    }

    /// The ratio the page lays out at: a running drag's, else `model`'s.
    #[must_use]
    pub fn ratio(&self, model: f64) -> f64 {
        self.drag.borrow().ratio.unwrap_or(model)
    }

    /// The model took the ratio (a new plan with no drag running): the drag's own goes.
    pub fn settle(&self) {
        let mut drag = self.drag.borrow_mut();
        if drag.start.is_none() {
            drag.ratio = None;
        }
    }
}

/// What the page reports its geometry through, and what a finished drag persists through.
pub type FramesReport = Rc<dyn Fn(HudPaneFrames)>;
/// Persists a dragged ratio.
pub type RatioReport = Rc<dyn Fn(f64)>;

/// A session page's container (module docs): each child at its [`Place`] in z order.
pub struct PageElement {
    panes: Panes,
    axis: SplitAxis,
    ratio: f64,
    state: Rc<PageState>,
    children: Vec<(AnyElement, Place)>,
    frames: Option<FramesReport>,
    ratio_report: Option<RatioReport>,
}

impl PageElement {
    /// A page laying `panes` out along `axis` at the model's `ratio` (a running drag's wins).
    #[must_use]
    pub fn new(panes: Panes, axis: SplitAxis, ratio: f64, state: Rc<PageState>) -> Self {
        Self {
            panes,
            axis,
            ratio,
            state,
            children: Vec::new(),
            frames: None,
            ratio_report: None,
        }
    }

    /// Lays `child` out at `place`, above the children added before it.
    #[must_use]
    pub fn child(mut self, child: impl IntoElement, place: Place) -> Self {
        self.children.push((child.into_any_element(), place));
        self
    }

    /// Reports the page's geometry through `report` when it changes.
    #[must_use]
    pub fn report_frames(mut self, report: FramesReport) -> Self {
        self.frames = Some(report);
        self
    }

    /// Persists a finished drag's ratio through `report`.
    #[must_use]
    pub fn report_ratio(mut self, report: RatioReport) -> Self {
        self.ratio_report = Some(report);
        self
    }
}

impl IntoElement for PageElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// A whole number of logical pixels, rounded down, never negative.
fn whole(value: Pixels) -> i32 {
    let value = f32::from(value).floor().max(0.0);
    #[expect(clippy::cast_possible_truncation, reason = "a view's size fits an i32")]
    let value = value as i32;
    value
}

/// A rectangle of the page at `origin`, as GPUI bounds.
fn bounds_of(origin: Point<Pixels>, rect: Rect) -> Bounds<Pixels> {
    #[expect(clippy::cast_precision_loss, reason = "page coordinates are far below 2^24")]
    let (x, y, width, height) = (rect.x as f32, rect.y as f32, rect.width as f32, rect.height as f32);
    Bounds {
        origin: origin + point(px(x), px(y)),
        size: size(px(width.max(0.0)), px(height.max(0.0))),
    }
}

impl Element for PageElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let (width, height) = (whole(bounds.size.width), whole(bounds.size.height));
        self.state.size.set((width, height));
        let ratio = self.state.ratio(self.ratio);
        let geometry = geometry(self.panes, self.axis, ratio, width, height);
        for (child, place) in &mut self.children {
            let rect = place.rect(self.panes, &geometry, self.axis, width, height);
            let placed = bounds_of(bounds.origin, rect);
            let space = size(
                AvailableSpace::Definite(placed.size.width),
                AvailableSpace::Definite(placed.size.height),
            );
            child.prepaint_as_root(placed.origin, space, window, cx);
        }
        let frames = page_frames(self.panes, &geometry, width, height);
        if self.state.reported.get() != Some(frames)
            && let Some(report) = &self.frames
        {
            self.state.reported.set(Some(frames));
            report(frames);
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for (child, _) in &mut self.children {
            child.paint(window, cx);
        }
        if !self.state.dragging() {
            return;
        }
        // a running drag follows the pointer wherever it goes, and ends wherever the button comes up
        let (state, axis) = (Rc::clone(&self.state), self.axis);
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
            if phase == DispatchPhase::Bubble && state.drag_to(event.position, axis) {
                window.refresh();
            }
        });
        let (state, report) = (Rc::clone(&self.state), self.ratio_report.clone());
        window.on_mouse_event(move |_: &MouseUpEvent, phase, window, _| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            if let Some(ratio) = state.end_drag()
                && let Some(report) = &report
            {
                report(ratio);
            }
            window.refresh();
        });
    }
}

#[cfg(test)]
pub(crate) mod tests;
