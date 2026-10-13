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

use agterm_core::model::pane::{SPLIT_RATIO_DEFAULT, clamp_split_ratio};
use agterm_core::model::{Session, SplitAxis, StatusPane};
use agterm_core::zoom::SurfaceId;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use uuid::Uuid;

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
}

/// Which of the split's panes the page lays out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panes {
    /// Both, beside each other with the divider between: the split is shown.
    Both,
    /// One pane, maximized: the split is hidden or absent, or the window is zoomed into it.
    Alone(Slot),
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
    /// The slot that should hold keyboard focus while the page shows.
    pub focus: Slot,
}

impl PanePlan {
    /// The layout of `session` under the window's `zoom` (upstream's `sessionDetail`): a shown split lays both panes
    /// out, a hidden one only the focused pane, maximized; a shown scratch covers them and takes the keyboard, else
    /// the focused pane does. Zoomed into one of the session's terminals, the page shows that terminal alone.
    #[must_use]
    pub fn layout(session: &Session, zoom: Option<SurfaceId>) -> Self {
        let (panes, scratch_covers, focus) = match zoomed_pane(session, zoom) {
            Some(StatusPane::Scratch) => (unzoomed_panes(session), true, Slot::Scratch),
            Some(pane) => (Panes::Alone(Slot::of(pane)), false, Slot::of(pane)),
            None if session.scratch_active => (unzoomed_panes(session), true, Slot::Scratch),
            None if session.focuses_split() => (unzoomed_panes(session), false, Slot::Split),
            None => (unzoomed_panes(session), false, Slot::Primary),
        };
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
        }
    }

    /// Whether the page shows the terminal at `slot`.
    #[must_use]
    pub fn shows(&self, slot: Slot) -> bool {
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
        self.panes == Panes::Both && !self.scratch_covers
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
    fn full(width: i32, height: i32) -> Self {
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
#[derive(Debug, Default)]
struct StackLayout {
    panes: Option<Panes>,
    axis: SplitAxis,
    ratio: f64,
    primary: Option<gtk::Widget>,
    split: Option<gtk::Widget>,
    divider: Option<gtk::Widget>,
    drag: DividerDrag,
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

    fn allocate(&self, stack: &gtk::Widget, width: i32, height: i32) {
        let geometry = geometry(
            self.panes.unwrap_or(Panes::Alone(Slot::Primary)),
            self.axis,
            self.ratio(),
            width,
            height,
        );
        let mut child = stack.first_child();
        while let Some(widget) = child {
            let rect = if Some(&widget) == self.primary.as_ref() {
                geometry.primary
            } else if Some(&widget) == self.split.as_ref() {
                geometry.split
            } else if Some(&widget) == self.divider.as_ref() {
                geometry.divider.unwrap_or_default()
            } else {
                Rect::full(width, height)
            };
            widget.size_allocate(&gtk::Allocation::new(rect.x, rect.y, rect.width, rect.height), -1);
            child = widget.next_sibling();
        }
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
        }
    }

    impl WidgetImpl for PaneStack {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            self.layout.borrow().allocate(self.obj().upcast_ref(), width, height);
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
    plan: Option<PanePlan>,
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
        let view = Self {
            stack,
            divider,
            separator,
            panes: Vec::new(),
            plan: None,
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
            widget.set_parent(&self.stack);
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
    }

    /// Lays the terminals out by `plan`; with the page `shown`, the terminals it shows become visible.
    pub fn apply(&mut self, plan: PanePlan, shown: bool) {
        {
            let mut layout = self.stack.layout();
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
            let visible = shown && plan.slot_of(&pane.token).is_some_and(|slot| plan.shows(slot));
            if pane.terminal.is_deck_visible() != visible {
                set_pane_visible(&pane.terminal, visible);
            }
        }
        self.orient_divider(plan.axis);
        self.divider.set_visible(shown && plan.shows_divider());
        self.plan = Some(plan);
    }

    /// Hides every terminal: the session is out of the tree, held by a pending close.
    pub fn hide(&mut self) {
        for pane in &self.panes {
            if pane.terminal.is_deck_visible() {
                set_pane_visible(&pane.terminal, false);
            }
        }
        self.divider.set_visible(false);
    }

    /// The token and terminal that should hold keyboard focus by the last plan.
    #[must_use]
    pub fn focus(&self) -> Option<(&str, &TerminalSurface)> {
        let token = self.plan.as_ref()?.focus_token()?;
        self.terminal(token).map(|terminal| (token, terminal))
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
