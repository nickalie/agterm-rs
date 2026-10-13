//! `dashboard`: a window's view-only grid of pane cells (upstream's `ControlDispatcher+Dashboard.swift` for the
//! argument checks, `ControlServer.setDashboard` for the effect, the per-window `DashboardController` in
//! [`crate::dashboard`], and the GUI's actions from `WindowContentView+Dashboard.swift` and `AppActions.toggleDashboard`).
//!
//! Every window has its own dashboard, read back live by `tree` (`dashboardMembers`, `dashboardHighlighted`,
//! `dashboardFontSize`, `dashboardFontMode`). It follows the model: a cell whose session or split pane went away drops
//! out, and an emptied dashboard closes ([`Core::reconcile_dashboards`], which the frontend runs after every commit;
//! the read-back applies the same rule itself). A dashboard and a terminal zoom replace each other.

use std::collections::HashMap;

use agterm_proto::tree::ControlTree;
use agterm_proto::{ControlResult, Request};
use uuid::Uuid;

use super::args_of;
use super::surface::run_binding;
use crate::Core;
use crate::dashboard::{
    DashboardController, DashboardFontMode, DashboardMember, DashboardTarget, Direction, GHOSTTY_DEFAULT_FONT_SIZE,
    MAX_CELLS, ResolvedDashboardTarget,
};
use crate::error::ControlError;
use crate::model::AppStore;
use crate::resolve::{Resolver, TargetResolution, resolve, session_ids};
use crate::zoom::ZoomSurface;

/// Every window's open dashboard, by window id; a closed one has no entry.
#[derive(Debug, Default)]
pub struct Dashboards {
    by_window: HashMap<Uuid, DashboardController>,
}

/// What an open asks for, once the arguments passed their checks.
enum Open<'a> {
    Targets(&'a [String]),
    Mru,
}

/// Answers `dashboard`; `None` is a bare `ok`.
pub(crate) fn dispatch(core: &mut Core, request: &Request) -> Result<Option<ControlResult>, ControlError> {
    let args = args_of(request);
    let targets = args.targets.as_deref().unwrap_or_default();
    let font_size = args.font_size;
    let auto_size = args.auto_size.unwrap_or(false);
    let mru = args.mru.unwrap_or(false);
    let window = args.window.as_deref();
    if args.close == Some(true) {
        if !targets.is_empty() || mru || font_size.is_some() || auto_size {
            return Err(ControlError::rejected(
                "dashboard --close takes no ids, --mru, or font options",
            ));
        }
        let window = open_window(core, window)?;
        close(core, window);
        return Ok(None);
    }
    let font_mode = font_mode(font_size, auto_size)?;
    if mru {
        if !targets.is_empty() {
            return Err(ControlError::rejected(
                "dashboard --mru cannot be combined with explicit session ids",
            ));
        }
        return open(core, window, &Open::Mru, font_mode);
    }
    if targets.is_empty() {
        return Err(ControlError::rejected("dashboard requires at least one session id"));
    }
    // the grammar only: a well-formed ref naming nothing joins the `unresolved` note instead
    if let Some(malformed) = targets.iter().find(|raw| DashboardTarget::parse(raw).is_none()) {
        return Err(ControlError::rejected(format!(
            "dashboard: invalid session id '{malformed}' — use <id>, <id>:left, or <id>:right"
        )));
    }
    open(core, window, &Open::Targets(targets), font_mode)
}

/// The font mode of `--font-size`/`--auto-size`, which exclude each other; a size must be finite and positive.
fn font_mode(font_size: Option<f64>, auto_size: bool) -> Result<DashboardFontMode, ControlError> {
    if font_size.is_some() && auto_size {
        return Err(ControlError::rejected(
            "dashboard: --font-size is mutually exclusive with --auto-size",
        ));
    }
    match font_size {
        Some(size) if !size.is_finite() || size <= 0.0 => Err(ControlError::rejected(
            "dashboard --font-size must be a positive number",
        )),
        Some(size) => Ok(DashboardFontMode::Fixed(size)),
        None if auto_size => Ok(DashboardFontMode::Auto),
        None => Ok(DashboardFontMode::Untouched),
    }
}

/// The open window the command acts on: the `--window` one, else the frontmost.
fn open_window(core: &Core, window: Option<&str>) -> Result<Uuid, ControlError> {
    Resolver::new(core.library())
        .placement_window(window)?
        .ok_or(ControlError::WindowNotOpen)
}

/// Opens the window's dashboard over the cells `what` resolves to, capped at [`MAX_CELLS`] panes, replacing its zoom;
/// the targets that resolved to nothing and the panes the cap dropped are named in `result.text`.
fn open(
    core: &mut Core,
    window: Option<&str>,
    what: &Open<'_>,
    font_mode: DashboardFontMode,
) -> Result<Option<ControlResult>, ControlError> {
    let window = open_window(core, window)?;
    if let Some(error) = core.modals.pending_modal_error(window) {
        return Err(ControlError::rejected(error));
    }
    let store = core.library().store(window).ok_or(ControlError::WindowNotOpen)?;
    let (resolved, unresolved) = match what {
        Open::Mru => {
            let recent = store.recent_sessions(MAX_CELLS);
            if recent.is_empty() {
                return Err(ControlError::rejected("no recent sessions"));
            }
            let targets = recent
                .into_iter()
                .map(|session| ResolvedDashboardTarget { session, pane: None })
                .collect();
            (targets, Vec::new())
        }
        Open::Targets(targets) => resolve_targets(store, targets),
    };
    let (members, dropped) = store.dashboard_members(&resolved, MAX_CELLS);
    // judged on the cells: a resolved `<id>:right` without a split contributes none
    if members.is_empty() {
        return Err(ControlError::rejected("no dashboard sessions resolved"));
    }
    let base = base_font_size(core);
    let applied = font_mode.applied_font_size(members.len(), base);
    let mut controller = DashboardController::default();
    controller.open(members, None, font_mode);
    controller.set_applied_font_size(applied);
    controller.track_splits(store);
    core.set_zoom_target(window, None);
    install(core, window, controller);
    let mut notes = Vec::new();
    if !unresolved.is_empty() {
        notes.push(format!("unresolved: {}", unresolved.join(", ")));
    }
    if dropped > 0 {
        notes.push(format!("dropped {dropped} pane(s) beyond the {MAX_CELLS}-cell limit"));
    }
    Ok((!notes.is_empty()).then(|| ControlResult::text(notes.join("; "))))
}

/// Resolves each raw target inside `store`: the ones naming no session, or a split pane the session does not have, are
/// returned as unresolved, verbatim.
fn resolve_targets(store: &AppStore, targets: &[String]) -> (Vec<ResolvedDashboardTarget>, Vec<String>) {
    let candidates = session_ids(store);
    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    for raw in targets {
        let target = DashboardTarget::parse(raw).and_then(|parsed| {
            let TargetResolution::Resolved(id) = resolve(&parsed.head, &candidates, store.selected_session_id()) else {
                return None;
            };
            let session = store.session(id)?;
            (parsed.pane != Some(ZoomSurface::Split) || session.split.exists).then_some(ResolvedDashboardTarget {
                session: id,
                pane: parsed.pane,
            })
        });
        match target {
            Some(target) => resolved.push(target),
            None => unresolved.push(raw.clone()),
        }
    }
    (resolved, unresolved)
}

/// Puts `controller` up as window `window`'s dashboard. Opening one over a closed grid ends the active session's
/// search (upstream's `handleDashboardOpenChange`: libghostty would stay in search mode with no bar to end it).
fn install(core: &mut Core, window: Uuid, controller: DashboardController) {
    let opening = core.dashboard(window).is_none();
    core.dashboards.by_window.insert(window, controller);
    if !opening {
        return;
    }
    // the grid is modal: an open quick terminal steps aside (upstream's dashboard open hides the panel)
    if core.quick_visible() {
        core.dismiss_quick_terminal();
    }
    let Some(store) = core.library_mut().store_mut(window) else {
        return;
    };
    let active = store.selected_session_id().and_then(|id| store.session_mut(id));
    if let Some(session) = active
        && let Some(owner) = session.search.owner
    {
        run_binding(session, owner, "end_search");
    }
}

/// Closes window `window`'s dashboard, if open (a terminal zoom replaces it).
pub(crate) fn close(core: &mut Core, window: Uuid) {
    core.dashboards.by_window.remove(&window);
}

/// The size an auto-sized grid shrinks from: the settings' font size, else libghostty's default.
fn base_font_size(core: &Core) -> f64 {
    core.settings().font_size.unwrap_or(GHOSTTY_DEFAULT_FONT_SIZE)
}

/// Fills the tree's `dashboard*` fields from window `window`'s open dashboard, as it stands against the model now.
pub(crate) fn project(core: &Core, window: Option<Uuid>, tree: &mut ControlTree) {
    let Some((window, store)) = window.and_then(|id| core.library().store(id).map(|store| (id, store))) else {
        return;
    };
    let Some(mut controller) = core.dashboards.by_window.get(&window).cloned() else {
        return;
    };
    controller.sync_with_store(store, base_font_size(core));
    if !controller.is_open() {
        return;
    }
    tree.dashboard_members = Some(controller.members().iter().map(|member| member.control_ref()).collect());
    tree.dashboard_highlighted = controller.highlighted().map(DashboardMember::control_ref);
    tree.dashboard_font_size = controller.applied_font_size();
    tree.dashboard_font_mode = Some(controller.font_mode().wire_name().to_owned());
}

/// The frontend's side of the dashboard: reading it, the keyboard and mouse actions on the grid, and the follow-up
/// after model changes. Commit afterwards, as after any mutation.
impl Core {
    /// Window `window`'s open dashboard.
    #[must_use]
    pub fn dashboard(&self, window: Uuid) -> Option<&DashboardController> {
        self.dashboards
            .by_window
            .get(&window)
            .filter(|controller| controller.is_open())
    }

    /// Brings every dashboard in line with its window's model (upstream's `reconcileDashboardMembers`, run whenever the
    /// valid cells change): a promoted split pane's cell follows it, cells whose pane went away drop out, an emptied
    /// dashboard closes, and so does a closed window's.
    pub fn reconcile_dashboards(&mut self) {
        let base = base_font_size(self);
        let mut dashboards = std::mem::take(&mut self.dashboards.by_window);
        dashboards.retain(|window, controller| {
            let Some(store) = self.library().store(*window) else {
                return false;
            };
            controller.sync_with_store(store, base);
            controller.is_open()
        });
        self.dashboards.by_window = dashboards;
    }

    /// Moves window `window`'s highlight one cell (the arrow keys).
    pub fn dashboard_move(&mut self, window: Uuid, direction: Direction) {
        if let Some(controller) = self.dashboards.by_window.get_mut(&window) {
            controller.move_highlight(direction);
        }
    }

    /// Highlights `member` (a click flashes the cell before entering it).
    pub fn dashboard_highlight(&mut self, window: Uuid, member: DashboardMember) {
        if let Some(controller) = self.dashboards.by_window.get_mut(&window) {
            controller.highlight(member);
        }
    }

    /// Enters `member` (Enter, or a click after its flash): selects its session, closes the dashboard and gives the
    /// cell's own pane the keyboard (upstream's `selectDashboardMember`).
    pub fn dashboard_enter(&mut self, window: Uuid, member: DashboardMember) {
        close(self, window);
        let Some(store) = self.library_mut().store_mut(window) else {
            return;
        };
        let Some(has_split_pane) = store
            .session(member.session)
            .map(|session| session.split_surface.is_present())
        else {
            let active = store.selected_session_id();
            if let Some(active) = active {
                self.request_pane_focus(active);
            }
            return;
        };
        store.select_session(Some(member.session));
        store.set_split_focus(member.session, member.surface == ZoomSurface::Split && has_split_pane);
        self.request_pane_focus(member.session);
    }

    /// Closes window `window`'s dashboard and gives the selected session back the keyboard (Esc, the exit button).
    pub fn dashboard_close(&mut self, window: Uuid) {
        close(self, window);
        let active = self.library().store(window).and_then(AppStore::selected_session_id);
        if let Some(active) = active {
            self.request_pane_focus(active);
        }
    }

    /// The GUI's dashboard toggle (upstream's `toggleDashboard`, `dashboard --mru --auto-size`): an open dashboard
    /// closes; a closed one opens over the window's most recently used sessions, auto-sized. Inert while the window is
    /// zoomed and with no recent session.
    pub fn toggle_dashboard(&mut self, window: Uuid) {
        if self.zoom_target(window).is_some() || self.modal_pending(window) {
            return;
        }
        if self.dashboard(window).is_some() {
            self.dashboard_close(window);
            return;
        }
        let Some(store) = self.library().store(window) else {
            return;
        };
        let members = store.dashboard_mru_members(MAX_CELLS);
        if members.is_empty() {
            return;
        }
        let base = base_font_size(self);
        let applied = DashboardFontMode::Auto.applied_font_size(members.len(), base);
        let mut controller = DashboardController::default();
        controller.open(members, None, DashboardFontMode::Auto);
        controller.set_applied_font_size(applied);
        controller.track_splits(store);
        install(self, window, controller);
    }
}
