//! The dashboard grid (upstream's `Views/DashboardView.swift` and `Views/WindowContentView+Dashboard.swift`): while a
//! window's dashboard is open its deck shows every member pane's own terminal at that pane's grid cell, view-only, on
//! an opaque backdrop, under a chrome layer that draws each cell's frame and name chip, rings the highlighted one and
//! takes the clicks and keys.
//!
//! A live `GtkGLArea` is never re-parented, so the grid is laid out in place: each member session's page shows, and
//! its [`crate::split::PaneStack`] allocates the member panes at their cells (deck coordinates, from [`cell_rects`])
//! and hides the rest. The terminals take no input and no focus (upstream's `.allowsHitTesting(false)`); the
//! [`Chrome`] above them holds the keyboard: the arrows walk the highlight, Enter enters the highlighted cell, Esc
//! closes, and every other key is swallowed so nothing reaches a terminal behind. A click flashes the clicked cell's
//! ring, then enters it after [`CLICK_ENTER_DELAY`] unless another click or key moved the highlight meanwhile.
//!
//! A fixed or auto font mode gives every member terminal a transient font size while it is in the grid, put back when
//! it leaves ([`FontOverrides`]). A member pane under an overlay shows the overlay's cover over its cell (upstream's
//! `DashboardCover`). Not ported yet: the status-colored name chip.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::Duration;

use agterm_core::Core;
use agterm_core::dashboard::{DashboardController, DashboardCover, DashboardMember, Direction, MAX_CELLS, grid};
use agterm_core::model::{AppStore, OverlayPane, Session, SplitAxis};
use agterm_core::zoom::ZoomSurface;
use agterm_ghostty::SurfaceKey;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, pango};
use uuid::Uuid;

use crate::split::Rect;
use crate::state::{self, AppState, Poster};
use crate::surface::TerminalSurface;

/// The gap between cells and around the grid, in pixels (upstream's `gridSpacing`).
pub const GAP: i32 = 12;
/// How long a clicked cell's ring flashes before the click enters it (upstream's `dashboardClickEnterDelay`).
pub const CLICK_ENTER_DELAY: Duration = Duration::from_millis(180);
/// How far a name chip hangs below its cell's bottom edge, so it straddles the frame line.
const CAPTION_DROP: i32 = 8;
/// The name chip's inset from its cell's right edge.
const CAPTION_INSET: i32 = 6;

/// The style of the backdrop, the frames and the chips, themed by libadwaita's named colors.
const CSS: &str = "
.agterm-dashboard-backdrop { background-color: @view_bg_color; }
.agterm-dashboard-cell { border: 1px solid alpha(@view_fg_color, 0.12); border-radius: 6px; }
.agterm-dashboard-cell.highlighted { border: 2px solid @view_fg_color; }
.agterm-dashboard-caption {
  background-color: mix(@view_bg_color, @accent_bg_color, 0.3);
  color: alpha(@view_fg_color, 0.55);
  border-radius: 999px;
  padding: 1px 6px;
  font-size: smaller;
}
.agterm-dashboard-caption.highlighted { color: @view_fg_color; }
.agterm-dashboard-cell.covered { background-color: @view_bg_color; }
.agterm-dashboard-cover-source { font-size: smaller; }
";

/// The CSS class the deck's backdrop carries while its window's dashboard is open.
pub const BACKDROP_CLASS: &str = "agterm-dashboard-backdrop";

/// The rectangles of `count` cells in a `width`×`height` deck, row-major: the grid's columns and rows share what the
/// [`GAP`]s leave, the last pixels going to the last column and row.
#[must_use]
pub fn cell_rects(count: usize, width: i32, height: i32) -> Vec<Rect> {
    let count = count.min(MAX_CELLS);
    let (cols, rows) = grid(count);
    let span = |extent: i32, cells: usize, index: usize| {
        let cells = i32::try_from(cells).unwrap_or(1);
        let index = i32::try_from(index).unwrap_or(0);
        let usable = (extent - GAP * (cells + 1)).max(0);
        let start = usable * index / cells;
        let end = usable * (index + 1) / cells;
        (GAP * (index + 1) + start, end - start)
    };
    (0..count)
        .map(|index| {
            let (col, row) = agterm_core::dashboard::cell(index, cols);
            let (x, width) = span(width, cols, col);
            let (y, height) = span(height, rows, row);
            Rect { x, y, width, height }
        })
        .collect()
}

/// The index of the cell of `rects` holding the point `(x, y)`.
#[must_use]
pub fn cell_at(rects: &[Rect], x: f64, y: f64) -> Option<usize> {
    rects.iter().position(|rect| {
        x >= f64::from(rect.x)
            && x < f64::from(rect.x + rect.width)
            && y >= f64::from(rect.y)
            && y < f64::from(rect.y + rect.height)
    })
}

/// The pane marker after a cell's name: which pane of a split session it hosts, by the split's axis; nothing for a
/// session without a split.
#[must_use]
pub fn pane_marker(member: DashboardMember, session: &Session) -> &'static str {
    let top_bottom = session.split.axis == SplitAxis::TopBottom;
    match (member.surface, top_bottom) {
        (ZoomSurface::Split, true) => " ▼",
        (ZoomSurface::Split, false) => " ▶",
        _ if !session.split.exists => "",
        (_, true) => " ▲",
        (_, false) => " ◀",
    }
}

/// What the deck renders of an open dashboard.
#[derive(Debug, Clone, PartialEq)]
pub struct GridModel {
    /// The cells, row-major.
    pub members: Vec<DashboardMember>,
    pub highlighted: Option<DashboardMember>,
    /// The font size the members take, `None` to leave each at its own.
    pub font_size: Option<f64>,
}

impl GridModel {
    /// Window `window`'s open dashboard.
    #[must_use]
    pub fn of(core: &Core, window: Uuid) -> Option<Self> {
        let controller = core.dashboard(window)?;
        Some(Self {
            members: controller.members().to_vec(),
            highlighted: controller.highlighted(),
            font_size: controller.applied_font_size(),
        })
    }

    /// The cells session `session` hosts: each one's index and pane.
    pub fn cells_of(&self, session: Uuid) -> impl Iterator<Item = (usize, ZoomSurface)> + '_ {
        self.members
            .iter()
            .enumerate()
            .filter(move |(_, member)| member.session == session)
            .map(|(index, member)| (index, member.surface))
    }

    /// Whether session `session` hosts a cell.
    #[must_use]
    pub fn hosts(&self, session: Uuid) -> bool {
        self.members.iter().any(|member| member.session == session)
    }

    /// What the chrome draws over each cell, read from `store`.
    #[must_use]
    pub fn chrome_cells(&self, store: Option<&AppStore>) -> Vec<ChromeCell> {
        self.members
            .iter()
            .map(|member| {
                let caption = store
                    .and_then(|store| store.session(member.session))
                    .map(|session| format!("{}{}", session.display_name(), pane_marker(*member, session)))
                    .unwrap_or_default();
                let pane = if member.surface == ZoomSurface::Split {
                    OverlayPane::Right
                } else {
                    OverlayPane::Left
                };
                let cover = store
                    .and_then(|store| store.session(member.session))
                    .and_then(|session| session.dashboard_cover(pane));
                ChromeCell {
                    member: *member,
                    caption,
                    highlighted: self.highlighted == Some(*member),
                    cover,
                }
            })
            .collect()
    }
}

// MARK: - fonts

/// One terminal's transient grid font size and the size it had before.
struct FontOverride {
    terminal: TerminalSurface,
    original: f64,
    applied: f64,
}

/// The transient font sizes the grid gives its member terminals (upstream's `dashboardFontOverride`): set while a
/// terminal sits in a cell under a fixed or auto mode, and put back to the size it had when it leaves the grid or the
/// grid closes. A terminal whose surface is not up yet gets its size on a later render. While overridden a terminal's
/// size reports are not persisted, nor is the revert's (see [`TerminalSurface::set_font_override`]).
#[derive(Default)]
pub struct FontOverrides {
    by_key: HashMap<SurfaceKey, FontOverride>,
}

impl FontOverrides {
    /// Gives `members` the font `size` (none: their own) and puts every other overridden terminal back.
    pub fn sync(&mut self, members: &[TerminalSurface], size: Option<f64>) {
        let wanted: Vec<&TerminalSurface> = match size {
            Some(_) => members.iter().filter(|terminal| terminal.is_realized()).collect(),
            None => Vec::new(),
        };
        let keep: Vec<SurfaceKey> = wanted.iter().map(|terminal| terminal.key()).collect();
        self.by_key.retain(|key, entry| {
            let stays = keep.contains(key);
            if !stays {
                set_font_size(&entry.terminal, entry.original);
                entry.terminal.clear_font_override(entry.original);
            }
            stays
        });
        let Some(size) = size else { return };
        for terminal in wanted {
            self.apply(terminal, size);
        }
    }

    fn apply(&mut self, terminal: &TerminalSurface, size: f64) {
        match self.by_key.get_mut(&terminal.key()) {
            Some(entry) if (entry.applied - size).abs() < f64::EPSILON => {}
            Some(entry) => {
                entry.applied = size;
                terminal.set_font_override(size);
                set_font_size(terminal, size);
            }
            None => {
                let Some(original) = terminal.font_size() else { return };
                terminal.set_font_override(size);
                set_font_size(terminal, size);
                let entry = FontOverride {
                    terminal: terminal.clone(),
                    original,
                    applied: size,
                };
                self.by_key.insert(terminal.key(), entry);
            }
        }
    }
}

/// Sets a terminal's font size through libghostty's keybind action, which keeps it across a config update.
fn set_font_size(terminal: &TerminalSurface, size: f64) {
    terminal.binding_action(&format!("set_font_size:{size}"));
}

// MARK: - the chrome

/// What the chrome draws over one cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeCell {
    pub member: DashboardMember,
    /// The session's name and the pane marker.
    pub caption: String,
    pub highlighted: bool,
    /// What hides the member pane's terminal in its session's own view, which the cell then names instead.
    pub cover: Option<DashboardCover>,
}

/// A key the grid acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridKey {
    Move(Direction),
    Enter,
    Close,
}

/// The grid's reading of a key press; every other key is swallowed.
#[must_use]
pub fn grid_key(key: gdk::Key) -> Option<GridKey> {
    match key {
        gdk::Key::Left | gdk::Key::KP_Left => Some(GridKey::Move(Direction::Left)),
        gdk::Key::Right | gdk::Key::KP_Right => Some(GridKey::Move(Direction::Right)),
        gdk::Key::Up | gdk::Key::KP_Up => Some(GridKey::Move(Direction::Up)),
        gdk::Key::Down | gdk::Key::KP_Down => Some(GridKey::Move(Direction::Down)),
        gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => Some(GridKey::Enter),
        gdk::Key::Escape => Some(GridKey::Close),
        _ => None,
    }
}

/// One cell's frame, the cover it shows over a covered pane, and its name chip.
struct CellChrome {
    frame: gtk::Box,
    cover: CoverLabels,
    caption: gtk::Label,
}

/// The stand-in for a covered member pane (upstream's `DashboardOverlayCover`): the kind of cover, its app-derived
/// source, and a page's own title as a separate dimmed line.
struct CoverLabels {
    column: gtk::Box,
    kind: gtk::Label,
    source: gtk::Label,
    title: gtk::Label,
}

impl CoverLabels {
    fn new(frame: &gtk::Box) -> Self {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
        column.set_hexpand(true);
        column.set_halign(gtk::Align::Center);
        column.set_valign(gtk::Align::Center);
        column.set_can_target(false);
        let label = |class: Option<&str>, ellipsize: pango::EllipsizeMode| {
            let label = gtk::Label::new(None);
            label.set_ellipsize(ellipsize);
            label.set_single_line_mode(true);
            if let Some(class) = class {
                label.add_css_class(class);
            }
            column.append(&label);
            label
        };
        let kind = label(None, pango::EllipsizeMode::End);
        let source = label(Some("agterm-dashboard-cover-source"), pango::EllipsizeMode::Middle);
        let title = label(Some("dim-label"), pango::EllipsizeMode::End);
        frame.append(&column);
        column.set_visible(false);
        Self {
            column,
            kind,
            source,
            title,
        }
    }

    /// Shows `cover`, or nothing.
    fn update(&self, frame: &gtk::Box, cover: Option<&DashboardCover>) {
        let Some(cover) = cover else {
            self.column.set_visible(false);
            frame.remove_css_class("covered");
            return;
        };
        let (kind, source, title) = match cover {
            DashboardCover::Page { identity, title } => ("HTML overlay", Some(identity.as_str()), title.as_deref()),
            DashboardCover::Program { command } => ("Program overlay", command.as_deref(), None),
        };
        self.kind.set_label(kind);
        self.source.set_label(source.unwrap_or_default());
        self.source.set_visible(source.is_some());
        self.title.set_label(title.unwrap_or_default());
        self.title.set_visible(title.is_some());
        self.column.set_visible(true);
        frame.add_css_class("covered");
    }
}

/// What [`ChromeWidget`] lays out and hit-tests.
#[derive(Default)]
struct ChromeLayout {
    members: Vec<DashboardMember>,
    cells: Vec<CellChrome>,
}

impl ChromeLayout {
    fn allocate(&self, width: i32, height: i32) {
        let rects = cell_rects(self.cells.len(), width, height);
        for (cell, rect) in self.cells.iter().zip(rects) {
            cell.frame
                .size_allocate(&gtk::Allocation::new(rect.x, rect.y, rect.width, rect.height), -1);
            let (_, natural) = cell.caption.preferred_size();
            let caption_width = natural.width().min(rect.width - 2 * CAPTION_INSET).max(0);
            let caption_height = natural.height();
            let x = rect.x + rect.width - CAPTION_INSET - caption_width;
            let y = rect.y + rect.height + CAPTION_DROP - caption_height;
            cell.caption
                .size_allocate(&gtk::Allocation::new(x, y, caption_width, caption_height), -1);
        }
    }
}

mod imp {
    use std::cell::RefCell;

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    use super::ChromeLayout;

    #[derive(Default)]
    pub struct ChromeWidget {
        pub(super) layout: RefCell<ChromeLayout>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ChromeWidget {
        const NAME: &'static str = "AgtermDashboardChrome";
        type Type = super::ChromeWidget;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ChromeWidget {
        fn dispose(&self) {
            let widget = self.obj();
            while let Some(child) = widget.first_child() {
                child.unparent();
            }
            self.layout.borrow_mut().cells.clear();
        }
    }

    impl WidgetImpl for ChromeWidget {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            self.layout.borrow().allocate(width, height);
        }
    }
}

glib::wrapper! {
    /// The layer over the grid: the cells' frames and name chips, and the clicks and keys.
    pub struct ChromeWidget(ObjectSubclass<imp::ChromeWidget>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ChromeWidget {
    fn new() -> Self {
        glib::Object::builder()
            .property("hexpand", true)
            .property("vexpand", true)
            .property("focusable", true)
            .property("visible", false)
            .build()
    }

    /// The member under the point `(x, y)` of the widget.
    fn member_at(&self, x: f64, y: f64) -> Option<DashboardMember> {
        let layout = self.imp().layout.borrow();
        let rects = cell_rects(layout.members.len(), self.width(), self.height());
        cell_at(&rects, x, y).and_then(|index| layout.members.get(index).copied())
    }
}

/// The chrome of one window's grid.
pub struct Chrome {
    widget: ChromeWidget,
}

impl Chrome {
    #[must_use]
    pub fn new() -> Self {
        install_css();
        Self {
            widget: ChromeWidget::new(),
        }
    }

    #[must_use]
    pub fn widget(&self) -> &ChromeWidget {
        &self.widget
    }

    /// Routes the clicks and keys to window `window`'s dashboard through `poster`.
    pub fn connect(&self, window: Uuid, poster: &Poster) {
        let click = gtk::GestureClick::new();
        let weak = self.widget.downgrade();
        let posting = poster.clone();
        click.connect_pressed(move |_, _, x, y| {
            if let Some(member) = weak.upgrade().and_then(|chrome| chrome.member_at(x, y)) {
                clicked(&posting, window, member);
            }
        });
        self.widget.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        let posting = poster.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if let Some(key) = grid_key(key) {
                state::post(&posting, move |state: &mut AppState| state.dashboard_key(window, key));
            }
            glib::Propagation::Stop
        });
        self.widget.add_controller(keys);
    }

    /// Shows the chrome over `cells`, or hides it with none.
    pub fn update(&self, cells: &[ChromeCell]) {
        self.widget.set_visible(!cells.is_empty());
        let mut layout = self.widget.imp().layout.borrow_mut();
        while layout.cells.len() > cells.len() {
            if let Some(cell) = layout.cells.pop() {
                cell.frame.unparent();
                cell.caption.unparent();
            }
        }
        while layout.cells.len() < cells.len() {
            layout.cells.push(cell_chrome(&self.widget));
        }
        for (chrome, cell) in layout.cells.iter().zip(cells) {
            chrome.caption.set_label(&cell.caption);
            chrome.cover.update(&chrome.frame, cell.cover.as_ref());
            for widget in [chrome.frame.upcast_ref::<gtk::Widget>(), chrome.caption.upcast_ref()] {
                if cell.highlighted {
                    widget.add_css_class("highlighted");
                } else {
                    widget.remove_css_class("highlighted");
                }
            }
        }
        layout.members = cells.iter().map(|cell| cell.member).collect();
        drop(layout);
        self.widget.queue_allocate();
    }

    /// Takes the keyboard unless it holds it (upstream's key catcher re-asserts first responder every render).
    pub fn grab_focus(&self) {
        if self.widget.get_visible() && !self.widget.has_focus() {
            self.widget.grab_focus();
        }
    }
}

impl Default for Chrome {
    fn default() -> Self {
        Self::new()
    }
}

/// A new cell's frame and chip on `chrome`, neither taking a click: the chrome hit-tests the cells itself.
fn cell_chrome(chrome: &ChromeWidget) -> CellChrome {
    let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    frame.add_css_class("agterm-dashboard-cell");
    frame.set_can_target(false);
    frame.set_parent(chrome);
    let cover = CoverLabels::new(&frame);
    let caption = gtk::Label::new(None);
    caption.add_css_class("agterm-dashboard-caption");
    caption.set_ellipsize(pango::EllipsizeMode::Middle);
    caption.set_single_line_mode(true);
    caption.set_can_target(false);
    caption.set_parent(chrome);
    CellChrome { frame, cover, caption }
}

/// A click highlights the cell at once, then enters it after the flash if it is still the highlighted one.
fn clicked(poster: &Poster, window: Uuid, member: DashboardMember) {
    state::post(poster, move |state: &mut AppState| {
        state.dashboard_click(window, member);
    });
    let posting = poster.clone();
    glib::timeout_add_local_once(CLICK_ENTER_DELAY, move || {
        state::post(&posting, move |state: &mut AppState| {
            state.dashboard_click_enter(window, member);
        });
    });
}

thread_local! {
    static CSS_INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Adds the grid's style to the default display, once.
fn install_css() {
    if CSS_INSTALLED.with(Cell::get) {
        return;
    }
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    CSS_INSTALLED.with(|installed| installed.set(true));
}

// MARK: - the actions

/// The grid's actions, each a main-loop task ending in a settle.
impl AppState {
    /// A key on window `window`'s grid: the arrows walk the highlight, Enter enters it, Esc closes.
    pub fn dashboard_key(&mut self, window: Uuid, key: GridKey) {
        match key {
            GridKey::Move(direction) => self.core.dashboard_move(window, direction),
            GridKey::Enter => {
                let highlighted = self.core.dashboard(window).and_then(DashboardController::highlighted);
                if let Some(member) = highlighted {
                    self.core.dashboard_enter(window, member);
                }
            }
            GridKey::Close => self.core.dashboard_close(window),
        }
        self.settle();
    }

    /// A click on `member`'s cell: its ring flashes.
    pub fn dashboard_click(&mut self, window: Uuid, member: DashboardMember) {
        self.core.dashboard_highlight(window, member);
        self.settle();
    }

    /// The flash of a click ran out: the cell is entered unless another click or key moved the highlight, or the grid
    /// closed.
    pub fn dashboard_click_enter(&mut self, window: Uuid, member: DashboardMember) {
        let still = self
            .core
            .dashboard(window)
            .is_some_and(|grid| grid.highlighted() == Some(member));
        if still {
            self.core.dashboard_enter(window, member);
            self.settle();
        }
    }

    /// The header bar's dashboard button: the window's most recently used sessions, auto-sized, or closes the grid.
    pub fn toggle_dashboard(&mut self, window: Uuid) {
        self.core.toggle_dashboard(window);
        self.settle();
    }

    /// The header bar's exit button.
    pub fn exit_dashboard(&mut self, window: Uuid) {
        self.core.dashboard_close(window);
        self.settle();
    }
}

#[cfg(test)]
mod tests {
    use agterm_core::model::{SessionSeed, SplitLayout};

    use super::*;

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect { x, y, width, height }
    }

    #[test]
    fn one_cell_fills_the_deck_inside_the_gap() {
        assert_eq!(cell_rects(1, 800, 600), vec![rect(12, 12, 776, 576)]);
    }

    #[test]
    fn cells_share_the_space_the_gaps_leave_row_major() {
        let rects = cell_rects(3, 812, 612);
        // 2×2: (812 - 36) / 2 = 388 wide, (612 - 36) / 2 = 288 high
        assert_eq!(
            rects,
            vec![rect(12, 12, 388, 288), rect(412, 12, 388, 288), rect(12, 312, 388, 288)]
        );
        let nine = cell_rects(12, 900, 900);
        assert_eq!(nine.len(), MAX_CELLS, "capped");
        let last = nine[8];
        assert_eq!(last.x + last.width, 900 - GAP);
        assert_eq!(last.y + last.height, 900 - GAP);
    }

    #[test]
    fn a_degenerate_deck_never_gets_a_negative_cell() {
        assert!(
            cell_rects(9, 10, 0)
                .iter()
                .all(|rect| rect.width >= 0 && rect.height >= 0)
        );
        assert!(cell_rects(0, 800, 600).is_empty());
    }

    #[test]
    fn a_point_hits_the_cell_holding_it_and_a_gap_none() {
        let rects = cell_rects(2, 812, 600);
        assert_eq!(cell_at(&rects, 20.0, 20.0), Some(0));
        assert_eq!(cell_at(&rects, 500.0, 300.0), Some(1));
        assert_eq!(cell_at(&rects, 5.0, 300.0), None);
        assert_eq!(cell_at(&rects, 405.0, 300.0), None);
    }

    #[test]
    fn the_pane_marker_follows_the_split_axis() {
        let mut session = Session::new(SessionSeed::default());
        let id = session.id();
        assert_eq!(pane_marker(DashboardMember::primary(id), &session), "");
        session.split = SplitLayout {
            exists: true,
            ..SplitLayout::default()
        };
        assert_eq!(pane_marker(DashboardMember::primary(id), &session), " ◀");
        assert_eq!(pane_marker(DashboardMember::split(id), &session), " ▶");
        session.split.axis = SplitAxis::TopBottom;
        assert_eq!(pane_marker(DashboardMember::primary(id), &session), " ▲");
        assert_eq!(pane_marker(DashboardMember::split(id), &session), " ▼");
    }

    #[test]
    fn the_grid_reads_arrows_enter_and_escape() {
        assert_eq!(grid_key(gdk::Key::Left), Some(GridKey::Move(Direction::Left)));
        assert_eq!(grid_key(gdk::Key::KP_Down), Some(GridKey::Move(Direction::Down)));
        assert_eq!(grid_key(gdk::Key::Return), Some(GridKey::Enter));
        assert_eq!(grid_key(gdk::Key::KP_Enter), Some(GridKey::Enter));
        assert_eq!(grid_key(gdk::Key::Escape), Some(GridKey::Close));
        assert_eq!(grid_key(gdk::Key::a), None);
    }

    #[test]
    fn the_model_names_each_cell_and_marks_the_highlight() {
        let mut store = AppStore::new();
        let workspace = store.add_workspace("work");
        let id = store
            .add_session(workspace, agterm_core::model::NewSession::at_cwd("/srv/app"))
            .unwrap();
        store.set_split_visibility(id, true, None);
        let model = GridModel {
            members: vec![DashboardMember::primary(id), DashboardMember::split(id)],
            highlighted: Some(DashboardMember::split(id)),
            font_size: None,
        };
        let cells = model.chrome_cells(Some(&store));
        assert_eq!(cells[0].caption, "app ◀");
        assert_eq!(cells[1].caption, "app ▶");
        assert_eq!(
            cells.iter().map(|cell| cell.highlighted).collect::<Vec<_>>(),
            [false, true]
        );
        assert_eq!(
            model.cells_of(id).collect::<Vec<_>>(),
            [(0, ZoomSurface::Primary), (1, ZoomSurface::Split)]
        );
        assert!(model.hosts(id) && !model.hosts(Uuid::new_v4()));
    }
}
