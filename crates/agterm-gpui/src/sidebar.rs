//! The sidebar of one window (upstream's `WorkspaceSidebar.swift`, its outline coordinator, and the column around it in
//! `WindowContentView.swift`: the divider, the tint wash, the footer; ported from the GTK build's `sidebar/`): a list
//! of workspace and session rows ([`rows`], a pure function of the model), the session rows' multi-selection, per-row
//! context menus ([`menu`]), inline rename, drag-and-drop reorder, spring-loaded folders and the file manager's folder
//! drops ([`drag`]), the flagged view's three layouts and its empty-state hint, the footer, and the divider that
//! resizes it. [`view`] draws it.
//!
//! RULE: the handlers never touch the model. They read the snapshot the last sync took ([`context`]) and post a
//! [`SidebarAction`] for the window to the main loop; what needs a dialog, the clipboard or the rename field happens
//! here first. A workspace-row click opens or folds the row after the double-click interval, so a double-click renames
//! without a toggle in between.

mod context;
mod drag;
mod look;
mod menu;
mod rows;
mod view;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use agterm_core::Core;
use agterm_core::model::{SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN};
use agterm_core::sidebar_actions::SidebarAction;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, EntityId, ExternalPaths, FocusHandle, Focusable as _,
    Modifiers, PromptButton, PromptLevel, ScrollHandle, Subscription, Task, WeakEntity, Window,
};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use uuid::Uuid;

pub use self::look::Look;
pub use self::rows::{RowKind, SidebarRow};

use self::context::SidebarContext;
use self::drag::{DragPayload, Highlight, Proposal, RowSpan};
use self::menu::{MenuAction, MenuEntry};
use self::rows::{Expansion, RowSettings};
use crate::state::{self, AppState, Poster};

/// How long a drag hovers over a folded workspace before it opens for the drag (upstream's spring-loading).
const SPRING_DELAY: Duration = Duration::from_millis(650);
/// How long a workspace-row click waits for a second one before it toggles the row (GTK's default
/// `gtk-double-click-time`, which GPUI's Linux platform counts clicks with too).
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// The empty flagged view's hint (upstream's `installEmptyState`).
pub const EMPTY_FLAGGED: &str = "No flagged sessions.\nRight-click a session → Flag.";

/// What the footer's controls ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterAction {
    NewWorkspace,
    NewSession,
    OpenDirectory,
    ToggleFocusFilter,
    ToggleFlaggedView,
}

/// The row being renamed and its field.
struct Editing {
    id: Uuid,
    workspace: bool,
    input: Entity<InputState>,
    _events: Subscription,
}

/// What a drag over the list has done to the rows.
#[derive(Default)]
struct DragState {
    /// A drag of rows or folders is over the list.
    over: bool,
    highlight: Option<Highlight>,
    /// The folded workspace a hover is about to open.
    spring_pending: Option<(Uuid, Task<()>)>,
    /// The workspace the drag opened, folded back unless the drop lands.
    spring_open: Option<Uuid>,
}

impl DragState {
    /// The pending hover opened workspace `id`: it becomes the one tracked, and the workspace an earlier hover
    /// opened is returned to fold back, as upstream cancels the previous spring-load when the target changes.
    fn spring_opened(&mut self, id: Uuid) -> Option<Uuid> {
        self.spring_pending = None;
        self.spring_open.replace(id).filter(|previous| *previous != id)
    }
}

/// A row drag in progress: what it carries and what its preview shows.
pub struct RowDrag {
    payload: DragPayload,
    label: String,
    look: Look,
}

/// The divider drag in progress.
pub struct DividerDrag;

/// The width the divider drag asks for, handed to the main loop one task at a time.
#[derive(Default)]
struct Resize {
    /// A task carrying the latest width is queued.
    pending: AtomicBool,
    /// The latest width, as `f64` bits.
    width: AtomicU64,
}

/// The sidebar of one window.
pub struct Sidebar {
    window: Uuid,
    poster: Poster,
    /// The sidebar shows: the window's sidebar is visible and no zoom or dashboard covers it.
    shown: bool,
    /// The model's width.
    width: f64,
    /// The width a divider drag shows while it runs.
    resizing: Option<f64>,
    resize: Arc<Resize>,
    rows: Vec<SidebarRow>,
    context: SidebarContext,
    expansion: Expansion,
    look: Look,
    /// The selected session the list last scrolled to.
    revealed: Option<Uuid>,
    /// Where a Shift-click extends the selection from: the session row clicked last without Shift, or the model's
    /// selected session once a selection made elsewhere leaves that row out.
    anchor: Option<Uuid>,
    /// The row menu holding the keyboard, while it lives: its entity goes with the trigger's element state, so a
    /// menu dropped without a dismiss (its row gone, the sidebar hidden) stops holding it.
    menu: Option<WeakEntity<PopupMenu>>,
    editing: Option<Editing>,
    pending_toggle: Option<Task<()>>,
    /// The workspace row under the pointer, which shows its "+".
    hovered: Option<Uuid>,
    drag: DragState,
    scroll: ScrollHandle,
}

impl Sidebar {
    /// The sidebar of model window `window`, its handlers posting through `poster`; it paints in the window chrome's
    /// colors ([`crate::chrome`]).
    #[must_use]
    pub fn new(window: Uuid, poster: Poster) -> Self {
        Self {
            window,
            poster,
            shown: false,
            width: agterm_core::model::SIDEBAR_WIDTH_DEFAULT,
            resizing: None,
            resize: Arc::default(),
            rows: Vec::new(),
            context: SidebarContext::default(),
            expansion: Expansion::default(),
            look: Look::default(),
            revealed: None,
            anchor: None,
            menu: None,
            editing: None,
            pending_toggle: None,
            hovered: None,
            drag: DragState::default(),
            scroll: ScrollHandle::new(),
        }
    }

    /// The rows shown.
    #[must_use]
    pub fn rows(&self) -> &[SidebarRow] {
        &self.rows
    }

    /// Whether the sidebar shows.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// The width shown: the model's, or the divider drag's while it runs.
    #[must_use]
    pub fn width(&self) -> f64 {
        self.resizing.unwrap_or(self.width)
    }

    /// The rename field's text.
    #[cfg(test)]
    pub(crate) fn edited_text(&self, cx: &App) -> Option<String> {
        self.editing
            .as_ref()
            .map(|editing| editing.input.read(cx).value().to_string())
    }

    /// Whether a row menu holds the keyboard: the window's deck moves no focus until it closes.
    #[must_use]
    pub fn menu_open(&self) -> bool {
        self.menu.as_ref().is_some_and(|menu| menu.upgrade().is_some())
    }

    /// Row menu `menu` opened, replacing any other.
    fn menu_opened(&mut self, menu: WeakEntity<PopupMenu>) {
        self.menu = Some(menu);
    }

    /// Row menu `menu` closed, dismissed or dropped: a sync lands the focus move the deck held for it (a
    /// right-click's narrowed selection shows another session). A menu another one replaced changes nothing.
    fn menu_closed(&mut self, menu: EntityId) {
        if self.menu.as_ref().is_some_and(|open| open.entity_id() == menu) {
            self.menu = None;
            self.post(|_| {});
        }
    }

    /// The live rename field's focus, which keeps the keyboard a row menu's Rename gave it.
    #[must_use]
    pub fn keyboard_owner(&self, cx: &App) -> Option<FocusHandle> {
        self.editing.as_ref().map(|editing| editing.input.focus_handle(cx))
    }

    /// The row being renamed.
    #[must_use]
    pub fn editing(&self) -> Option<Uuid> {
        self.editing.as_ref().map(|editing| editing.id)
    }

    /// Renders the model: the rows, visibility and width (a zoomed terminal or the dashboard fills the window: the
    /// sidebar steps aside without its visibility changing), the snapshot the handlers read, and the look. Mirrors the
    /// rows the outline shows open back into the store (`AppStore::note_sidebar_expansion`, view state for
    /// Collapse/Expand Workspace).
    pub fn sync(&mut self, core: &mut Core, cx: &mut Context<Self>) {
        let id = self.window;
        let covered = core.zoom_target(id).is_some() || core.dashboard(id).is_some();
        let settings = core.settings().clone();
        self.look = Look::of(crate::chrome::current(cx), &settings);
        let Some(store) = core.library_mut().store_mut(id) else {
            return;
        };
        self.shown = store.sidebar().visible && !covered;
        self.width = store.sidebar().width;
        self.expansion.apply_requests(store);
        let layout = rows::shown_flagged_layout(store, &settings);
        let expanded = self.expansion.sync(store, layout).clone();
        let rows = rows::rows(store, &expanded, RowSettings { settings: &settings });
        store.note_sidebar_expansion(expanded);
        self.context = SidebarContext::take(store, &settings);
        let selected = store.selected_session_id();
        self.show(rows, selected, cx);
        cx.notify();
    }

    /// Shows `rows`. A change of the list's shape (a row added, removed or moved) closes a rename in progress, as the
    /// row it edits may be gone; a content change keeps it, so the row being renamed updates too (its glyph and badge
    /// live, its name behind the field) and is current when the field closes. The list scrolls to a newly selected
    /// session. A selection made elsewhere (the keymap, `agtermctl`, auto-follow) that leaves the Shift-click anchor
    /// out moves the anchor to the selected session, as GTK's list moves its range anchor with each row it selects;
    /// the sidebar's own Shift-click keeps it.
    fn show(&mut self, rows: Vec<SidebarRow>, selected: Option<Uuid>, cx: &mut Context<Self>) {
        let reshaped =
            self.rows.len() != rows.len() || self.rows.iter().zip(&rows).any(|(old, new)| old.shape() != new.shape());
        self.rows = rows;
        if reshaped && self.editing.is_some() {
            self.finish_rename(false, cx);
        }
        let selection = self.selected_ids();
        if self.anchor.is_none_or(|anchor| !selection.contains(&anchor)) {
            self.anchor = selected;
        }
        if selected != self.revealed {
            self.revealed = selected;
            if let Some(index) = selected.and_then(|id| self.index_of(id)) {
                self.scroll.scroll_to_item(index);
            }
        }
    }

    fn index_of(&self, id: Uuid) -> Option<usize> {
        self.rows.iter().position(|row| row.id == id)
    }

    fn row(&self, id: Uuid) -> Option<&SidebarRow> {
        self.rows.iter().find(|row| row.id == id)
    }

    fn post(&self, task: impl FnOnce(&mut AppState) + Send + 'static) {
        state::post(&self.poster, task);
    }

    fn act(&self, action: SidebarAction) {
        let window = self.window;
        self.post(move |state: &mut AppState| state.sidebar_action(window, action));
    }

    // MARK: - selection

    /// The session rows the list shows selected, in visible order.
    fn selected_ids(&self) -> Vec<Uuid> {
        self.rows
            .iter()
            .filter(|row| matches!(row.kind, RowKind::Session { selected: true, .. }))
            .map(|row| row.id)
            .collect()
    }

    /// A click on session row `id` with `modifiers`: Ctrl toggles the row in the selection, Shift extends it from the
    /// anchor, a plain click selects the row alone. The clicked row (else the still-selected active session, else the
    /// last selected) becomes the active session (GTK's multiple-selection list, upstream's Shift/Command selection).
    pub fn session_clicked(&mut self, id: Uuid, modifiers: Modifiers) {
        let selection = click_selection(&self.rows, &self.selected_ids(), self.anchor, id, modifiers);
        if !modifiers.shift {
            self.anchor = Some(id);
        }
        let active = selection
            .contains(&id)
            .then_some(id)
            .or_else(|| self.context.active_session.filter(|active| selection.contains(active)))
            .or_else(|| selection.last().copied());
        match active {
            Some(active) => self.act(SidebarAction::SelectSessions { active, selection }),
            None => self.act(SidebarAction::SetSelection(selection)),
        }
    }

    /// A right-click on row `id`: outside the selection it narrows the selection to the clicked session row, inside
    /// it keeps it, so the menu acts on the whole selection.
    fn secondary_pressed(&mut self, id: Uuid) {
        let Some(row) = self.row(id) else { return };
        if row.is_workspace()
            || matches!(row.kind, RowKind::Session { selected: true, .. })
            || self.context.menu.selection.contains(&id)
        {
            return;
        }
        self.anchor = Some(id);
        self.act(SidebarAction::SelectSessions {
            active: id,
            selection: vec![id],
        });
    }

    // MARK: - clicks

    /// A primary click on row `id`, the `count`th in a row: a double-click renames, a single one selects a session or
    /// (after the double-click interval) toggles a workspace.
    pub fn row_clicked(
        &mut self,
        id: Uuid,
        count: usize,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing.as_ref().is_some_and(|editing| editing.id == id) {
            return;
        }
        let Some(workspace) = self.row(id).map(SidebarRow::is_workspace) else {
            return;
        };
        if count >= 2 {
            self.cancel_toggle();
            self.begin_rename(id, window, cx);
        } else if workspace {
            self.schedule_toggle(id, cx);
        } else {
            self.session_clicked(id, modifiers);
        }
    }

    fn cancel_toggle(&mut self) {
        self.pending_toggle = None;
    }

    /// A single click on a workspace row: toggles it after the double-click interval, unless a double-click renames
    /// first or the setting turns the whole-row target off (re-read when the interval ends).
    fn schedule_toggle(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if !self.context.row_click_expands {
            return;
        }
        self.pending_toggle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DOUBLE_CLICK).await;
            let toggled = this.update(cx, |this, _| {
                this.pending_toggle = None;
                if this.context.row_click_expands {
                    this.toggle(id);
                }
            });
            if toggled.is_err() {
                tracing::debug!("the sidebar is gone before its row toggled");
            }
        }));
    }

    /// Opens or folds workspace row `id`, persisted (a user toggle: the disclosure, or a row click).
    pub fn toggle(&mut self, id: Uuid) {
        let Some(RowKind::Workspace { expanded }) = self.row(id).map(|row| row.kind) else {
            return;
        };
        let window = self.window;
        self.post(move |state: &mut AppState| state.sidebar_toggle_workspace(window, id, !expanded));
    }

    /// The workspace row's hover "+": a session in it.
    fn add_session_clicked(&mut self, workspace: Uuid) {
        self.cancel_toggle();
        self.act(SidebarAction::NewSession { workspace });
    }

    fn set_hovered(&mut self, id: Uuid, hovered: bool, cx: &mut Context<Self>) {
        let now = if hovered {
            Some(id)
        } else {
            self.hovered.filter(|known| *known != id)
        };
        if now != self.hovered {
            self.hovered = now;
            cx.notify();
        }
    }

    // MARK: - rename

    /// Starts the inline rename of session or workspace row `id` (a double-click, the row menu's Rename, the Rename
    /// Session and Rename Workspace actions), seeded with the bare name (never the flat layout's ` : workspace`), all of
    /// it selected; nothing when the row is not shown or an edit is already open.
    pub fn begin_rename(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            return;
        }
        let Some(row) = self.row(id) else { return };
        let workspace = row.is_workspace();
        let text = if workspace {
            row.label.clone()
        } else {
            match self.context.menu.sessions.get(&id) {
                Some(facts) => facts.name.clone(),
                None => return,
            }
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(text));
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.finish_rename(true, cx);
            }
        });
        self.editing = Some(Editing {
            id,
            workspace,
            input,
            _events: events,
        });
        // an armed auto-follow jump would move the selection off this row mid-edit
        let window_id = self.window;
        self.post(move |state: &mut AppState| state.sidebar_rename_began(window_id));
        cx.notify();
    }

    /// Ends the rename in progress, renaming when `commit` (Enter or the field losing the keyboard; Escape cancels),
    /// and hands the keyboard back to the terminal.
    pub fn finish_rename(&mut self, commit: bool, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else { return };
        let action = commit.then(|| {
            let name = editing.input.read(cx).value().to_string();
            if editing.workspace {
                SidebarAction::RenameWorkspace {
                    workspace: editing.id,
                    name,
                }
            } else {
                SidebarAction::RenameSession {
                    session: editing.id,
                    name,
                }
            }
        });
        let window = self.window;
        self.post(move |state: &mut AppState| state.sidebar_rename_ended(window, action));
        cx.notify();
    }

    // MARK: - menus

    /// Row `id`'s context menu, from the last sync's snapshot (upstream's `menu(forRow:)`).
    fn menu_entries(&self, id: Uuid) -> Vec<MenuEntry> {
        let Some(row) = self.row(id) else {
            return Vec::new();
        };
        if row.is_workspace() {
            menu::workspace_menu(&self.context.menu, id)
        } else {
            menu::session_menu(&self.context.menu, id, |cwd| Path::new(cwd).is_dir())
        }
    }

    /// Runs a menu item.
    fn perform(&mut self, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            MenuAction::ClearStatus(ids) => self.act(SidebarAction::ClearStatus(ids)),
            MenuAction::Rename(id) => self.begin_rename(id, window, cx),
            MenuAction::CopyName(Some(name)) => cx.write_to_clipboard(ClipboardItem::new_string(name)),
            MenuAction::CopyName(None) => {}
            MenuAction::Duplicate(id) => self.act(SidebarAction::Duplicate(id)),
            MenuAction::MoveTo { sessions, workspace } => self.act(SidebarAction::MoveSessions { sessions, workspace }),
            MenuAction::ToggleFlags(ids) => self.act(SidebarAction::ToggleFlags(ids)),
            MenuAction::Reveal(cwd) => cx.reveal_path(Path::new(&cwd)),
            MenuAction::Close(ids) => self.close_sessions(ids, window, cx),
            MenuAction::NewSession(workspace) => self.act(SidebarAction::NewSession { workspace }),
            MenuAction::OpenDirectory(workspace) => self.open_directory(workspace, window, cx),
            MenuAction::ToggleFocus(id) => self.act(SidebarAction::ToggleFocus(id)),
            MenuAction::FocusMembership { workspace, member } => {
                self.act(SidebarAction::SetFocusMembership { workspace, member });
            }
            MenuAction::DeleteWorkspace(id) => self.delete_workspace(id, window, cx),
        }
    }

    /// Runs a footer control.
    fn footer_action(&mut self, action: FooterAction, window: &Window, cx: &mut Context<Self>) {
        match action {
            FooterAction::NewWorkspace => self.act(SidebarAction::NewWorkspace),
            FooterAction::NewSession => self.act(SidebarAction::NewSessionInCurrent),
            FooterAction::OpenDirectory => {
                if let Some(workspace) = self.context.drop.current_workspace {
                    self.open_directory(workspace, window, cx);
                }
            }
            FooterAction::ToggleFocusFilter => self.act(SidebarAction::ToggleFocusFilter),
            FooterAction::ToggleFlaggedView => self.act(SidebarAction::ToggleFlaggedView),
        }
    }

    // MARK: - dialogs

    /// Close Session for `ids`, confirmed first when the setting asks for it (upstream's `confirmCloseSession(s)`).
    pub fn close_sessions(&mut self, ids: Vec<Uuid>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.context.confirm_close {
            self.act(SidebarAction::CloseSessions(ids));
            return;
        }
        let (heading, body) = close_confirmation(&self.context, &ids);
        self.confirm(
            (&heading, &body, "Close"),
            SidebarAction::CloseSessions(ids),
            window,
            cx,
        );
    }

    /// Delete Workspace `id`, confirmed first while it holds sessions (upstream's `confirmDeleteWorkspace`).
    pub fn delete_workspace(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(facts) = self.context.menu.workspaces.iter().find(|facts| facts.id == id) else {
            return;
        };
        if facts.session_count == 0 {
            self.act(SidebarAction::DeleteWorkspace(id));
            return;
        }
        let heading = format!("Delete “{}”?", facts.name);
        let body = delete_confirmation(facts.session_count, self.context.close_grace);
        self.confirm(
            (&heading, &body, "Delete"),
            SidebarAction::DeleteWorkspace(id),
            window,
            cx,
        );
    }

    /// A destructive confirmation over the window; `action` runs on the confirming answer.
    /// `(heading, body, verb)`: the question, its detail and the confirming answer's label.
    fn confirm(
        &self,
        (heading, body, verb): (&str, &str, &str),
        action: SidebarAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answers = [PromptButton::cancel("Cancel"), PromptButton::ok(verb.to_owned())];
        let answer = window.prompt(PromptLevel::Warning, heading, Some(body), &answers, cx);
        let (poster, id) = (self.poster.clone(), self.window);
        cx.spawn(async move |_, _| {
            if answer.await == Ok(1) {
                state::post(&poster, move |state: &mut AppState| state.sidebar_action(id, action));
            }
        })
        .detach();
    }

    /// Open Directory…: a folder chooser over `window`, starting in the active session's directory, then a session
    /// there in `workspace`.
    pub fn open_directory(&self, workspace: Uuid, window: &Window, cx: &mut Context<Self>) {
        let start = self.context.chooser_folder().map(PathBuf::from);
        let parent = window.window_handle();
        let (poster, window) = (self.poster.clone(), self.window);
        cx.spawn(async move |_, cx| {
            // read from the live window as the chooser opens, so the handles never outlive it
            let handles = cx
                .update_window(parent, |_, window, _| raw_handles(window))
                .ok()
                .flatten();
            let parent = match handles {
                Some((window, display)) => ashpd::WindowIdentifier::from_raw_handle(&window, Some(&display)).await,
                None => None,
            };
            let Some(path) = pick_directory(start, parent).await else {
                return;
            };
            let directories = vec![path.to_string_lossy().into_owned()];
            state::post(&poster, move |state: &mut AppState| {
                state.sidebar_action(window, SidebarAction::OpenDirectories { workspace, directories });
            });
        })
        .detach();
    }

    // MARK: - the divider

    /// The divider dragged to `x` (window coordinates; the sidebar's leading edge at `left`): the width follows the
    /// pointer (from its absolute position, not the drag's translation, which would feed back on itself), clamped, and
    /// the model takes it one task at a time.
    fn divider_moved(&mut self, x: f64, left: f64, cx: &mut Context<Self>) {
        let width = (x - left).round().clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX);
        if self.resizing == Some(width) {
            return;
        }
        self.resizing = Some(width);
        self.resize.width.store(width.to_bits(), Ordering::Release);
        if !self.resize.pending.swap(true, Ordering::AcqRel) {
            let (resize, window) = (Arc::clone(&self.resize), self.window);
            self.post(move |state: &mut AppState| {
                resize.pending.store(false, Ordering::Release);
                let width = f64::from_bits(resize.width.load(Ordering::Acquire));
                state.sidebar_resized(window, width);
            });
        }
        cx.notify();
    }

    // MARK: - drag and drop

    /// Each row's vertical extent on screen (window coordinates), from the last layout.
    fn spans(&self) -> Vec<RowSpan> {
        let offset = f64::from(self.scroll.offset().y);
        (0..self.rows.len())
            .map(|index| {
                self.scroll
                    .bounds_for_item(index)
                    .map_or(RowSpan { top: -1.0, height: 0.0 }, |bounds| RowSpan {
                        top: f64::from(bounds.origin.y) + offset,
                        height: f64::from(bounds.size.height),
                    })
            })
            .collect()
    }

    fn propose(&self, payload: &DragPayload, y: f64) -> Option<(Proposal, Highlight)> {
        if !self.context.drop.tree {
            return None;
        }
        let spans = self.spans();
        if payload.workspace {
            drag::propose_workspace(&self.rows, &spans, y, *payload.ids.first()?, &self.context.drop)
        } else {
            drag::propose_sessions(&self.rows, &spans, y, &payload.ids, &self.context.drop)
        }
    }

    /// The rows drag of `payload` hovers at `y`: the drop it proposes is highlighted, and a session drag spring-loads
    /// a folded workspace.
    fn rows_dragged_over(&mut self, payload: &DragPayload, y: f64, cx: &mut Context<Self>) {
        self.drag.over = true;
        let proposal = self.propose(payload, y);
        self.set_highlight(proposal.map(|(_, highlight)| highlight), cx);
        if !payload.workspace {
            self.spring_load(y, cx);
        }
    }

    /// The rows drag of `payload` dropped at `y`: the move it proposes, if any.
    fn rows_dropped(&mut self, payload: &DragPayload, y: f64, cx: &mut Context<Self>) {
        let action = match self.propose(payload, y) {
            Some((
                Proposal::Sessions {
                    sessions,
                    workspace,
                    destination,
                },
                _,
            )) => SidebarAction::DropSessions {
                sessions,
                workspace,
                destination,
            },
            Some((Proposal::Workspace { workspace, destination }, _)) => {
                SidebarAction::DropWorkspace { workspace, destination }
            }
            Some((Proposal::Directories { .. }, _)) | None => {
                self.end_drag(false, cx);
                return;
            }
        };
        self.end_drag(true, cx);
        self.act(action);
    }

    /// The file manager's folders hover at `y`: the workspace they would land in is highlighted, a folded one
    /// spring-loads.
    fn folders_dragged_over(&mut self, y: f64, cx: &mut Context<Self>) {
        self.drag.over = true;
        let proposal = drag::propose_directories(&self.rows, &self.spans(), y, &self.context.drop);
        self.set_highlight(proposal.and_then(|(_, highlight)| highlight), cx);
        self.spring_load(y, cx);
    }

    /// The file manager's `paths` dropped at `y`: a session per directory in the proposed workspace, at most the
    /// import cap ("Too Many Folders" past it).
    fn folders_dropped(&mut self, paths: &ExternalPaths, y: f64, window: &mut Window, cx: &mut Context<Self>) {
        let Some((Proposal::Directories { workspace }, _)) =
            drag::propose_directories(&self.rows, &self.spans(), y, &self.context.drop)
        else {
            self.end_drag(false, cx);
            return;
        };
        match drag::directories(paths.paths().iter().cloned()) {
            Ok(directories) if !directories.is_empty() => {
                self.end_drag(true, cx);
                self.act(SidebarAction::OpenDirectories { workspace, directories });
            }
            Ok(_) => self.end_drag(false, cx),
            Err(()) => {
                self.end_drag(false, cx);
                too_many_folders(window, cx);
            }
        }
    }

    fn set_highlight(&mut self, highlight: Option<Highlight>, cx: &mut Context<Self>) {
        if self.drag.highlight != highlight {
            self.drag.highlight = highlight;
            cx.notify();
        }
    }

    /// Opens the folded workspace under `y` for the drag after a hover (view only), or forgets a pending open once the
    /// pointer left it.
    fn spring_load(&mut self, y: f64, cx: &mut Context<Self>) {
        let hovered = drag::row_at(&self.spans(), y).and_then(|index| {
            let row = self.rows.get(index)?;
            matches!(row.kind, RowKind::Workspace { expanded: false }).then_some(row.id)
        });
        if self.drag.spring_pending.as_ref().map(|(id, _)| *id) == hovered {
            return;
        }
        self.drag.spring_pending = None;
        let Some(id) = hovered.filter(|_| self.context.drop.tree) else {
            return;
        };
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SPRING_DELAY).await;
            let opened = this.update(cx, |this, _| this.spring_open(id));
            if opened.is_err() {
                tracing::debug!("the sidebar is gone before its spring-load");
            }
        });
        self.drag.spring_pending = Some((id, task));
    }

    /// The spring-load of workspace `id` fired: it opens for view, and one an earlier hover opened folds back.
    fn spring_open(&mut self, id: Uuid) {
        if let Some(previous) = self.drag.spring_opened(id) {
            self.expansion.set(previous, false);
        }
        self.expansion.set(id, true);
        self.refresh();
    }

    /// Ends a drag over the list: the highlight goes, and a workspace the drag opened folds back unless `landed`.
    fn end_drag(&mut self, landed: bool, cx: &mut Context<Self>) {
        self.drag.over = false;
        self.drag.spring_pending = None;
        self.set_highlight(None, cx);
        if let Some(id) = self.drag.spring_open.take().filter(|_| !landed) {
            self.expansion.set(id, false);
            self.refresh();
        }
    }

    /// Asks for a sync: the rows are rebuilt from the model after the next main-loop task, this one changing only the
    /// outline's view state.
    fn refresh(&self) {
        self.post(|_: &mut AppState| {});
    }

    /// A drag the list was showing ended without a drop on it, or the divider drag ended: what they left is undone.
    fn settle_drags(&mut self, cx: &mut Context<Self>) {
        if cx.has_active_drag() {
            return;
        }
        if self.drag.over {
            self.end_drag(false, cx);
        }
        if let Some(width) = self.resizing.take() {
            self.width = width;
        }
    }
}

/// The selection a click on session row `clicked` makes, in visible order: Ctrl toggles the row, Shift selects the
/// session rows from `anchor` to it (added to the selection with Ctrl too), a plain click selects it alone.
#[must_use]
pub fn click_selection(
    rows: &[SidebarRow],
    current: &[Uuid],
    anchor: Option<Uuid>,
    clicked: Uuid,
    modifiers: Modifiers,
) -> Vec<Uuid> {
    let sessions: Vec<Uuid> = rows
        .iter()
        .filter(|row| !row.is_workspace())
        .map(|row| row.id)
        .collect();
    let position = |id: Uuid| sessions.iter().position(|known| *known == id);
    let range = || -> Vec<Uuid> {
        match (anchor.and_then(position), position(clicked)) {
            (Some(from), Some(to)) => sessions[from.min(to)..=from.max(to)].to_vec(),
            _ => vec![clicked],
        }
    };
    let chosen: Vec<Uuid> = match (modifiers.shift, modifiers.control) {
        (true, false) => range(),
        (true, true) => current.iter().copied().chain(range()).collect(),
        (false, true) if current.contains(&clicked) => current.iter().copied().filter(|id| *id != clicked).collect(),
        (false, true) => current.iter().copied().chain([clicked]).collect(),
        (false, false) => vec![clicked],
    };
    sessions.into_iter().filter(|id| chosen.contains(id)).collect()
}

/// The close confirmation's heading and body (upstream's `confirmCloseSession` and `confirmCloseSessions`).
fn close_confirmation(context: &SidebarContext, ids: &[Uuid]) -> (String, String) {
    let grace = context.close_grace;
    if let [id] = ids {
        let name = context.menu.sessions.get(id).map_or("", |facts| facts.name.as_str());
        let body = if grace {
            "The session will close after a short undo window."
        } else {
            "The session will close immediately and can be reopened from File > Open Recent."
        };
        return (format!("Close “{name}”?"), body.to_owned());
    }
    let body = if grace {
        "The sessions will close after a short undo window."
    } else {
        "The sessions will close immediately and can be reopened from File > Open Recent."
    };
    (format!("Close {} Sessions?", ids.len()), body.to_owned())
}

/// The delete confirmation's body (upstream's `confirmDelete`).
fn delete_confirmation(sessions: usize, grace: bool) -> String {
    let suffix = if grace {
        "after a short undo window."
    } else {
        "immediately. You can reopen it from File > Open Recent."
    };
    if sessions == 1 {
        format!("This closes its session {suffix}")
    } else {
        format!("This closes {sessions} sessions {suffix}")
    }
}

/// "Too Many Folders" (upstream's `presentDirectoryDropLimitAlert`).
fn too_many_folders(window: &mut Window, cx: &mut App) {
    let body = format!(
        "You can open up to {} folders at once.",
        agterm_core::sidebar_drop::MAXIMUM_DIRECTORY_IMPORT_COUNT
    );
    // the only answer is OK: nothing waits on it
    drop(window.prompt(
        PromptLevel::Info,
        "Too Many Folders",
        Some(&body),
        &[PromptButton::ok("OK")],
        cx,
    ));
}

/// `window`'s raw window and display handles (an X11 window id, or a Wayland surface and display), which the portal
/// names its parent by; `None` when the platform has none.
fn raw_handles(window: &Window) -> Option<(RawWindowHandle, RawDisplayHandle)> {
    let raw = HasWindowHandle::window_handle(window).ok()?.as_raw();
    let display = HasDisplayHandle::display_handle(window).ok()?.as_raw();
    Some((raw, display))
}

/// The desktop's folder chooser (the file chooser portal, as GTK's `FileDialog` and GPUI's own prompt use it), modal
/// over `parent` (the window as the portal names it: an X11 id or an `xdg-foreign` export), titled and starting in
/// `start`; the folder picked, `None` when cancelled or without a portal (logged).
async fn pick_directory(start: Option<PathBuf>, parent: Option<ashpd::WindowIdentifier>) -> Option<PathBuf> {
    use ashpd::desktop::file_chooser::OpenFileRequest;
    let base = || {
        OpenFileRequest::default()
            .title("Choose a directory for the new session")
            .accept_label("Open")
            .modal(true)
            .multiple(false)
            .directory(true)
    };
    let request = match start {
        Some(folder) => base().current_folder(folder).unwrap_or_else(|_| base()),
        None => base(),
    };
    let request = match request.identifier(parent).send().await {
        Ok(request) => request,
        Err(err) => {
            tracing::warn!(%err, "the folder chooser did not open");
            return None;
        }
    };
    let selected = request.response().ok()?;
    selected
        .uris()
        .first()
        .and_then(|uri| url::Url::parse(uri.as_str()).ok())
        .and_then(|url| url.to_file_path().ok())
}
