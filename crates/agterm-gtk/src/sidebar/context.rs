//! The snapshot of a window the sidebar's handlers read: a GTK handler never borrows the model (it posts a task), so
//! what a menu, a drop or a footer control needs to know is taken on each refresh.

use std::collections::HashMap;
use std::path::Path;

use agterm_core::model::{AgentStatus, AppStore, SidebarMode};
use agterm_core::persist::AppSettings;
use agterm_core::persist::settings::InterfaceElement;
use uuid::Uuid;

use super::drag::DropContext;
use super::menu::{MenuContext, SessionFacts, WorkspaceFacts};

/// What the footer shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags the footer reads one each, as upstream's view reads them"
)]
pub struct FooterState {
    pub new_workspace: bool,
    pub new_session: bool,
    pub focus_filter: bool,
    pub flagged_view: bool,
    /// The focus filter applies.
    pub filter_on: bool,
    /// The focus set has members (the filter toggle is usable).
    pub has_members: bool,
    /// The sidebar shows the flagged view.
    pub flagged_mode: bool,
    /// Some session is flagged.
    pub has_flagged: bool,
}

/// The window as the sidebar's handlers see it.
#[derive(Debug, Clone, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent settings and facts the handlers read one each"
)]
pub struct SidebarContext {
    pub menu: MenuContext,
    pub drop: DropContext,
    pub footer: FooterState,
    /// The selected session.
    pub active_session: Option<Uuid>,
    /// The active session's focused cwd, where Open Directory… starts.
    pub active_cwd: Option<String>,
    pub confirm_close: bool,
    pub close_grace: bool,
    /// A whole workspace-row click opens or folds it (`workspaceRowClickExpands`, default on).
    pub row_click_expands: bool,
    /// The workspace row's hover "+" is shown.
    pub row_add_session: bool,
    /// The flagged view shows and nothing is flagged: the empty-state hint.
    pub empty_flagged: bool,
}

impl SidebarContext {
    /// Takes the snapshot of `store` under `settings`.
    #[must_use]
    pub fn take(store: &AppStore, settings: &AppSettings) -> Self {
        let shows = |element| !settings.is_interface_element_hidden(element);
        let flagged_mode = store.sidebar().mode == SidebarMode::Flagged;
        let has_flagged = store.sessions().any(|session| session.flagged);
        Self {
            menu: menu_context(store),
            drop: drop_context(store),
            footer: FooterState {
                new_workspace: shows(InterfaceElement::NewWorkspace),
                new_session: shows(InterfaceElement::NewSession),
                focus_filter: shows(InterfaceElement::FocusFilter),
                flagged_view: shows(InterfaceElement::FlaggedView),
                filter_on: store.focus().enabled(),
                has_members: !store.focus().ids().is_empty(),
                flagged_mode,
                has_flagged,
            },
            active_session: store.selected_session_id(),
            active_cwd: store.active_session().map(|session| session.focused_cwd().to_owned()),
            confirm_close: settings.confirm_close_session.unwrap_or(false),
            close_grace: settings.close_grace_undo_enabled.unwrap_or(true),
            row_click_expands: settings.workspace_row_click_expands.unwrap_or(true),
            row_add_session: shows(InterfaceElement::WorkspaceAddSession),
            empty_flagged: flagged_mode && !has_flagged,
        }
    }

    /// A directory for a file chooser to start in: the active session's cwd when it exists here.
    #[must_use]
    pub fn chooser_folder(&self) -> Option<&str> {
        self.active_cwd.as_deref().filter(|cwd| Path::new(cwd).is_dir())
    }
}

fn menu_context(store: &AppStore) -> MenuContext {
    let mut sessions = HashMap::new();
    let mut workspaces = Vec::new();
    for workspace in store.workspaces() {
        for session in &workspace.sessions {
            sessions.insert(
                session.id(),
                SessionFacts {
                    name: session.display_name(),
                    workspace: workspace.id(),
                    flagged: session.flagged,
                    has_status: session.agent_indicator.status != AgentStatus::Idle,
                    cwd: session.focused_cwd().to_owned(),
                },
            );
        }
        workspaces.push(WorkspaceFacts {
            id: workspace.id(),
            name: workspace.name.clone(),
            session_count: workspace.sessions.len(),
            focus_member: store.focus().ids().contains(&workspace.id()),
            sole_focus: store.is_sole_focus(workspace.id()),
        });
    }
    MenuContext {
        selection: store.sidebar_selection_ids(),
        sessions,
        workspaces,
        can_remove_workspace: store.can_remove_workspace(),
    }
}

fn drop_context(store: &AppStore) -> DropContext {
    DropContext {
        locations: store
            .sessions()
            .filter_map(|session| Some((session.id(), store.session_location(session.id())?)))
            .collect(),
        workspaces: store
            .workspaces()
            .iter()
            .map(|workspace| (workspace.id(), workspace.sessions.len()))
            .collect(),
        tree: store.sidebar().mode == SidebarMode::Tree,
        sole_focus: store.sole_focused_workspace_id(),
        current_workspace: store.current_workspace_id(),
    }
}

/// The session ids of the rows a drag starting on session row `id` carries: the selection when the row is in it, else
/// the row alone (upstream's `pasteboardWriterForItem`).
#[must_use]
pub fn dragged_sessions(context: &SidebarContext, id: Uuid) -> Vec<Uuid> {
    context.menu.targets(id)
}

#[cfg(test)]
mod tests {
    use agterm_core::model::{NewSession, WindowLibrary};

    use super::*;

    #[test]
    fn the_snapshot_reads_the_footer_menu_and_drop_facts() {
        let mut library = WindowLibrary::seeded("/home/u");
        let window = library.active_window_id().unwrap();
        let store = library.store_mut(window).unwrap();
        let one = store.workspaces()[0].id();
        let first = store.sessions().next().unwrap().id();
        let second = store.add_session(one, NewSession::at_cwd("/srv")).unwrap();
        store.set_flag(true, second);
        store.set_focus_membership(one, true);
        let settings = AppSettings {
            hidden_interface_elements: Some(vec!["newWorkspace".into()]),
            ..AppSettings::default()
        };
        let context = SidebarContext::take(store, &settings);
        assert!(!context.footer.new_workspace && context.footer.new_session);
        assert!(context.footer.has_members && !context.footer.filter_on);
        assert!(context.footer.has_flagged && !context.empty_flagged);
        assert!(context.menu.sessions[&second].flagged);
        assert_eq!(context.menu.workspaces[0].session_count, 2);
        assert!(!context.menu.can_remove_workspace);
        assert_eq!(context.drop.locations[&second].index, 1);
        assert_eq!(context.drop.current_workspace, Some(one));
        assert_eq!(dragged_sessions(&context, first), [first]);
        assert!(context.row_click_expands && context.close_grace && !context.confirm_close);
    }
}
