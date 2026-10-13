//! One window's state (upstream's `AppStore.swift`, `AppStore+Naming.swift`, `AppStore+SessionState.swift`,
//! `AppStore+Sidebar.swift`, `AppStore+Events.swift`): the workspace tree, the selection, the per-window sidebar
//! state and the recency order.
//!
//! Selection is one session id: workspace rows are headers, so the current workspace is derived. Every structural
//! mutation asks for an immediate save and a selection or font change for a debounced one, through the store's
//! [`Outbox`]. The focus filter and visibility rules live in `focus.rs`, navigation and reselection in
//! `navigation.rs`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::SystemTime;

use agterm_proto::events::EventKind;
use uuid::Uuid;

use super::naming::{default_workspace_name, sanitized, trimmed_or_none};
use super::outbox::{EventPayload, ModelEvent, Outbox, SaveRequest, WatermarkRemoval};
use super::pending_close::PendingCloses;
use super::recency::RecencyStack;
use super::recent_closed::{ClosedSessionPlace, RecentClosedList};
use super::session::{AgentIndicator, AgentStatus, Session, SessionSeed};
use super::workspace::{ReorderDirection, SidebarMode, Workspace};
use crate::pane_env::uuid_string;

/// The sidebar width a window starts with, in points.
pub const SIDEBAR_WIDTH_DEFAULT: f64 = 220.0;
/// The narrowest sidebar a drag, `sidebar.width` or a restore leaves.
pub const SIDEBAR_WIDTH_MIN: f64 = 160.0;
/// The widest sidebar a drag, `sidebar.width` or a restore leaves.
pub const SIDEBAR_WIDTH_MAX: f64 = 560.0;

/// Clamps a finite width into the sidebar bounds (upstream's `clampSidebarWidth`); the dispatcher refuses a
/// non-finite one.
#[must_use]
pub fn clamp_sidebar_width(width: f64) -> f64 {
    width.clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX)
}

/// A window's sidebar: shown or not, which view, how wide, and which workspace rows the outline shows open.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarState {
    /// Persisted per window.
    pub visible: bool,
    /// Persisted per window.
    pub mode: SidebarMode,
    /// Persisted per window, within the sidebar width bounds.
    pub width: f64,
    /// The workspace rows the outline renders open, mirrored from it. View state, never persisted, and at odds with
    /// [`Workspace::is_expanded`] wherever a reveal opened a row collapsed on disk.
    pub expanded_workspace_ids: HashSet<Uuid>,
    /// Per-row opens and folds asked from outside the outline (`workspace.collapse`/`expand`), last one per workspace
    /// winning, for the outline to apply on its next refresh (upstream's `.agtermSetWorkspaceExpanded` notification).
    /// View state, never persisted.
    pub(crate) expansion_requests: HashMap<Uuid, bool>,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            visible: true,
            mode: SidebarMode::Tree,
            width: SIDEBAR_WIDTH_DEFAULT,
            expanded_workspace_ids: HashSet::new(),
            expansion_requests: HashMap::new(),
        }
    }
}

/// The sidebar focus filter: the marked workspaces the tree renders while it is enabled. Enabled with an empty set
/// is unrepresentable: every mutator keeps it out of reach. Persisted per window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FocusFilter {
    pub(crate) ids: BTreeSet<Uuid>,
    pub(crate) enabled: bool,
}

impl FocusFilter {
    /// The marked workspaces.
    #[must_use]
    pub fn ids(&self) -> &BTreeSet<Uuid> {
        &self.ids
    }

    /// Whether the filter applies.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

/// How a new workspace enters the window (upstream's `addWorkspace` flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceOptions {
    /// Starts collapsed, against the default of expanded (`workspace.new --collapsed`).
    pub collapsed: bool,
    /// A foreground create: joins an enabled focus filter so it is visible, and becomes the current workspace until
    /// the selection moves. A background create leaves both alone, so a script cannot steer the GUI's next add.
    pub reveal: bool,
}

impl Default for WorkspaceOptions {
    fn default() -> Self {
        Self {
            collapsed: false,
            reveal: true,
        }
    }
}

/// A session to create (upstream's `addSession` arguments).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub cwd: String,
    /// What the primary pane runs instead of the login shell.
    pub command: Option<String>,
    /// The custom name; blank means the automatic basename.
    pub name: Option<String>,
    /// Hold the pane after the command exits.
    pub wait: bool,
    /// Insert at this clamped index; append when `None`.
    pub at: Option<usize>,
    /// Select the new session; `false` creates it in the background, leaving selection, focus and recency alone.
    pub select: bool,
    /// The host of a session attached from another machine.
    pub remote_host: Option<String>,
}

impl NewSession {
    /// A selected login-shell session at `cwd`, appended.
    #[must_use]
    pub fn at_cwd(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            command: None,
            name: None,
            wait: false,
            at: None,
            select: true,
            remote_host: None,
        }
    }
}

/// Where a session sits: its workspace's index in the window and its own index in that workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Location {
    pub(crate) workspace: usize,
    pub(crate) session: usize,
}

/// A session's place for the sidebar drop resolver: the owning workspace, the index in it, and its session count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLocation {
    pub workspace: Uuid,
    pub index: usize,
    pub count: usize,
}

/// One window's workspace tree, selection and sidebar state.
#[derive(Debug, Default)]
pub struct AppStore {
    pub(crate) workspaces: Vec<Workspace>,
    pub(crate) selected_session_id: Option<Uuid>,
    /// True while a reload runs: a reload is not a selection, so it emits no `session.selected`.
    pub(crate) restoring_selection: bool,
    /// The transient sidebar multi-selection; the selected session stays the active target.
    pub(crate) sidebar_selection_raw: Vec<Uuid>,
    pub(crate) sidebar: SidebarState,
    pub(crate) focus: FocusFilter,
    pub(crate) session_recency: RecencyStack<Uuid>,
    /// The workspace that holds the target without owning the selection: a foreground create, or selecting an empty
    /// workspace. Dropped by a selection change, by selecting a workspace that has a session, by removing it, by the
    /// focus filter hiding it, and by a restore.
    pub(crate) fresh_workspace_id: Option<Uuid>,
    pub(crate) outbox: Outbox,
    /// The undoable closes: their sessions, out of the tree, still own their terminals.
    pub(crate) pending: PendingCloses,
    /// Open Recent, shared with the library and its other stores once the store is open in one.
    pub(crate) recent_closed: RecentClosedList,
}

impl AppStore {
    /// An empty window: no workspaces, no selection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // MARK: - Reads

    #[must_use]
    pub fn workspaces(&self) -> &[Workspace] {
        &self.workspaces
    }

    #[must_use]
    pub fn selected_session_id(&self) -> Option<Uuid> {
        self.selected_session_id
    }

    /// The selected session.
    #[must_use]
    pub fn active_session(&self) -> Option<&Session> {
        self.selected_session_id.and_then(|id| self.session(id))
    }

    #[must_use]
    pub fn sidebar(&self) -> &SidebarState {
        &self.sidebar
    }

    #[must_use]
    pub fn focus(&self) -> &FocusFilter {
        &self.focus
    }

    /// The most-recently-selected session ids, current first; persisted, so the order survives a relaunch.
    #[must_use]
    pub fn session_recency(&self) -> &RecencyStack<Uuid> {
        &self.session_recency
    }

    /// Takes the side effects recorded since the last call.
    pub fn take_outbox(&mut self) -> Outbox {
        std::mem::take(&mut self.outbox)
    }

    /// The pending side effects, for inspection.
    #[must_use]
    pub fn outbox(&self) -> &Outbox {
        &self.outbox
    }

    #[must_use]
    pub fn workspace(&self, id: Uuid) -> Option<&Workspace> {
        self.workspaces.iter().find(|workspace| workspace.id() == id)
    }

    pub(crate) fn workspace_index(&self, id: Uuid) -> Option<usize> {
        self.workspaces.iter().position(|workspace| workspace.id() == id)
    }

    /// The session with `id` across all workspaces.
    #[must_use]
    pub fn session(&self, id: Uuid) -> Option<&Session> {
        self.workspaces.iter().find_map(|workspace| workspace.session(id))
    }

    /// The session with `id`, for the per-session fields with no rules of their own (live cwd and title reports,
    /// surface slots). Structural changes go through the store's methods.
    pub fn session_mut(&mut self, id: Uuid) -> Option<&mut Session> {
        self.workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.sessions.iter_mut())
            .find(|session| session.id() == id)
    }

    /// Every session in sidebar tree order.
    pub fn sessions(&self) -> impl Iterator<Item = &Session> {
        self.workspaces.iter().flat_map(|workspace| workspace.sessions.iter())
    }

    /// The workspace owning `session`.
    #[must_use]
    pub fn workspace_for_session(&self, session: Uuid) -> Option<&Workspace> {
        self.location(session).map(|at| &self.workspaces[at.workspace])
    }

    pub(crate) fn location(&self, session: Uuid) -> Option<Location> {
        self.workspaces.iter().enumerate().find_map(|(workspace, entry)| {
            entry
                .sessions
                .iter()
                .position(|candidate| candidate.id() == session)
                .map(|session| Location { workspace, session })
        })
    }

    /// The owning workspace, the session's index in it and its session count; one walk for the sidebar drop
    /// resolver.
    #[must_use]
    pub fn session_location(&self, session: Uuid) -> Option<SessionLocation> {
        let at = self.location(session)?;
        let workspace = &self.workspaces[at.workspace];
        Some(SessionLocation {
            workspace: workspace.id(),
            index: at.session,
            count: workspace.sessions.len(),
        })
    }

    /// The workspace a new session lands in: a freshly created one, else the selected session's, else the last.
    /// Resolves `active` for control-channel workspace targets.
    #[must_use]
    pub fn current_workspace_id(&self) -> Option<Uuid> {
        if let Some(fresh) = self.fresh_workspace_id
            && self.workspace(fresh).is_some()
        {
            return Some(fresh);
        }
        if let Some(workspace) = self.selected_session_id.and_then(|id| self.workspace_for_session(id)) {
            return Some(workspace.id());
        }
        self.workspaces.last().map(Workspace::id)
    }

    /// The auto name of the next new workspace.
    #[must_use]
    pub fn default_workspace_name(&self) -> String {
        default_workspace_name(self.workspaces.len())
    }

    /// The first workspace whose name equals `name`, case-sensitively, after the same sanitize and trim stored names
    /// get; `None` when none matches or `name` is blank (`session.new --workspace-name`).
    #[must_use]
    pub fn workspace_named(&self, name: &str) -> Option<&Workspace> {
        let needle = trimmed_or_none(&sanitized(name))?;
        self.workspaces.iter().find(|workspace| workspace.name == needle)
    }

    /// A workspace's name; `None` when it is gone or blank (a rebuilt workspace may carry a blank one).
    #[must_use]
    pub fn workspace_name(&self, id: Uuid) -> Option<String> {
        self.workspace(id)
            .and_then(|workspace| trimmed_or_none(&workspace.name))
    }

    /// Whether a workspace may be removed: one is always kept.
    #[must_use]
    pub fn can_remove_workspace(&self) -> bool {
        self.workspaces.len() > 1
    }

    /// The flagged sessions across all workspaces, in tree order: what the flagged view renders.
    #[must_use]
    pub fn flagged_sessions(&self) -> Vec<&Session> {
        self.sessions().filter(|session| session.flagged).collect()
    }

    // MARK: - Effects

    pub(crate) fn save(&mut self) {
        self.outbox.request_save(SaveRequest::Now);
    }

    pub(crate) fn schedule_save(&mut self) {
        self.outbox.request_save(SaveRequest::Debounced);
    }

    pub(crate) fn request_save(&mut self, request: SaveRequest) {
        self.outbox.request_save(request);
    }

    /// Every session, for the restore paths' per-session repairs.
    pub(crate) fn sessions_mut(&mut self) -> impl Iterator<Item = &mut Session> {
        self.workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.sessions.iter_mut())
    }

    /// Emits `session.created` for every session, as a window opened mid-run announces the sessions it brings back.
    pub(crate) fn announce_sessions(&mut self) {
        let created: Vec<(Uuid, Uuid)> = self
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace
                    .sessions
                    .iter()
                    .map(move |session| (session.id(), workspace.id()))
            })
            .collect();
        for (session, workspace) in created {
            self.emit_session_created(session, workspace);
        }
        self.schedule_tree_changed();
    }

    pub(crate) fn emit(
        &mut self,
        kind: EventKind,
        workspace: Option<Uuid>,
        session: Option<Uuid>,
        payload: EventPayload,
    ) {
        self.outbox.emit(ModelEvent {
            kind,
            workspace,
            session,
            payload,
        });
    }

    pub(crate) fn schedule_tree_changed(&mut self) {
        self.outbox.emit(ModelEvent::bare(EventKind::TreeChanged));
    }

    fn named(session: &Session) -> EventPayload {
        EventPayload {
            name: Some(session.display_name()),
            ..EventPayload::default()
        }
    }

    pub(crate) fn emit_session_created(&mut self, session: Uuid, workspace: Uuid) {
        let payload = self.session(session).map(Self::named).unwrap_or_default();
        self.emit(EventKind::SessionCreated, Some(workspace), Some(session), payload);
        self.schedule_tree_changed();
    }

    pub(crate) fn emit_session_closed(&mut self, session: &Session, workspace: Uuid) {
        self.emit(
            EventKind::SessionClosed,
            Some(workspace),
            Some(session.id()),
            Self::named(session),
        );
        self.schedule_tree_changed();
    }

    /// Makes every pending close final, emits `session.closed` for every session, tears their terminals down and drops
    /// the pending write: the store is leaving the open set and its outbox is all that remains of it. A deleted window's write must not re-create
    /// its removed file (upstream's `removeWindow` cancels the pending save); a closing window's caller has already
    /// written its snapshot (see [`WindowLibrary::close_window`](super::WindowLibrary::close_window)).
    pub(crate) fn retire(&mut self) {
        // the undo window dies with the store: a session left pending would keep its terminals with nothing left to
        // finalize it
        self.finalize_all_pending_closes();
        let closing: Vec<(Uuid, Uuid, EventPayload)> = self
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace
                    .sessions
                    .iter()
                    .map(move |session| (workspace.id(), session.id(), Self::named(session)))
            })
            .collect();
        for (workspace, session, payload) in closing {
            self.emit(EventKind::SessionClosed, Some(workspace), Some(session), payload);
            self.schedule_tree_changed();
        }
        for session in self
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.sessions.iter_mut())
        {
            session.teardown_surfaces();
        }
        self.schedule_tree_changed();
        self.outbox.save = SaveRequest::None;
    }

    /// `session.selected` fires on every change of this window's selection, whoever made it, a close reselecting
    /// included; a cleared selection carries no session.
    fn emit_session_selected(&mut self, previous: Option<Uuid>) {
        if self.restoring_selection {
            return;
        }
        let selected = self
            .active_session()
            .map(|session| (session.id(), session.display_name()));
        let workspace = selected
            .as_ref()
            .and_then(|(id, _)| self.workspace_for_session(*id))
            .map(Workspace::id);
        let (session, name) = selected.unzip();
        let payload = EventPayload {
            name,
            previous: previous.map(uuid_string),
            ..EventPayload::default()
        };
        self.emit(EventKind::SessionSelected, workspace, session, payload);
    }

    // MARK: - Selection

    /// The one write point of the selection: a change drops the fresh-workspace preference and emits
    /// `session.selected`; a same-value write does neither, since reselecting the active session moves nobody.
    pub(crate) fn set_selected(&mut self, id: Option<Uuid>) {
        let previous = self.selected_session_id;
        if previous == id {
            return;
        }
        self.selected_session_id = id;
        self.fresh_workspace_id = None;
        self.emit_session_selected(previous);
    }

    /// Selects a session (`None` clears) and schedules a save. An id naming no session is ignored. Clears the
    /// visited session's unseen badge, resets an `auto_reset` indicator on both the destination and the session
    /// left, and pushes the recency order. Returns the destination's indicator from before the reset, so the
    /// frontend can reveal the pane it tags.
    pub fn select_session(&mut self, id: Option<Uuid>) -> Option<AgentIndicator> {
        self.select_session_with(id, None)
    }

    /// [`AppStore::select_session`] with the sidebar's multi-selection (a shift or command click).
    pub fn select_session_in_sidebar(&mut self, id: Option<Uuid>, selection: Vec<Uuid>) -> Option<AgentIndicator> {
        self.select_session_with(id, Some(selection))
    }

    fn select_session_with(&mut self, id: Option<Uuid>, selection: Option<Vec<Uuid>>) -> Option<AgentIndicator> {
        if let Some(id) = id
            && self.session(id).is_none()
        {
            return None;
        }
        let destination = id
            .and_then(|id| self.session(id))
            .map(|session| session.agent_indicator.clone());
        let previous = self.selected_session_id;
        self.set_selected(id);
        match selection {
            Some(ids) => self.set_sidebar_selection(ids),
            None => self.replace_sidebar_selection(id),
        }
        self.disable_focus_if_selection_outside_set(id);
        if let Some(id) = id {
            self.clear_unseen(id);
        }
        self.clear_auto_reset_indicator(id);
        self.clear_auto_reset_indicator(previous);
        self.record_recency();
        self.schedule_save();
        destination
    }

    fn clear_auto_reset_indicator(&mut self, id: Option<Uuid>) {
        let Some(id) = id else { return };
        if self
            .session(id)
            .is_some_and(|session| session.agent_indicator.auto_reset)
        {
            self.set_agent_indicator(AgentIndicator::default(), id);
        }
    }

    /// Clears a session's unseen badge; ephemeral, so no save.
    pub fn clear_unseen(&mut self, id: Uuid) {
        if let Some(session) = self.session_mut(id) {
            session.unseen_count = 0;
        }
    }

    /// Records a delivered notification on a session (upstream's `recordNotificationEvent` plus the manager's badge
    /// bump): the `notify` event with the effective title (the caller's, else the session's name) and the unseen
    /// count raised. The effective title; `None` for a session not in the tree, which records nothing.
    pub fn record_notification(&mut self, id: Uuid, title: &str, body: &str) -> Option<String> {
        let workspace = self.workspace_for_session(id)?.id();
        let session = self.session_mut(id)?;
        let name = session.display_name();
        let effective = if title.is_empty() {
            name.clone()
        } else {
            title.to_owned()
        };
        session.unseen_count += 1;
        let payload = EventPayload {
            name: Some(name),
            title: Some(effective.clone()),
            body: Some(body.to_owned()),
            ..EventPayload::default()
        };
        self.emit(EventKind::Notify, Some(workspace), Some(id), payload);
        Some(effective)
    }

    pub(crate) fn record_recency(&mut self) {
        if let Some(id) = self.selected_session_id {
            self.session_recency.push(id);
        }
    }

    /// Sets a session's status glyph and stamps the change time, repeats and idle included; `status` fires only when
    /// the indicator changed, carrying the glyph's overrides and the status it replaced (upstream's
    /// `setAgentIndicator`). A transition into blocked is a new episode, which idle auto-follow may pull to again.
    /// Control writes go through [`AppStore::apply_control_status`], which may refuse one.
    pub fn set_agent_indicator(&mut self, mut indicator: AgentIndicator, id: Uuid) {
        let workspace = self.workspace_for_session(id).map(Workspace::id);
        let Some(session) = self.session_mut(id) else { return };
        // gated on the split EXISTING, not on its surface: a scripted split followed at once by a `--pane right`
        // status lands before the frontend realized the split surface, where `right` is the correct tag
        indicator.status_pane = indicator.normalized_pane(session.split.exists);
        let previous = std::mem::replace(&mut session.agent_indicator, indicator);
        session.status_changed_at = Some(SystemTime::now());
        if previous.status != AgentStatus::Blocked && session.agent_indicator.status == AgentStatus::Blocked {
            session.set_auto_follow_consumed(false);
        }
        let current = &session.agent_indicator;
        if previous == *current {
            return;
        }
        let payload = EventPayload {
            name: Some(session.display_name()),
            status: Some(current.status.wire_name().to_owned()),
            pane: current.status_pane.map(|pane| pane.as_str().to_owned()),
            blink: Some(current.blink),
            color: current.color.clone(),
            shape: current.shape.map(|shape| shape.wire_name().to_owned()),
            previous: Some(previous.status.wire_name().to_owned()),
            ..EventPayload::default()
        };
        self.emit(EventKind::Status, workspace, Some(id), payload);
    }

    /// Every non-idle session in attention order: blocked, active, completed, newest change first within each.
    #[must_use]
    pub fn attention_sessions(&self) -> Vec<&Session> {
        let mut sessions: Vec<&Session> = self
            .sessions()
            .filter(|session| session.agent_indicator.status != AgentStatus::Idle)
            .collect();
        sessions.sort_by(|a, b| attention_order(a, b));
        sessions
    }

    // MARK: - Workspaces

    /// Appends a revealed, expanded workspace; see [`AppStore::add_workspace_with`].
    pub fn add_workspace(&mut self, name: &str) -> Uuid {
        self.add_workspace_with(name, WorkspaceOptions::default())
    }

    /// Creates a workspace and appends it. A revealed create joins an enabled focus filter, widening rather than
    /// clearing it, and becomes the current workspace until the selection moves.
    pub fn add_workspace_with(&mut self, name: &str, options: WorkspaceOptions) -> Uuid {
        let mut workspace = Workspace::new(sanitized(name));
        workspace.is_expanded = !options.collapsed;
        let id = workspace.id();
        self.workspaces.push(workspace);
        if options.reveal {
            self.reveal_new_focus_member(id);
            self.fresh_workspace_id = Some(id);
        }
        self.schedule_tree_changed();
        self.save();
        id
    }

    /// The workspace named `name`, created (revealed per `reveal`) when none exists; `None` only for a blank name
    /// (`--workspace-name --create-workspace`).
    pub fn ensure_workspace(&mut self, name: &str, reveal: bool) -> Option<Uuid> {
        let needle = trimmed_or_none(&sanitized(name))?;
        if let Some(existing) = self.workspace_named(&needle) {
            return Some(existing.id());
        }
        Some(self.add_workspace_with(
            &needle,
            WorkspaceOptions {
                collapsed: false,
                reveal,
            },
        ))
    }

    /// Renames a workspace; a blank or unchanged name is a no-op.
    pub fn rename_workspace(&mut self, id: Uuid, name: &str) {
        let Some(name) = trimmed_or_none(&sanitized(name)) else {
            return;
        };
        let Some(index) = self.workspace_index(id) else { return };
        if self.workspaces[index].name == name {
            return;
        }
        self.workspaces[index].name = name;
        self.schedule_tree_changed();
        self.save();
    }

    /// Removes a workspace and its sessions, tearing their terminals down, records it in Open Recent and returns it.
    /// A no-op unless another workspace remains. When it held the selection, the most recent visible session takes
    /// it.
    pub fn remove_workspace(&mut self, id: Uuid) -> Option<Workspace> {
        if !self.can_remove_workspace() {
            return None;
        }
        let index = self.workspace_index(id)?;
        let selected = self
            .selected_session_id
            .filter(|selected| self.workspaces[index].session(*selected).is_some());
        let removing_active = selected.is_some();
        let mut workspace = self.workspaces.remove(index);
        // before the membership is dropped below, so a reopen can mark it again
        let focus_member = self.focus.ids.contains(&id);
        self.record_recent_closed_workspace(&workspace, selected, focus_member, Uuid::new_v4());
        for session in &workspace.sessions {
            self.emit_session_closed(session, id);
        }
        if workspace.sessions.is_empty() {
            self.schedule_tree_changed();
        }
        for session in &mut workspace.sessions {
            session.teardown_surfaces();
            // the session is gone, and its rendered text with it
            self.outbox
                .removed_watermarks
                .push(WatermarkRemoval::Session(session.id()));
            self.session_recency.remove(session.id());
        }
        self.drop_focus_member(id);
        self.forget_fresh_workspace(id);
        if removing_active {
            let target = self.workspace_removal_target(index);
            self.reselect_after_removal(target);
        } else {
            self.prune_sidebar_selection();
        }
        self.save();
        Some(workspace)
    }

    /// Drops the fresh-workspace preference when that workspace goes away, so an undo re-inserting the same id
    /// cannot revive it.
    pub(crate) fn forget_fresh_workspace(&mut self, id: Uuid) {
        if self.fresh_workspace_id == Some(id) {
            self.fresh_workspace_id = None;
        }
    }

    /// Makes `id` the current workspace and selects its first session when it has one; an empty workspace becomes
    /// the target itself, revealed through the focus filter. `None` only for an unknown id.
    pub fn select_workspace(&mut self, id: Uuid) -> Option<WorkspaceStep> {
        let workspace = self.workspace(id)?;
        if let Some(first) = workspace.sessions.first().map(Session::id) {
            let indicator = self.select_session(Some(first));
            self.fresh_workspace_id = None;
            return Some(WorkspaceStep {
                workspace: id,
                indicator,
            });
        }
        let marked = self.focus.ids.clone();
        self.reveal_new_focus_member(id);
        self.fresh_workspace_id = Some(id);
        if self.focus.ids != marked {
            self.save();
        }
        Some(WorkspaceStep {
            workspace: id,
            indicator: None,
        })
    }

    /// Moves a workspace to `index` among its siblings, the destination after the move's removal, clamped.
    pub fn move_workspace(&mut self, id: Uuid, index: usize) {
        let Some(current) = self.workspace_index(id) else {
            return;
        };
        let before: Vec<Uuid> = self.workspaces.iter().map(Workspace::id).collect();
        let workspace = self.workspaces.remove(current);
        let destination = index.min(self.workspaces.len());
        self.workspaces.insert(destination, workspace);
        if self.workspaces.iter().map(Workspace::id).ne(before) {
            self.schedule_tree_changed();
        }
        self.save();
    }

    /// Moves a workspace one step among its siblings; a no-op (no write) when already at that end.
    pub fn reorder_workspace(&mut self, id: Uuid, direction: ReorderDirection) {
        let Some(current) = self.workspace_index(id) else {
            return;
        };
        if let Some(destination) = direction.destination_index(current, self.workspaces.len()) {
            self.move_workspace(id, destination);
        }
    }

    /// Sets one workspace's persisted expansion: a genuine per-row user toggle, never a programmatic reveal.
    pub fn set_workspace_expanded(&mut self, id: Uuid, expanded: bool) {
        let Some(index) = self.workspace_index(id) else { return };
        if self.workspaces[index].is_expanded == expanded {
            return;
        }
        self.workspaces[index].is_expanded = expanded;
        self.save();
    }

    /// Marks each workspace expanded iff `expanded` holds its id, in one write (Expand / Collapse Workspaces).
    pub fn set_workspaces_expanded(&mut self, expanded: &HashSet<Uuid>) {
        let mut changed = false;
        for workspace in &mut self.workspaces {
            let want = expanded.contains(&workspace.id());
            if workspace.is_expanded != want {
                workspace.is_expanded = want;
                changed = true;
            }
        }
        if changed {
            self.save();
        }
    }

    /// Sets a workspace's persisted expansion and asks the outline to open or fold its row to match, even where a
    /// reveal opened a row the persisted flag already calls collapsed (upstream's `AppActions.setWorkspaceExpanded`:
    /// the store write plus the notification the sidebar coordinator applies).
    pub fn set_workspace_expanded_in_outline(&mut self, id: Uuid, expanded: bool) {
        if self.workspace(id).is_none() {
            return;
        }
        self.set_workspace_expanded(id, expanded);
        self.sidebar.expansion_requests.insert(id, expanded);
    }

    /// Takes the outline's pending per-row open and fold requests (see
    /// [`AppStore::set_workspace_expanded_in_outline`]); called by the sidebar alone.
    pub fn take_sidebar_expansion_requests(&mut self) -> HashMap<Uuid, bool> {
        std::mem::take(&mut self.sidebar.expansion_requests)
    }

    /// Records what the sidebar outline shows expanded; called by the sidebar alone.
    pub fn note_sidebar_expansion(&mut self, ids: HashSet<Uuid>) {
        self.sidebar.expanded_workspace_ids = ids;
    }

    /// Whether the current workspace is folded ON SCREEN: what Collapse/Expand Workspace acts on. With the sidebar
    /// hidden there are no rows, so the persisted flag answers.
    #[must_use]
    pub fn is_current_workspace_collapsed(&self) -> bool {
        let Some(id) = self.current_workspace_id() else {
            return false;
        };
        if !self.sidebar.visible {
            return self.workspace(id).is_some_and(|workspace| !workspace.is_expanded);
        }
        !self.sidebar.expanded_workspace_ids.contains(&id)
    }

    // MARK: - Sessions

    /// Creates a session in `workspace`, selecting it unless `spec.select` is off. Name and cwd are sanitized; a
    /// blank name means the automatic basename. `None` when no workspace matches.
    pub fn add_session(&mut self, workspace: Uuid, spec: NewSession) -> Option<Uuid> {
        let index = self.workspace_index(workspace)?;
        let mut session = Session::new(SessionSeed {
            initial_cwd: sanitized(&spec.cwd),
            custom_name: spec.name.as_deref().map(sanitized).as_deref().and_then(trimmed_or_none),
            remote_host: spec.remote_host,
            ..SessionSeed::default()
        });
        session.launch.command = spec.command;
        session.launch.wait = spec.wait;
        let id = session.id();
        let sessions = &mut self.workspaces[index].sessions;
        match spec.at {
            Some(at) => sessions.insert(at.min(sessions.len()), session),
            None => sessions.push(session),
        }
        // ahead of the selection, so a consumer hears of the session before it hears it was selected
        self.emit_session_created(id, workspace);
        if spec.select {
            self.set_selected(Some(id));
            self.disable_focus_if_selection_outside_set(Some(id));
            self.record_recency();
        }
        self.save();
        Some(id)
    }

    /// Removes a session, tears its terminals down, records it in Open Recent and returns it. When it was selected,
    /// the most recent visible session of its scope takes the selection (`close_reselection_target`).
    pub fn close_session(&mut self, id: Uuid) -> Option<Session> {
        let at = self.location(id)?;
        let was_active = self.selected_session_id == Some(id);
        let workspace = self.workspaces[at.workspace].id();
        let workspace_name = self.workspaces[at.workspace].name.clone();
        let mut removed = self.workspaces[at.workspace].sessions.remove(at.session);
        self.emit_session_closed(&removed, workspace);
        let place = ClosedSessionPlace {
            workspace_id: workspace,
            workspace_name: &workspace_name,
            workspace_index: at.workspace,
            session_index: at.session,
        };
        self.record_recent_closed_session(&removed, &place, Uuid::new_v4());
        removed.teardown_surfaces();
        // the session is gone, and its rendered text with it
        self.outbox.removed_watermarks.push(WatermarkRemoval::Session(id));
        self.session_recency.remove(id);
        if was_active {
            let target = self.close_reselection_target(at);
            self.reselect_after_removal(target);
        } else {
            self.prune_sidebar_selection();
        }
        self.save();
        Some(removed)
    }

    /// Selects `target` after the active session left: a direct reselect, with the sidebar selection following and
    /// the focus filter suspended when the pick lies outside it.
    pub(crate) fn reselect_after_removal(&mut self, target: Option<Uuid>) {
        self.set_selected(target);
        self.replace_sidebar_selection(target);
        self.disable_focus_if_selection_outside_set(target);
        self.record_recency();
    }

    /// Sets a session's custom name; a blank one restores the automatic name.
    pub fn rename_session(&mut self, id: Uuid, name: &str) {
        let renamed = trimmed_or_none(&sanitized(name));
        let Some(session) = self.session_mut(id) else { return };
        if session.custom_name == renamed {
            return;
        }
        session.custom_name = renamed;
        self.schedule_tree_changed();
        self.save();
    }

    /// Moves a session to another workspace or within one, keeping the session itself (and so its live terminals).
    /// `index` is the destination after the move's removal, clamped; `None` appends. Moving the selected session out
    /// of the marked set suspends the focus filter, keeping the set.
    pub fn move_session(&mut self, id: Uuid, target: Uuid, index: Option<usize>) {
        let Some(source) = self.location(id) else { return };
        let Some(target_index) = self.workspace_index(target) else {
            return;
        };
        let before = self.tree_order();
        let session = self.workspaces[source.workspace].sessions.remove(source.session);
        let sessions = &mut self.workspaces[target_index].sessions;
        let destination = index.unwrap_or(sessions.len()).min(sessions.len());
        sessions.insert(destination, session);
        if self.selected_session_id == Some(id) {
            self.disable_focus_if_selection_outside_set(Some(id));
        }
        self.prune_sidebar_selection();
        if self.tree_order() != before {
            self.schedule_tree_changed();
        }
        self.save();
    }

    /// Moves several sessions in their tree order and returns how many moved. Without `index`, a multi-session move
    /// appends the sessions from other workspaces and leaves those already in the target alone, while a single one
    /// appends as [`AppStore::move_session`] does. With `index` (a drop), the whole block is removed first and then
    /// inserted at that post-removal index.
    pub fn move_sessions(&mut self, ids: &[Uuid], target: Uuid, index: Option<usize>) -> usize {
        if self.workspace(target).is_none() {
            return 0;
        }
        let before = self.tree_order();
        let wanted: HashSet<Uuid> = ids.iter().copied().collect();
        let mut moving_ids: Vec<Uuid> = self
            .sessions()
            .map(Session::id)
            .filter(|id| wanted.contains(id))
            .collect();
        if index.is_none() && moving_ids.len() > 1 {
            moving_ids.retain(|id| self.workspace_for_session(*id).map(Workspace::id) != Some(target));
        }
        if moving_ids.is_empty() {
            return 0;
        }
        let mut moving = Vec::with_capacity(moving_ids.len());
        for id in &moving_ids {
            if let Some(at) = self.location(*id) {
                moving.push(self.workspaces[at.workspace].sessions.remove(at.session));
            }
        }
        let Some(target_index) = self.workspace_index(target) else {
            self.prune_sidebar_selection();
            return 0;
        };
        let count = moving.len();
        let sessions = &mut self.workspaces[target_index].sessions;
        let destination = index.unwrap_or(sessions.len()).min(sessions.len());
        sessions.splice(destination..destination, moving);
        if let Some(selected) = self.selected_session_id
            && moving_ids.contains(&selected)
        {
            self.disable_focus_if_selection_outside_set(Some(selected));
        }
        self.prune_sidebar_selection();
        if self.tree_order() != before {
            self.schedule_tree_changed();
        }
        self.save();
        count
    }

    /// Moves a session one step within its workspace; a no-op (no write) when already at that end.
    pub fn reorder_session(&mut self, id: Uuid, direction: ReorderDirection) {
        let Some(at) = self.location(id) else { return };
        let workspace = &self.workspaces[at.workspace];
        if let Some(destination) = direction.destination_index(at.session, workspace.sessions.len()) {
            let target = workspace.id();
            self.move_session(id, target, Some(destination));
        }
    }

    fn tree_order(&self) -> Vec<Vec<Uuid>> {
        self.workspaces
            .iter()
            .map(|workspace| workspace.sessions.iter().map(Session::id).collect())
            .collect()
    }

    // MARK: - Per-session state

    /// Records a session's font size with a debounced write, so a held zoom key writes once; unchanged is a no-op.
    pub fn set_font_size(&mut self, id: Uuid, size: f64) {
        let Some(session) = self.session_mut(id) else { return };
        if session.font_size == Some(size) {
            return;
        }
        session.font_size = Some(size);
        self.schedule_save();
    }

    /// Clears every per-session font size (an appearance change resets live surfaces to the default).
    pub fn reset_session_font_sizes(&mut self) {
        let mut changed = false;
        for session in self
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.sessions.iter_mut())
        {
            changed |= session.font_size.take().is_some();
        }
        if changed {
            self.save();
        }
    }

    /// Sets or clears a session's flag. Unflagging in flagged mode hides the row, so the selection may move.
    pub fn set_flag(&mut self, on: bool, id: Uuid) {
        let Some(session) = self.session_mut(id) else { return };
        if session.flagged == on {
            return;
        }
        session.flagged = on;
        self.prune_sidebar_selection();
        self.reselect_if_selection_hidden();
        self.save();
    }

    /// Sets or clears several sessions' flags in one write; unknown ids are ignored.
    pub fn set_flags(&mut self, on: bool, ids: &[Uuid]) {
        let mut changed = false;
        for session in self
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.sessions.iter_mut())
        {
            if ids.contains(&session.id()) && session.flagged != on {
                session.flagged = on;
                changed = true;
            }
        }
        if changed {
            self.prune_sidebar_selection();
            self.reselect_if_selection_hidden();
            self.save();
        }
    }

    /// Unflags every session in one write. Clearing every flag leaves the flagged view empty, so there is nowhere to
    /// move the selection.
    pub fn clear_flags(&mut self) {
        let mut changed = false;
        for session in self
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.sessions.iter_mut())
        {
            changed |= std::mem::replace(&mut session.flagged, false);
        }
        if changed {
            self.prune_sidebar_selection();
            self.save();
        }
    }

    /// Sets or clears a session's title-bar context, already trimmed and validated; returns whether it changed.
    pub fn set_context(&mut self, context: Option<String>, id: Uuid) -> bool {
        let Some(session) = self.session_mut(id) else {
            return false;
        };
        if session.context == context {
            return false;
        }
        let previous = session.effective_context().map(str::to_owned);
        session.context = context;
        let effective_changed = previous.as_deref() != session.effective_context();
        self.save();
        if effective_changed {
            self.schedule_tree_changed();
        }
        true
    }

    // MARK: - Sidebar

    /// Shows or hides this window's sidebar; unchanged is a no-op.
    pub fn set_sidebar_visible(&mut self, visible: bool) {
        if self.sidebar.visible == visible {
            return;
        }
        self.sidebar.visible = visible;
        self.save();
    }

    pub fn toggle_sidebar_visible(&mut self) {
        self.set_sidebar_visible(!self.sidebar.visible);
    }

    /// Switches the sidebar view; either flip reselects when it would hide the active session.
    pub fn set_sidebar_mode(&mut self, mode: SidebarMode) {
        if self.sidebar.mode == mode {
            return;
        }
        self.sidebar.mode = mode;
        self.prune_sidebar_selection();
        self.reselect_if_selection_hidden();
        self.save();
    }

    /// Sets the sidebar width, clamped; unchanged after the clamp is a no-op.
    pub fn set_sidebar_width(&mut self, width: f64) {
        let clamped = clamp_sidebar_width(width);
        if (self.sidebar.width - clamped).abs() < f64::EPSILON {
            return;
        }
        self.sidebar.width = clamped;
        self.save();
    }
}

/// What one workspace step landed on: the workspace, and its selected session's indicator from before an
/// `auto_reset` cleared it (`None` for an empty workspace).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceStep {
    pub workspace: Uuid,
    pub indicator: Option<AgentIndicator>,
}

/// The attention order every attention list shares: rank ascending (blocked, active, completed), then the change
/// time descending, an unset one last within its rank (upstream's `attentionPrecedes`).
#[must_use]
pub fn attention_order(a: &Session, b: &Session) -> std::cmp::Ordering {
    let rank = |session: &Session| session.agent_indicator.status.attention_rank();
    rank(a)
        .cmp(&rank(b))
        .then_with(|| b.status_changed_at.cmp(&a.status_changed_at))
}
