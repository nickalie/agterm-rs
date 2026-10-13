//! The sidebar after upstream's `WorkspaceSidebar.swift` (its outline coordinator), in GTK list idioms: a list of
//! workspace and session rows ([`rows`]), each built once and updated in place ([`row`]); the session rows'
//! multi-selection; per-row context menus ([`menu`]); inline rename; drag-and-drop reorder and folder import
//! ([`drag`]); the flagged view's three layouts and its empty-state hint; and the footer ([`footer`]).
//!
//! RULE: the handlers never touch the model. They read the snapshot the last refresh took ([`context`]) and post a
//! [`SidebarAction`] for the window to the main loop; what needs a dialog, the clipboard or the rename field happens
//! here first. A row click selects through the list's own selection; a workspace-row click opens or folds the row
//! after the double-click interval, so a double-click renames without a toggle in between.

mod context;
mod drag;
mod footer;
mod menu;
mod row;
mod rows;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use agterm_core::model::AppStore;
use agterm_core::persist::AppSettings;
use agterm_core::sidebar_actions::SidebarAction;
use gtk::{gdk, gio, glib};
use uuid::Uuid;

use self::context::SidebarContext;
use self::drag::{DragPayload, Highlight, Proposal, RowSpan};
use self::footer::{Footer, FooterAction};
use self::menu::MenuAction;
use self::row::RowWidget;
use self::rows::{Expansion, RowKind, RowSettings, SidebarRow};
use crate::state::{self, AppState, Poster};

/// How long a drag hovers over a folded workspace before it opens for the drag (upstream's spring-loading).
const SPRING_DELAY: Duration = Duration::from_millis(650);

/// The empty flagged view's hint (upstream's `installEmptyState`).
const EMPTY_FLAGGED: &str = "No flagged sessions.\nRight-click a session → Flag.";

/// The row being renamed.
#[derive(Debug, Clone, Copy)]
struct Editing {
    id: Uuid,
    workspace: bool,
}

/// What a drag in progress has done to the rows.
#[derive(Default)]
struct DragState {
    highlighted: Vec<gtk::ListBoxRow>,
    /// The folded workspace a hover is about to open.
    spring_pending: Option<(Uuid, glib::SourceId)>,
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

/// The state the list's handlers share.
struct Shared {
    window: Uuid,
    poster: Poster,
    list: gtk::ListBox,
    scrolled: gtk::ScrolledWindow,
    rows: RefCell<Vec<SidebarRow>>,
    widgets: RefCell<Vec<RowWidget>>,
    context: RefCell<SidebarContext>,
    /// Set while the selection is set from the model, so the change handler ignores it.
    applying: Cell<bool>,
    /// The session row the pointer last pressed: the one a selection change makes active.
    pressed: Cell<Option<Uuid>>,
    /// The selected session the list last scrolled to.
    revealed: Cell<Option<Uuid>>,
    editing: Cell<Option<Editing>>,
    pending_toggle: RefCell<Option<glib::SourceId>>,
    drag: RefCell<DragState>,
}

/// The sidebar of one window.
pub struct Sidebar {
    root: gtk::Box,
    shared: Rc<Shared>,
    footer: Footer,
    empty: gtk::Label,
    expansion: Expansion,
}

impl Sidebar {
    #[must_use]
    pub fn new(window: Uuid, poster: &Poster) -> Self {
        install_css();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Multiple)
            .activate_on_single_click(false)
            .focusable(false)
            .build();
        list.add_css_class("navigation-sidebar");
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        let empty = gtk::Label::builder()
            .label(EMPTY_FLAGGED)
            .justify(gtk::Justification::Center)
            .wrap(true)
            .valign(gtk::Align::Start)
            .halign(gtk::Align::Center)
            .margin_top(40)
            .margin_start(16)
            .margin_end(16)
            .can_target(false)
            .visible(false)
            .build();
        empty.add_css_class("dim-label");
        let overlay = gtk::Overlay::builder().child(&scrolled).vexpand(true).build();
        overlay.add_overlay(&empty);
        let shared = Rc::new(Shared {
            window,
            poster: poster.clone(),
            list,
            scrolled,
            rows: RefCell::default(),
            widgets: RefCell::default(),
            context: RefCell::default(),
            applying: Cell::new(false),
            pressed: Cell::new(None),
            revealed: Cell::new(None),
            editing: Cell::new(None),
            pending_toggle: RefCell::default(),
            drag: RefCell::default(),
        });
        let weak = Rc::downgrade(&shared);
        let footer = Footer::new(move |action| {
            if let Some(shared) = weak.upgrade() {
                shared.footer_action(action);
            }
        });
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("agterm-sidebar");
        root.append(&overlay);
        root.append(footer.widget());
        connect_list(&shared);
        Self {
            root,
            shared,
            footer,
            empty,
            expansion: Expansion::default(),
        }
    }

    /// The widget to place in the window.
    #[must_use]
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Opens or folds workspace `id`'s row in the outline, without persisting it.
    pub fn set_expanded(&mut self, id: Uuid, expanded: bool) {
        self.expansion.set(id, expanded);
    }

    /// Starts the inline rename of session or workspace row `id` (the Rename Session and Rename Workspace actions);
    /// nothing when the row is not shown or an edit is already open.
    pub fn begin_rename(&self, id: Uuid) {
        self.shared.begin_rename(id);
    }

    /// Close Session for `ids`, confirmed first when the setting asks for it.
    pub fn close_sessions(&self, ids: Vec<Uuid>) {
        self.shared.confirm_close(ids);
    }

    /// Delete Workspace `id`, confirmed first while it holds sessions.
    pub fn delete_workspace(&self, id: Uuid) {
        self.shared.confirm_delete(id);
    }

    /// Open Directory…: the folder chooser, then a session there in `workspace`.
    pub fn open_directory(&self, workspace: Uuid) {
        self.shared.choose_directory(workspace);
    }

    /// Re-renders from `store` under `settings`, mirroring the rows the outline shows open back into it
    /// (`AppStore::note_sidebar_expansion`, view state for Collapse/Expand Workspace).
    pub fn refresh(&mut self, store: &mut AppStore, settings: &AppSettings) {
        self.root.set_visible(store.sidebar().visible);
        self.expansion.apply_requests(store);
        let layout = rows::shown_flagged_layout(store, settings);
        let expanded = self.expansion.sync(store, layout).clone();
        let rows = rows::rows(store, &expanded, RowSettings { settings });
        store.note_sidebar_expansion(expanded);
        let context = SidebarContext::take(store, settings);
        self.footer.update(context.footer);
        self.empty.set_visible(context.empty_flagged);
        *self.shared.context.borrow_mut() = context;
        apply_font_size(settings.sidebar_font_size);
        self.shared.render(rows, store.selected_session_id());
    }
}

impl Shared {
    fn post(&self, task: impl FnOnce(&mut AppState) + Send + 'static) {
        state::post(&self.poster, task);
    }

    fn act(&self, action: SidebarAction) {
        let window = self.window;
        self.post(move |state: &mut AppState| state.sidebar_action(window, action));
    }

    /// The row a list row shows, by its current position.
    fn row_of(&self, list_row: &gtk::ListBoxRow) -> Option<SidebarRow> {
        let index = usize::try_from(list_row.index()).ok()?;
        self.rows.borrow().get(index).cloned()
    }

    fn index_of(&self, id: Uuid) -> Option<usize> {
        self.rows.borrow().iter().position(|row| row.id == id)
    }

    // MARK: - rendering

    /// Shows `rows`: rebuilt when their shape changed (a row added, removed or moved), else updated in place; the
    /// selection follows the model. An in-place update never touches the rename field, so the row being renamed
    /// updates too (its glyph and badge live; its name behind the field) and is current when the field closes.
    fn render(self: &Rc<Self>, rows: Vec<SidebarRow>, selected: Option<Uuid>) {
        let reshaped = {
            let current = self.rows.borrow();
            current.len() != rows.len() || current.iter().zip(&rows).any(|(old, new)| old.shape() != new.shape())
        };
        if reshaped {
            if self.editing.get().is_some() {
                self.finish_rename(false);
            }
            self.rebuild(&rows);
        } else {
            let current = self.rows.borrow();
            for ((old, new), widget) in current.iter().zip(&rows).zip(self.widgets.borrow().iter()) {
                if old != new {
                    widget.update(new);
                }
            }
        }
        *self.rows.borrow_mut() = rows;
        self.sync_selection();
        if selected != self.revealed.get() {
            self.revealed.set(selected);
            self.scroll_to(selected);
        }
    }

    fn rebuild(self: &Rc<Self>, rows: &[SidebarRow]) {
        self.clear_highlight();
        // removing selected rows changes the list's selection: not the user's doing
        self.applying.set(true);
        self.list.remove_all();
        let mut widgets = Vec::with_capacity(rows.len());
        for row in rows {
            let widget = RowWidget::build(row);
            wire_row(self, &widget, row.is_workspace());
            self.list.append(widget.widget());
            widgets.push(widget);
        }
        *self.widgets.borrow_mut() = widgets;
        self.applying.set(false);
    }

    fn sync_selection(&self) {
        self.applying.set(true);
        for (row, widget) in self.rows.borrow().iter().zip(self.widgets.borrow().iter()) {
            let list_row = widget.widget();
            match row.kind {
                RowKind::Session { selected: true, .. } if !list_row.is_selected() => {
                    self.list.select_row(Some(list_row));
                }
                RowKind::Session { selected: false, .. } if list_row.is_selected() => self.list.unselect_row(list_row),
                _ => {}
            }
        }
        self.applying.set(false);
    }

    /// Scrolls session `id`'s row into view once laid out.
    fn scroll_to(self: &Rc<Self>, id: Option<Uuid>) {
        let Some(id) = id else { return };
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(shared) = weak.upgrade() else { return };
            let Some(index) = shared.index_of(id) else { return };
            let widgets = shared.widgets.borrow();
            let Some(bounds) = widgets
                .get(index)
                .and_then(|widget| widget.widget().compute_bounds(&shared.list))
            else {
                return;
            };
            let top = f64::from(bounds.y());
            shared
                .scrolled
                .vadjustment()
                .clamp_page(top, top + f64::from(bounds.height()));
        });
    }

    /// The model selected rows in the list: the selected session rows become the multi-selection, the pressed one
    /// (else the still-selected active one, else the last) the active session.
    fn selection_changed(&self) {
        if self.applying.get() {
            return;
        }
        let selected: Vec<Uuid> = {
            let rows = self.rows.borrow();
            let widgets = self.widgets.borrow();
            rows.iter()
                .zip(widgets.iter())
                .filter(|(row, widget)| !row.is_workspace() && widget.widget().is_selected())
                .map(|(row, _)| row.id)
                .collect()
        };
        let pressed = self.pressed.take().filter(|id| selected.contains(id));
        let active = self.context.borrow().active_session.filter(|id| selected.contains(id));
        match pressed.or(active).or_else(|| selected.last().copied()) {
            Some(active) => self.act(SidebarAction::SelectSessions {
                active,
                selection: selected,
            }),
            None => self.act(SidebarAction::SetSelection(selected)),
        }
    }

    // MARK: - workspace rows

    fn cancel_toggle(&self) {
        if let Some(source) = self.pending_toggle.borrow_mut().take() {
            source.remove();
        }
    }

    /// A single click on a workspace row: toggles it after the double-click interval, unless a double-click renames
    /// first or the setting turns the whole-row target off.
    fn schedule_toggle(self: &Rc<Self>, id: Uuid) {
        if !self.context.borrow().row_click_expands {
            return;
        }
        self.cancel_toggle();
        let interval = gtk::Settings::default().map_or(400, |settings| settings.gtk_double_click_time());
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(
            Duration::from_millis(u64::try_from(interval).unwrap_or(400)),
            move || {
                let Some(shared) = weak.upgrade() else { return };
                // the source is firing: forget it rather than remove it
                shared.pending_toggle.borrow_mut().take();
                if shared.context.borrow().row_click_expands {
                    shared.toggle(id);
                }
            },
        );
        *self.pending_toggle.borrow_mut() = Some(source);
    }

    /// Opens or folds workspace `id`'s row, persisted (a user toggle).
    fn toggle(&self, id: Uuid) {
        let Some(expanded) = self
            .rows
            .borrow()
            .iter()
            .find(|row| row.id == id)
            .and_then(|row| match row.kind {
                RowKind::Workspace { expanded } => Some(expanded),
                RowKind::Session { .. } => None,
            })
        else {
            return;
        };
        let window = self.window;
        self.post(move |state: &mut AppState| state.sidebar_toggle_workspace(window, id, !expanded));
    }

    // MARK: - rename

    /// Starts the inline rename of row `id`, seeded with the bare name (never the flat layout's ` : workspace`).
    fn begin_rename(&self, id: Uuid) {
        if self.editing.get().is_some() {
            return;
        }
        let Some(index) = self.index_of(id) else { return };
        let workspace = self.rows.borrow()[index].is_workspace();
        let text = if workspace {
            self.rows.borrow()[index].label.clone()
        } else {
            match self.context.borrow().menu.sessions.get(&id) {
                Some(facts) => facts.name.clone(),
                None => return,
            }
        };
        self.editing.set(Some(Editing { id, workspace }));
        if let Some(widget) = self.widgets.borrow().get(index) {
            widget.begin_edit(&text);
        }
        // an armed auto-follow jump would move the selection off this row mid-edit
        let window = self.window;
        self.post(move |state: &mut AppState| state.sidebar_rename_began(window));
    }

    /// Ends the rename in progress, renaming when `commit`, and hands the keyboard back to the terminal.
    fn finish_rename(&self, commit: bool) {
        let Some(editing) = self.editing.take() else { return };
        let mut action = None;
        if let Some(index) = self.index_of(editing.id)
            && let Some(widget) = self.widgets.borrow().get(index)
        {
            let name = widget.edited_text();
            widget.end_edit();
            let rename = if editing.workspace {
                SidebarAction::RenameWorkspace {
                    workspace: editing.id,
                    name,
                }
            } else {
                SidebarAction::RenameSession {
                    session: editing.id,
                    name,
                }
            };
            action = commit.then_some(rename);
        }
        let window = self.window;
        self.post(move |state: &mut AppState| state.sidebar_rename_ended(window, action));
    }

    // MARK: - context menu

    fn context_menu(self: &Rc<Self>, list_row: &gtk::ListBoxRow, row: &SidebarRow, x: f64, y: f64) {
        let entries = {
            let context = self.context.borrow();
            if row.is_workspace() {
                menu::workspace_menu(&context.menu, row.id)
            } else {
                // a right-click outside the selection narrows it to the clicked row
                if !context.menu.selection.contains(&row.id)
                    && !matches!(row.kind, RowKind::Session { selected: true, .. })
                {
                    self.act(SidebarAction::SelectSessions {
                        active: row.id,
                        selection: vec![row.id],
                    });
                }
                menu::session_menu(&context.menu, row.id, |cwd| std::path::Path::new(cwd).is_dir())
            }
        };
        let weak = Rc::downgrade(self);
        menu::popup(list_row, x, y, entries, move |action| {
            if let Some(shared) = weak.upgrade() {
                shared.perform(action);
            }
        });
    }

    fn perform(self: &Rc<Self>, action: MenuAction) {
        match action {
            MenuAction::ClearStatus(ids) => self.act(SidebarAction::ClearStatus(ids)),
            MenuAction::Rename(id) => self.begin_rename(id),
            MenuAction::CopyName(Some(name)) => self.list.clipboard().set_text(&name),
            MenuAction::CopyName(None) => {}
            MenuAction::Duplicate(id) => self.act(SidebarAction::Duplicate(id)),
            MenuAction::MoveTo { sessions, workspace } => self.act(SidebarAction::MoveSessions { sessions, workspace }),
            MenuAction::ToggleFlags(ids) => self.act(SidebarAction::ToggleFlags(ids)),
            MenuAction::Reveal(cwd) => crate::app::reveal_in_file_manager(std::path::Path::new(&cwd)),
            MenuAction::Close(ids) => self.confirm_close(ids),
            MenuAction::NewSession(workspace) => self.act(SidebarAction::NewSession { workspace }),
            MenuAction::OpenDirectory(workspace) => self.choose_directory(workspace),
            MenuAction::ToggleFocus(id) => self.act(SidebarAction::ToggleFocus(id)),
            MenuAction::FocusMembership { workspace, member } => {
                self.act(SidebarAction::SetFocusMembership { workspace, member });
            }
            MenuAction::DeleteWorkspace(id) => self.confirm_delete(id),
        }
    }

    fn footer_action(self: &Rc<Self>, action: FooterAction) {
        match action {
            FooterAction::NewWorkspace => self.act(SidebarAction::NewWorkspace),
            FooterAction::NewSession => self.act(SidebarAction::NewSessionInCurrent),
            FooterAction::OpenDirectory => {
                if let Some(workspace) = self.context.borrow().drop.current_workspace {
                    self.choose_directory(workspace);
                }
            }
            FooterAction::ToggleFocusFilter => self.act(SidebarAction::ToggleFocusFilter),
            FooterAction::ToggleFlaggedView => self.act(SidebarAction::ToggleFlaggedView),
        }
    }

    // MARK: - dialogs

    /// Close Session(s), confirmed first when the setting asks for it (upstream's `confirmCloseSession(s)`).
    fn confirm_close(&self, ids: Vec<Uuid>) {
        let context = self.context.borrow();
        if !context.confirm_close {
            drop(context);
            self.act(SidebarAction::CloseSessions(ids));
            return;
        }
        let (heading, body) = close_confirmation(&context, &ids);
        drop(context);
        let window = self.window;
        let poster = self.poster.clone();
        confirm(&self.list, &heading, &body, "Close", move || {
            state::post(&poster, move |state: &mut AppState| {
                state.sidebar_action(window, SidebarAction::CloseSessions(ids));
            });
        });
    }

    /// Delete Workspace, confirmed first while it holds sessions (upstream's `confirmDeleteWorkspace`).
    fn confirm_delete(&self, id: Uuid) {
        let context = self.context.borrow();
        let Some(facts) = context.menu.workspaces.iter().find(|facts| facts.id == id) else {
            return;
        };
        if facts.session_count == 0 {
            drop(context);
            self.act(SidebarAction::DeleteWorkspace(id));
            return;
        }
        let heading = format!("Delete “{}”?", facts.name);
        let body = delete_confirmation(facts.session_count, context.close_grace);
        drop(context);
        let window = self.window;
        let poster = self.poster.clone();
        confirm(&self.list, &heading, &body, "Delete", move || {
            state::post(&poster, move |state: &mut AppState| {
                state.sidebar_action(window, SidebarAction::DeleteWorkspace(id));
            });
        });
    }

    /// Open Directory…: a folder chooser, then a session there in `workspace`.
    fn choose_directory(&self, workspace: Uuid) {
        let dialog = gtk::FileDialog::builder()
            .title("Choose a directory for the new session")
            .accept_label("Open")
            .modal(true)
            .build();
        if let Some(folder) = self.context.borrow().chooser_folder() {
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }
        let parent = self.list.root().and_downcast::<gtk::Window>();
        let window = self.window;
        let poster = self.poster.clone();
        dialog.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else {
                return;
            };
            let directories = vec![path.to_string_lossy().into_owned()];
            state::post(&poster, move |state: &mut AppState| {
                state.sidebar_action(window, SidebarAction::OpenDirectories { workspace, directories });
            });
        });
    }

    // MARK: - drag and drop

    fn spans(&self) -> Vec<RowSpan> {
        self.widgets
            .borrow()
            .iter()
            .map(|widget| {
                widget
                    .widget()
                    .compute_bounds(&self.list)
                    .map_or(RowSpan { top: -1.0, height: 0.0 }, |bounds| RowSpan {
                        top: f64::from(bounds.y()),
                        height: f64::from(bounds.height()),
                    })
            })
            .collect()
    }

    fn propose(&self, payload: &DragPayload, y: f64) -> Option<(Proposal, Highlight)> {
        let rows = self.rows.borrow();
        let spans = self.spans();
        let context = self.context.borrow();
        if !context.drop.tree {
            return None;
        }
        if payload.workspace {
            drag::propose_workspace(&rows, &spans, y, *payload.ids.first()?, &context.drop)
        } else {
            drag::propose_sessions(&rows, &spans, y, &payload.ids, &context.drop)
        }
    }

    fn highlight(&self, highlight: Option<Highlight>) {
        self.clear_highlight();
        let Some(highlight) = highlight else { return };
        let (index, class) = match highlight {
            Highlight::OnRow(index) => (index, None),
            Highlight::Above(index) => (index, Some("drop-above")),
            Highlight::Below(index) => (index, Some("drop-below")),
        };
        let widgets = self.widgets.borrow();
        let Some(list_row) = widgets.get(index).map(RowWidget::widget) else {
            return;
        };
        match class {
            None => self.list.drag_highlight_row(list_row),
            Some(class) => list_row.add_css_class(class),
        }
        self.drag.borrow_mut().highlighted.push(list_row.clone());
    }

    fn clear_highlight(&self) {
        self.list.drag_unhighlight_row();
        for row in self.drag.borrow_mut().highlighted.drain(..) {
            row.remove_css_class("drop-above");
            row.remove_css_class("drop-below");
        }
    }

    /// Opens the folded workspace under the pointer for the drag after a hover (view only), or forgets a pending open
    /// once the pointer left it.
    fn spring_load(self: &Rc<Self>, y: f64) {
        let spans = self.spans();
        let hovered = drag::row_at(&spans, y).and_then(|index| {
            let rows = self.rows.borrow();
            let row = rows.get(index)?;
            matches!(row.kind, RowKind::Workspace { expanded: false }).then_some(row.id)
        });
        let mut drag = self.drag.borrow_mut();
        if drag.spring_pending.as_ref().map(|(id, _)| *id) == hovered {
            return;
        }
        if let Some((_, source)) = drag.spring_pending.take() {
            source.remove();
        }
        let Some(id) = hovered.filter(|_| self.context.borrow().drop.tree) else {
            return;
        };
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(SPRING_DELAY, move || {
            let Some(shared) = weak.upgrade() else { return };
            let previous = shared.drag.borrow_mut().spring_opened(id);
            let window = shared.window;
            shared.post(move |state: &mut AppState| {
                if let Some(previous) = previous {
                    state.sidebar_spring_load(window, previous, false);
                }
                state.sidebar_spring_load(window, id, true);
            });
        });
        drag.spring_pending = Some((id, source));
    }

    /// Ends a drag over the list: the highlight goes, and a workspace the drag opened folds back unless `landed`.
    fn end_drag(&self, landed: bool) {
        self.clear_highlight();
        let mut drag = self.drag.borrow_mut();
        if let Some((_, source)) = drag.spring_pending.take() {
            source.remove();
        }
        let opened = drag.spring_open.take();
        drop(drag);
        if let Some(id) = opened.filter(|_| !landed) {
            let window = self.window;
            self.post(move |state: &mut AppState| state.sidebar_spring_load(window, id, false));
        }
    }
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

/// A destructive confirmation over `anchor`'s window; `confirmed` runs on the confirming response.
fn confirm(anchor: &gtk::ListBox, heading: &str, body: &str, verb: &str, confirmed: impl FnOnce() + 'static) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", verb)]);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    // answered from the response, never from `closed`, which libadwaita emits first
    dialog.choose(Some(anchor), None::<&gio::Cancellable>, move |response| {
        if response == "confirm" {
            confirmed();
        }
    });
}

/// "Too Many Folders" (upstream's `presentDirectoryDropLimitAlert`).
fn too_many_folders(anchor: &gtk::ListBox) {
    let dialog = adw::AlertDialog::new(
        Some("Too Many Folders"),
        Some(&format!(
            "You can open up to {} folders at once.",
            agterm_core::sidebar_drop::MAXIMUM_DIRECTORY_IMPORT_COUNT
        )),
    );
    dialog.add_response("ok", "OK");
    dialog.present(Some(anchor));
}

/// The list-wide handlers: the selection and the two drop targets.
fn connect_list(shared: &Rc<Shared>) {
    let weak = Rc::downgrade(shared);
    shared.list.connect_selected_rows_changed(move |_| {
        if let Some(shared) = weak.upgrade() {
            shared.selection_changed();
        }
    });
    connect_row_drops(shared);
    connect_folder_drops(shared);
}

fn connect_row_drops(shared: &Rc<Shared>) {
    let target = gtk::DropTarget::new(DragPayload::static_type(), gdk::DragAction::MOVE);
    target.set_preload(true);
    let weak = Rc::downgrade(shared);
    target.connect_motion(move |target, _, y| {
        let Some(shared) = weak.upgrade() else {
            return gdk::DragAction::empty();
        };
        let payload = target.value().and_then(|value| value.get::<DragPayload>().ok());
        let proposal = payload.as_ref().and_then(|payload| shared.propose(payload, y));
        shared.highlight(proposal.as_ref().map(|(_, highlight)| *highlight));
        if payload.is_some_and(|payload| !payload.workspace) {
            shared.spring_load(y);
        }
        if proposal.is_some() {
            gdk::DragAction::MOVE
        } else {
            gdk::DragAction::empty()
        }
    });
    let weak = Rc::downgrade(shared);
    target.connect_leave(move |_| {
        if let Some(shared) = weak.upgrade() {
            shared.end_drag(false);
        }
    });
    let weak = Rc::downgrade(shared);
    target.connect_drop(move |_, value, _, y| {
        let Some(shared) = weak.upgrade() else { return false };
        let proposal = value
            .get::<DragPayload>()
            .ok()
            .and_then(|payload| shared.propose(&payload, y));
        let action = match proposal {
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
                shared.end_drag(false);
                return false;
            }
        };
        shared.end_drag(true);
        shared.act(action);
        true
    });
    shared.list.add_controller(target);
}

fn connect_folder_drops(shared: &Rc<Shared>) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let propose = |shared: &Shared, y: f64| {
        let rows = shared.rows.borrow();
        drag::propose_directories(&rows, &shared.spans(), y, &shared.context.borrow().drop)
    };
    let weak = Rc::downgrade(shared);
    target.connect_motion(move |_, _, y| {
        let Some(shared) = weak.upgrade() else {
            return gdk::DragAction::empty();
        };
        let proposal = propose(&shared, y);
        shared.highlight(proposal.as_ref().and_then(|(_, highlight)| *highlight));
        shared.spring_load(y);
        if proposal.is_some() {
            gdk::DragAction::COPY
        } else {
            gdk::DragAction::empty()
        }
    });
    let weak = Rc::downgrade(shared);
    target.connect_leave(move |_| {
        if let Some(shared) = weak.upgrade() {
            shared.end_drag(false);
        }
    });
    let weak = Rc::downgrade(shared);
    target.connect_drop(move |_, value, _, y| {
        let Some(shared) = weak.upgrade() else { return false };
        let Some((Proposal::Directories { workspace }, _)) = propose(&shared, y) else {
            shared.end_drag(false);
            return false;
        };
        let files = value
            .get::<gdk::FileList>()
            .map(|list| {
                list.files()
                    .into_iter()
                    .filter_map(|file| file.path())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        match drag::directories(files) {
            Ok(directories) if !directories.is_empty() => {
                shared.end_drag(true);
                shared.act(SidebarAction::OpenDirectories { workspace, directories });
                true
            }
            Ok(_) => {
                shared.end_drag(false);
                false
            }
            Err(()) => {
                shared.end_drag(false);
                too_many_folders(&shared.list);
                false
            }
        }
    });
    shared.list.add_controller(target);
}

/// The list row a controller added to it sits on.
fn controller_row(controller: &impl IsA<gtk::EventController>) -> Option<gtk::ListBoxRow> {
    controller.widget().and_downcast::<gtk::ListBoxRow>()
}

/// One row's handlers: clicks (select, toggle, rename, menu), the workspace row's arrow, "+" and hover, the rename
/// field, and the drag source.
///
/// RULE: a handler owned by the row (its controllers, its children's signals) never holds the row strongly: that
/// cycle would keep every row set a rebuild drops alive. It reaches the row through the controller, or a weak ref.
fn wire_row(shared: &Rc<Shared>, widget: &RowWidget, workspace: bool) {
    let list_row = widget.widget();
    let click = gtk::GestureClick::builder().button(0).build();
    let weak = Rc::downgrade(shared);
    click.connect_pressed(move |gesture, presses, x, y| {
        let Some(shared) = weak.upgrade() else { return };
        let Some(pressed_row) = controller_row(gesture) else {
            return;
        };
        let Some(row) = shared.row_of(&pressed_row) else { return };
        on_row_pressed(&shared, gesture, &pressed_row, &row, (presses, x, y));
    });
    list_row.add_controller(click);
    if workspace {
        wire_workspace_controls(shared, widget);
    }
    wire_rename_field(shared, widget);
    list_row.add_controller(drag_source(shared));
}

fn on_row_pressed(
    shared: &Rc<Shared>,
    gesture: &gtk::GestureClick,
    list_row: &gtk::ListBoxRow,
    row: &SidebarRow,
    (presses, x, y): (i32, f64, f64),
) {
    if shared.editing.get().is_some_and(|editing| editing.id == row.id) {
        return;
    }
    match gesture.current_button() {
        gdk::BUTTON_SECONDARY => {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            shared.context_menu(list_row, row, x, y);
        }
        gdk::BUTTON_PRIMARY if presses >= 2 => {
            shared.cancel_toggle();
            shared.begin_rename(row.id);
        }
        gdk::BUTTON_PRIMARY if row.is_workspace() => shared.schedule_toggle(row.id),
        gdk::BUTTON_PRIMARY => shared.pressed.set(Some(row.id)),
        _ => {}
    }
}

fn wire_workspace_controls(shared: &Rc<Shared>, widget: &RowWidget) {
    let list_row = widget.widget();
    if let Some(arrow) = widget.arrow() {
        let weak = Rc::downgrade(shared);
        let row = list_row.downgrade();
        arrow.connect_clicked(move |_| {
            let (Some(shared), Some(row)) = (weak.upgrade(), row.upgrade()) else {
                return;
            };
            shared.cancel_toggle();
            if let Some(id) = shared.row_of(&row).map(|row| row.id) {
                shared.toggle(id);
            }
        });
    }
    let Some(add) = widget.add_button() else { return };
    let weak = Rc::downgrade(shared);
    let row = list_row.downgrade();
    add.connect_clicked(move |_| {
        let (Some(shared), Some(row)) = (weak.upgrade(), row.upgrade()) else {
            return;
        };
        shared.cancel_toggle();
        if let Some(id) = shared.row_of(&row).map(|row| row.id) {
            shared.act(SidebarAction::NewSession { workspace: id });
        }
    });
    let hover = gtk::EventControllerMotion::new();
    let weak = Rc::downgrade(shared);
    let shown = add.clone();
    hover.connect_enter(move |_, _, _| {
        // the hover "+" is an interface element, read at hover time
        if weak
            .upgrade()
            .is_some_and(|shared| shared.context.borrow().row_add_session)
        {
            shown.set_visible(true);
        }
    });
    let hidden = add.clone();
    hover.connect_leave(move |_| hidden.set_visible(false));
    list_row.add_controller(hover);
}

fn wire_rename_field(shared: &Rc<Shared>, widget: &RowWidget) {
    let entry = widget.entry();
    let weak = Rc::downgrade(shared);
    entry.connect_activate(move |_| {
        if let Some(shared) = weak.upgrade() {
            shared.finish_rename(true);
        }
    });
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(shared);
    keys.connect_key_pressed(move |_, key, _, _| {
        if key != gdk::Key::Escape {
            return glib::Propagation::Proceed;
        }
        if let Some(shared) = weak.upgrade() {
            shared.finish_rename(false);
        }
        glib::Propagation::Stop
    });
    entry.add_controller(keys);
    let focus = gtk::EventControllerFocus::new();
    let weak = Rc::downgrade(shared);
    focus.connect_leave(move |_| {
        if let Some(shared) = weak.upgrade() {
            shared.finish_rename(true);
        }
    });
    entry.add_controller(focus);
}

/// The row's drag source: the tree only (the flagged view is a projection, not a reorderable tree), never mid-rename.
fn drag_source(shared: &Rc<Shared>) -> gtk::DragSource {
    let source = gtk::DragSource::builder().actions(gdk::DragAction::MOVE).build();
    let weak: Weak<Shared> = Rc::downgrade(shared);
    source.connect_prepare(move |source, _, _| {
        let shared = weak.upgrade()?;
        if shared.editing.get().is_some() {
            return None;
        }
        let dragged = shared.row_of(&controller_row(source)?)?;
        let context = shared.context.borrow();
        if !context.drop.tree {
            return None;
        }
        let payload = if dragged.is_workspace() {
            DragPayload {
                workspace: true,
                ids: vec![dragged.id],
            }
        } else {
            DragPayload {
                workspace: false,
                ids: context::dragged_sessions(&context, dragged.id),
            }
        };
        Some(gdk::ContentProvider::for_value(&payload.to_value()))
    });
    source.connect_drag_begin(|source, _| {
        let paintable = gtk::WidgetPaintable::new(controller_row(source).as_ref());
        source.set_icon(Some(&paintable), 0, 0);
    });
    let weak = Rc::downgrade(shared);
    source.connect_drag_end(move |_, _, _| {
        // a landed drop already ended the drag; anything else folds a spring-opened workspace back
        if let Some(shared) = weak.upgrade() {
            shared.end_drag(false);
        }
    });
    source
}

/// The sidebar's stylesheet, installed once per display: the badge capsule, the drop lines, compact row buttons.
fn install_css() {
    thread_local! {
        static INSTALLED: Cell<bool> = const { Cell::new(false) };
    }
    if INSTALLED.with(|installed| installed.replace(true)) {
        return;
    }
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        ".agterm-sidebar .agterm-badge { background-color: @destructive_bg_color; color: @destructive_fg_color; \
         border-radius: 999px; min-width: 16px; min-height: 16px; padding: 0 4px; font-size: 0.8em; \
         font-weight: bold; }\n\
         .agterm-sidebar row.drop-above { box-shadow: inset 0 2px @accent_color; }\n\
         .agterm-sidebar row.drop-below { box-shadow: inset 0 -2px @accent_color; }\n\
         .agterm-sidebar button.agterm-disclosure, .agterm-sidebar button.agterm-row-add { min-width: 16px; \
         min-height: 16px; padding: 0; }\n\
         .agterm-sidebar .agterm-footer button { min-width: 24px; min-height: 22px; padding: 2px; }",
    );
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

/// The rows' text size from the `sidebarFontSize` setting (9...20, 13 the default), relative to the desktop's own
/// font: the setting is a density knob, so its default leaves the platform size alone.
fn apply_font_size(size: Option<f64>) {
    thread_local! {
        static APPLIED: RefCell<Option<(gtk::CssProvider, Option<u32>)>> = const { RefCell::new(None) };
    }
    let Some(display) = gdk::Display::default() else { return };
    // whole-tenths of a point: comparing floats exactly would reload on noise
    let tenths = size.map(|size| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 9...20 first"
        )]
        let tenths = (size.clamp(9.0, 20.0) * 10.0).round() as u32;
        tenths
    });
    APPLIED.with(|applied| {
        let mut applied = applied.borrow_mut();
        if applied.as_ref().is_some_and(|(_, current)| *current == tenths) {
            return;
        }
        let provider = applied.take().map_or_else(
            || {
                let provider = gtk::CssProvider::new();
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
                provider
            },
            |(provider, _)| provider,
        );
        let css = tenths.map_or_else(String::new, |tenths| {
            format!(
                ".agterm-sidebar list row {{ font-size: {:.3}em; }}",
                f64::from(tenths) / 130.0
            )
        });
        provider.load_from_string(&css);
        *applied = Some((provider, tenths));
    });
}

#[cfg(test)]
mod tests {
    use agterm_core::model::{NewSession, SidebarMode, WindowLibrary};

    use super::*;
    use crate::gtk_test::on_gtk_thread;

    /// The list's rows in order: `w:` workspaces, `s:` sessions, `*` selected.
    fn names(sidebar: &Sidebar) -> Vec<String> {
        let rows = sidebar.shared.rows.borrow();
        let widgets = sidebar.shared.widgets.borrow();
        assert_eq!(rows.len(), widgets.len());
        rows.iter()
            .zip(widgets.iter())
            .map(|(row, widget)| {
                let mark = if widget.widget().is_selected() { "*" } else { "" };
                let kind = if row.is_workspace() { "w" } else { "s" };
                format!("{kind}:{}{mark}", row.label)
            })
            .collect()
    }

    #[test]
    fn the_list_follows_the_model_in_place_and_rebuilds_on_a_reshape() {
        let shown = on_gtk_thread(|| {
            let mut library = WindowLibrary::seeded("/home/u");
            let window = library.active_window_id().unwrap();
            let store = library.store_mut(window).unwrap();
            let one = store.workspaces()[0].id();
            let srv = store.add_session(one, NewSession::at_cwd("/srv")).unwrap();
            let settings = AppSettings::default();
            let mut sidebar = Sidebar::new(window, &state::detached_poster());
            sidebar.refresh(store, &settings);
            let first = names(&sidebar);
            let built = sidebar.shared.widgets.borrow()[2].widget().clone();

            store.rename_session(srv, "build");
            sidebar.refresh(store, &settings);
            let renamed = names(&sidebar);
            let kept = sidebar.shared.widgets.borrow()[2].widget() == &built;

            store.set_flag(true, srv);
            store.set_sidebar_mode(SidebarMode::Flagged);
            sidebar.refresh(store, &settings);
            let flagged = names(&sidebar);
            let footer = sidebar.shared.context.borrow().footer;
            (
                first,
                renamed,
                kept,
                flagged,
                footer.flagged_mode,
                sidebar.empty.is_visible(),
            )
        });
        let Some((first, renamed, kept, flagged, flagged_mode, empty)) = shown else {
            return;
        };
        assert_eq!(first, ["w:workspace 1", "s:u", "s:srv*"]);
        assert_eq!(renamed, ["w:workspace 1", "s:u", "s:build*"]);
        assert!(kept, "a content change updates the row in place");
        assert_eq!(flagged, ["s:build : workspace 1*"]);
        assert!(flagged_mode && !empty);
    }

    #[test]
    fn a_rebuild_releases_the_rows_it_replaces() {
        let released = on_gtk_thread(|| {
            let mut library = WindowLibrary::seeded("/home/u");
            let window = library.active_window_id().unwrap();
            let store = library.store_mut(window).unwrap();
            let one = store.workspaces()[0].id();
            let settings = AppSettings::default();
            let mut sidebar = Sidebar::new(window, &state::detached_poster());
            sidebar.refresh(store, &settings);
            // the workspace row carries the arrow and "+" handlers, the session row the click and drag ones
            let old: Vec<glib::WeakRef<gtk::ListBoxRow>> = sidebar
                .shared
                .widgets
                .borrow()
                .iter()
                .map(|widget| widget.widget().downgrade())
                .collect();
            store.add_session(one, NewSession::at_cwd("/srv")).unwrap();
            sidebar.refresh(store, &settings);
            let rebuilt = sidebar.shared.widgets.borrow().len();
            (
                old.len(),
                rebuilt,
                old.iter().filter(|row| row.upgrade().is_some()).count(),
            )
        });
        let Some((old, rebuilt, alive)) = released else { return };
        assert_eq!((old, rebuilt), (2, 3));
        assert_eq!(
            alive, 0,
            "a dropped row set is finalized, not kept alive by its own handlers"
        );
    }

    #[test]
    fn a_row_that_changed_mid_rename_is_current_once_the_field_closes() {
        let shown = on_gtk_thread(|| {
            let mut library = WindowLibrary::seeded("/home/u");
            let window = library.active_window_id().unwrap();
            let store = library.store_mut(window).unwrap();
            let one = store.workspaces()[0].id();
            let srv = store.add_session(one, NewSession::at_cwd("/srv")).unwrap();
            let settings = AppSettings::default();
            let mut sidebar = Sidebar::new(window, &state::detached_poster());
            sidebar.refresh(store, &settings);
            sidebar.shared.begin_rename(srv);
            store.rename_session(srv, "build");
            sidebar.refresh(store, &settings);
            sidebar.shared.finish_rename(false);
            let name = sidebar.shared.widgets.borrow()[2].shown_name();
            // the next refresh finds nothing changed
            sidebar.refresh(store, &settings);
            (name, sidebar.shared.widgets.borrow()[2].shown_name())
        });
        let Some((closed, refreshed)) = shown else { return };
        assert_eq!(closed, "build", "the cancelled field reveals the current name");
        assert_eq!(refreshed, "build");
    }

    #[test]
    fn a_new_spring_load_folds_the_workspace_an_earlier_one_opened() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut drag = DragState::default();
        assert_eq!(drag.spring_opened(a), None);
        assert_eq!(drag.spring_opened(a), None, "the same workspace stays open");
        assert_eq!(drag.spring_opened(b), Some(a));
        assert_eq!(drag.spring_open, Some(b), "the drag now folds only b back");
    }

    #[test]
    fn confirmations_use_upstreams_words() {
        let mut context = SidebarContext {
            close_grace: true,
            ..SidebarContext::default()
        };
        let id = Uuid::new_v4();
        context.menu.sessions.insert(
            id,
            menu::SessionFacts {
                name: "build".into(),
                workspace: Uuid::nil(),
                flagged: false,
                has_status: false,
                cwd: "/".into(),
            },
        );
        assert_eq!(
            close_confirmation(&context, &[id]),
            (
                "Close “build”?".to_owned(),
                "The session will close after a short undo window.".to_owned()
            )
        );
        context.close_grace = false;
        assert_eq!(
            close_confirmation(&context, &[id, Uuid::nil()]),
            (
                "Close 2 Sessions?".to_owned(),
                "The sessions will close immediately and can be reopened from File > Open Recent.".to_owned()
            )
        );
        assert_eq!(
            delete_confirmation(1, true),
            "This closes its session after a short undo window."
        );
        assert_eq!(
            delete_confirmation(3, false),
            "This closes 3 sessions immediately. You can reopen it from File > Open Recent."
        );
    }
}
