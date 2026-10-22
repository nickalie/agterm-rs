//! What the sidebar's own controls do to a window (upstream's `WorkspaceSidebar` coordinator handlers and the
//! `AppActions` they call: `WorkspaceSidebar+ContextMenu.swift`, `WorkspaceSidebar+DragDrop.swift`,
//! `AppActions+Batch.swift`, `AppActions+WorkspaceFocus.swift`, the footer in `WindowContentView.swift`).
//!
//! The frontend turns a click, a menu choice or a drop into a [`SidebarAction`] for the window it happened in and
//! hands it to [`Core::sidebar_action`]; what needs a dialog (close and delete confirmations, the directory chooser),
//! the clipboard or the inline rename field stays in the frontend. Every action but a committed rename stands down
//! while the window's modal slot is held, as the rest of the window's chrome does.

use uuid::Uuid;

use crate::Core;
use crate::model::{AgentIndicator, AppStore, NewSession, NewSessionPlacement, SidebarMode};
use crate::persist::settings::NewSessionPlacementSetting;

/// One sidebar gesture, already resolved to ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarAction {
    /// A session-row click or a change of the multi-selection: `active` becomes the selected session, `selection`
    /// the transient multi-selection in visible order (upstream's `outlineViewSelectionDidChange`).
    SelectSessions {
        active: Uuid,
        selection: Vec<Uuid>,
    },
    /// The multi-selection changed with no session to make active.
    SetSelection(Vec<Uuid>),
    /// A user open or fold of a workspace row, persisted (the outline's expand and collapse callbacks).
    SetWorkspaceExpanded {
        workspace: Uuid,
        expanded: bool,
    },
    /// An inline rename committed.
    RenameSession {
        session: Uuid,
        name: String,
    },
    RenameWorkspace {
        workspace: Uuid,
        name: String,
    },
    /// Row menu: Duplicate Session.
    Duplicate(Uuid),
    /// Row menu: Move to.
    MoveSessions {
        sessions: Vec<Uuid>,
        workspace: Uuid,
    },
    /// Row menu: Flag/Unflag (one value for the batch: flag all unless all are flagged).
    ToggleFlags(Vec<Uuid>),
    /// Row menu: Clear Status/Clear Statuses.
    ClearStatus(Vec<Uuid>),
    /// Row menu: Close (confirmed by the frontend when the setting asks for it).
    CloseSessions(Vec<Uuid>),
    /// Workspace row menu and its hover "+": New Session in that workspace.
    NewSession {
        workspace: Uuid,
    },
    /// Open Directory… (row menu or footer) and a folder dropped on the sidebar: sessions at `directories`,
    /// appended, the last one selected.
    OpenDirectories {
        workspace: Uuid,
        directories: Vec<String>,
    },
    /// Workspace row menu: Delete Workspace (confirmed by the frontend while it holds sessions).
    DeleteWorkspace(Uuid),
    /// Workspace row menu: Focus/Unfocus.
    ToggleFocus(Uuid),
    /// Workspace row menu: Add to Focus/Remove from Focus.
    SetFocusMembership {
        workspace: Uuid,
        member: bool,
    },
    /// A session drag landing: the block moves to `workspace` at the post-removal `destination`.
    DropSessions {
        sessions: Vec<Uuid>,
        workspace: Uuid,
        destination: usize,
    },
    /// A workspace drag landing at the post-removal `destination`.
    DropWorkspace {
        workspace: Uuid,
        destination: usize,
    },
    /// Footer: New Workspace.
    NewWorkspace,
    /// Footer: New Session in the current workspace.
    NewSessionInCurrent,
    /// Footer: the focus filter toggle.
    ToggleFocusFilter,
    /// Footer: the flagged view toggle.
    ToggleFlaggedView,
    /// Gives the selected session's terminal the keyboard back after a sidebar interaction took it (upstream's
    /// `focusActiveTerminal` after a rename).
    FocusActiveTerminal,
}

impl Core {
    /// Performs `action` in window `window`'s sidebar. False when it did nothing: the window is gone or not open, its
    /// modal slot is held (except for a committed rename), or the action changed nothing it could. Commit afterwards.
    pub fn sidebar_action(&mut self, window: Uuid, action: SidebarAction) -> bool {
        // a picker or ask arriving mid-rename takes the field's keyboard, and that focus loss is what commits it: the
        // rename still lands, as upstream's `controlTextDidEndEditing` renames without the modal gate
        let rename = matches!(
            action,
            SidebarAction::RenameSession { .. } | SidebarAction::RenameWorkspace { .. }
        );
        if (!rename && self.modal_pending(window)) || !self.library().is_open(window) {
            return false;
        }
        match action {
            SidebarAction::SelectSessions { active, selection } => self.select_from_sidebar(window, active, selection),
            SidebarAction::NewSession { workspace } => self.new_session_in(window, workspace),
            SidebarAction::NewSessionInCurrent => {
                let current = self.library().store(window).and_then(AppStore::current_workspace_id);
                current.is_some_and(|workspace| self.new_session_in(window, workspace))
            }
            SidebarAction::OpenDirectories { workspace, directories } => {
                self.open_directories(window, workspace, &directories)
            }
            SidebarAction::FocusActiveTerminal => {
                let selected = self.library().store(window).and_then(AppStore::selected_session_id);
                selected.is_some_and(|id| {
                    self.request_pane_focus(id);
                    true
                })
            }
            SidebarAction::Duplicate(session) => {
                let home = self.library().home().to_owned();
                let created = self
                    .library_mut()
                    .store_mut(window)
                    .and_then(|store| store.duplicate_session(session, &home));
                created.is_some_and(|id| self.created_by_user(id))
            }
            other => {
                let grace = self.settings().close_grace_undo_enabled.unwrap_or(true);
                self.library_mut()
                    .store_mut(window)
                    .is_some_and(|store| store_action(store, other, grace))
            }
        }
    }

    /// A row click: selects as user activity, then lands on the pane a blocked or completed status tagged.
    fn select_from_sidebar(&mut self, window: Uuid, active: Uuid, selection: Vec<Uuid>) -> bool {
        let Some(store) = self.library_mut().store_mut(window) else {
            return false;
        };
        if store.session(active).is_none() {
            return false;
        }
        let indicator = store.select_session_in_sidebar(Some(active), selection);
        self.note_user_activity(active);
        self.reveal_blocked_pane(active, indicator.as_ref());
        true
    }

    /// New Session in `workspace` (upstream's `addNewSession`): the new-session directory and placement settings.
    fn new_session_in(&mut self, window: Uuid, workspace: Uuid) -> bool {
        let home = self.library().home().to_owned();
        let settings = self.settings();
        let placement = match settings.effective_new_session_placement() {
            NewSessionPlacementSetting::End => NewSessionPlacement::End,
            NewSessionPlacementSetting::AfterCurrent => NewSessionPlacement::AfterCurrent,
        };
        let Some(store) = self.library().store(window) else {
            return false;
        };
        let current = store
            .active_session()
            .map(|session| session.local_working_directory(session.focused_cwd(), &home));
        let cwd = settings.resolve_new_session_cwd(current.as_deref(), &home);
        let at = store.new_session_insertion_index(workspace, placement);
        let created = self.library_mut().store_mut(window).and_then(|store| {
            store.add_session(
                workspace,
                NewSession {
                    at,
                    ..NewSession::at_cwd(cwd)
                },
            )
        });
        created.is_some_and(|id| self.created_by_user(id))
    }

    /// Sessions at each directory in `workspace`, appended (Open Directory… and a folder drop).
    fn open_directories(&mut self, window: Uuid, workspace: Uuid, directories: &[String]) -> bool {
        let Some(store) = self.library_mut().store_mut(window) else {
            return false;
        };
        let created = directories
            .iter()
            .filter_map(|directory| store.add_session(workspace, NewSession::at_cwd(directory.clone())))
            .last();
        created.is_some_and(|id| self.created_by_user(id))
    }

    /// A session the user just created and the store selected: user activity, and its terminal takes the keyboard.
    fn created_by_user(&mut self, session: Uuid) -> bool {
        self.note_user_activity(session);
        self.request_pane_focus(session);
        true
    }
}

/// The actions that touch the window's store alone; `grace` is the close-grace undo setting.
fn store_action(store: &mut AppStore, action: SidebarAction, grace: bool) -> bool {
    match action {
        SidebarAction::SetSelection(ids) => store.set_sidebar_selection(ids),
        SidebarAction::SetWorkspaceExpanded { workspace, expanded } => {
            store.set_workspace_expanded(workspace, expanded);
        }
        SidebarAction::RenameSession { session, name } => store.rename_session(session, &name),
        SidebarAction::RenameWorkspace { workspace, name } => store.rename_workspace(workspace, &name),
        SidebarAction::MoveSessions { sessions, workspace } => {
            return store.move_sessions(&sessions, workspace, None) > 0;
        }
        SidebarAction::DropSessions {
            sessions,
            workspace,
            destination,
        } => return store.move_sessions(&sessions, workspace, Some(destination)) > 0,
        SidebarAction::DropWorkspace { workspace, destination } => store.move_workspace(workspace, destination),
        SidebarAction::ToggleFlags(ids) => {
            let ids: Vec<Uuid> = ids.into_iter().filter(|id| store.session(*id).is_some()).collect();
            if ids.is_empty() {
                return false;
            }
            let all_flagged = ids.iter().all(|id| store.session(*id).is_some_and(|s| s.flagged));
            store.set_flags(!all_flagged, &ids);
        }
        SidebarAction::ClearStatus(ids) => {
            for id in ids {
                store.set_agent_indicator(AgentIndicator::default(), id);
            }
        }
        SidebarAction::CloseSessions(ids) => return close_sessions(store, &ids, grace),
        SidebarAction::DeleteWorkspace(id) => return delete_workspace(store, id, grace),
        SidebarAction::ToggleFocus(id) => store.toggle_focused_workspace(id),
        SidebarAction::SetFocusMembership { workspace, member } => store.set_focus_membership(workspace, member),
        SidebarAction::NewWorkspace => {
            let name = store.default_workspace_name();
            store.add_workspace(&name);
        }
        SidebarAction::ToggleFocusFilter => {
            let enabled = store.focus().enabled();
            store.set_focus_enabled(!enabled);
        }
        SidebarAction::ToggleFlaggedView => {
            let mode = match store.sidebar().mode {
                SidebarMode::Tree => SidebarMode::Flagged,
                SidebarMode::Flagged => SidebarMode::Tree,
            };
            store.set_sidebar_mode(mode);
        }
        SidebarAction::SelectSessions { .. }
        | SidebarAction::NewSession { .. }
        | SidebarAction::NewSessionInCurrent
        | SidebarAction::OpenDirectories { .. }
        | SidebarAction::Duplicate(_)
        | SidebarAction::FocusActiveTerminal => return false,
    }
    true
}

/// Close one or more sessions as the row menu does (upstream's `closeSessions` after its confirmation): soft, with
/// an undo window, while close-grace undo is on (`grace`), else for good.
fn close_sessions(store: &mut AppStore, ids: &[Uuid], grace: bool) -> bool {
    let ids: Vec<Uuid> = ids.iter().copied().filter(|id| store.session(*id).is_some()).collect();
    if !grace {
        return ids.iter().filter(|id| store.close_session(**id).is_some()).count() > 0;
    }
    match ids.as_slice() {
        [] => false,
        [id] => store.soft_close_session(*id),
        _ => store.soft_close_sessions(&ids),
    }
}

/// Delete Workspace after its confirmation: soft while close-grace undo is on (`grace`), else for good; one
/// workspace is always kept.
fn delete_workspace(store: &mut AppStore, id: Uuid, grace: bool) -> bool {
    if !store.can_remove_workspace() || store.workspace(id).is_none() {
        return false;
    }
    if grace {
        store.soft_remove_workspace(id)
    } else {
        store.remove_workspace(id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use ngterm_proto::command::PickCmd;
    use ngterm_proto::{AppIdentity, Args, Command, PickItem, Request};

    use super::*;
    use crate::model::{WindowLibrary, Workspace};
    use crate::persist::AppSettings;
    use crate::{CoreConfig, CoreState};

    /// One open window: workspace "one" with three sessions (the last selected) and an empty "two".
    struct Fixture {
        core: Core,
        window: Uuid,
        workspaces: [Uuid; 2],
        sessions: [Uuid; 3],
    }

    fn fixture(settings: AppSettings) -> Fixture {
        let mut library = WindowLibrary::new("/home/u");
        let window = library.new_window(None);
        let store = library.store_mut(window).unwrap();
        let one = store.workspaces()[0].id();
        let first = store.sessions().next().unwrap().id();
        let second = store.add_session(one, NewSession::at_cwd("/srv")).unwrap();
        let third = store.add_session(one, NewSession::at_cwd("/var")).unwrap();
        let two = store.add_workspace("two");
        store.select_session(Some(third));
        let core = Core::new(
            CoreConfig {
                app: AppIdentity::from_recorded("0.1.0", None),
                capabilities: Vec::new(),
            },
            CoreState {
                library,
                settings,
                persistence: None,
            },
        );
        Fixture {
            core,
            window,
            workspaces: [one, two],
            sessions: [first, second, third],
        }
    }

    impl Fixture {
        fn act(&mut self, action: SidebarAction) -> bool {
            self.core.sidebar_action(self.window, action)
        }

        fn store(&self) -> &AppStore {
            self.core.library().store(self.window).unwrap()
        }

        fn store_mut(&mut self) -> &mut AppStore {
            self.core.library_mut().store_mut(self.window).unwrap()
        }

        fn order(&self, workspace: Uuid) -> Vec<Uuid> {
            self.store()
                .workspace(workspace)
                .map(|w| w.sessions.iter().map(crate::model::Session::id).collect())
                .unwrap_or_default()
        }
    }

    #[test]
    fn a_row_click_selects_the_active_session_with_the_multi_selection() {
        let mut f = fixture(AppSettings::default());
        let [first, second, _] = f.sessions;
        assert!(f.act(SidebarAction::SelectSessions {
            active: first,
            selection: vec![first, second],
        }));
        assert_eq!(f.store().selected_session_id(), Some(first));
        assert_eq!(f.store().sidebar_selection_ids(), [first, second]);
        assert!(!f.act(SidebarAction::SelectSessions {
            active: Uuid::new_v4(),
            selection: Vec::new(),
        }));
    }

    #[test]
    fn every_action_but_a_committed_rename_stands_down_while_the_modal_slot_is_held() {
        let mut f = fixture(AppSettings::default());
        let picked = f.core.dispatch(&Request {
            cmd: Command::Pick(PickCmd::Open),
            target: None,
            args: Some(Args {
                items: Some(vec![PickItem {
                    id: "a".into(),
                    label: "A".into(),
                    subtitle: None,
                }]),
                ..Args::default()
            }),
        });
        assert!(picked.ok, "{picked:?}");
        assert!(!f.act(SidebarAction::NewWorkspace));
        assert_eq!(f.store().workspaces().len(), 2);
        // the modal taking the rename field's keyboard commits it: the rename is not lost
        let [first, ..] = f.sessions;
        let [_, two] = f.workspaces;
        f.act(SidebarAction::RenameSession {
            session: first,
            name: "build".into(),
        });
        f.act(SidebarAction::RenameWorkspace {
            workspace: two,
            name: "ops".into(),
        });
        assert_eq!(f.store().session(first).unwrap().display_name(), "build");
        assert_eq!(f.store().workspace(two).unwrap().name, "ops");
    }

    #[test]
    fn flags_toggle_as_one_value_for_the_batch() {
        let mut f = fixture(AppSettings::default());
        let [first, second, _] = f.sessions;
        f.store_mut().set_flag(true, first);
        assert!(f.act(SidebarAction::ToggleFlags(vec![first, second])));
        let flagged = |f: &Fixture, id| f.store().session(id).unwrap().flagged;
        assert!(flagged(&f, first) && flagged(&f, second));
        f.act(SidebarAction::ToggleFlags(vec![first, second]));
        assert!(!flagged(&f, first) && !flagged(&f, second));
        assert!(!f.act(SidebarAction::ToggleFlags(vec![Uuid::new_v4()])));
    }

    #[test]
    fn clear_status_resets_every_target_to_idle() {
        let mut f = fixture(AppSettings::default());
        let [first, second, _] = f.sessions;
        for id in [first, second] {
            let indicator = AgentIndicator {
                status: crate::model::AgentStatus::Blocked,
                ..AgentIndicator::default()
            };
            f.store_mut().set_agent_indicator(indicator, id);
        }
        f.act(SidebarAction::ClearStatus(vec![first, second]));
        for id in [first, second] {
            assert_eq!(
                f.store().session(id).unwrap().agent_indicator,
                AgentIndicator::default()
            );
        }
    }

    #[test]
    fn close_is_soft_while_grace_undo_is_on_and_final_otherwise() {
        let mut f = fixture(AppSettings::default());
        let [first, second, _] = f.sessions;
        assert!(f.act(SidebarAction::CloseSessions(vec![first, second])));
        assert!(f.store().session(first).is_none());
        assert!(f.store().pending_close_session(first).is_some());

        let mut f = fixture(AppSettings {
            close_grace_undo_enabled: Some(false),
            ..AppSettings::default()
        });
        let [first, ..] = f.sessions;
        assert!(f.act(SidebarAction::CloseSessions(vec![first])));
        assert!(f.store().session(first).is_none());
        assert!(f.store().pending_close_session(first).is_none());
    }

    #[test]
    fn delete_keeps_the_last_workspace() {
        let mut f = fixture(AppSettings {
            close_grace_undo_enabled: Some(false),
            ..AppSettings::default()
        });
        let [one, two] = f.workspaces;
        assert!(f.act(SidebarAction::DeleteWorkspace(two)));
        assert!(!f.act(SidebarAction::DeleteWorkspace(one)));
        let ids: Vec<Uuid> = f.store().workspaces().iter().map(Workspace::id).collect();
        assert_eq!(ids, [one]);
    }

    #[test]
    fn new_session_follows_the_placement_setting() {
        let mut f = fixture(AppSettings {
            new_session_placement: Some("afterCurrent".into()),
            ..AppSettings::default()
        });
        let [one, _] = f.workspaces;
        let [first, ..] = f.sessions;
        f.store_mut().select_session(Some(first));
        assert!(f.act(SidebarAction::NewSession { workspace: one }));
        let created = f.store().selected_session_id().unwrap();
        assert_eq!(f.order(one)[1], created);
        assert_eq!(f.store().session(created).unwrap().effective_cwd(), "/home/u");

        let before = f.order(one).len();
        assert!(f.act(SidebarAction::NewSessionInCurrent));
        assert_eq!(f.order(one).len(), before + 1);
    }

    #[test]
    fn open_directories_appends_each_and_selects_the_last() {
        let mut f = fixture(AppSettings::default());
        let [_, two] = f.workspaces;
        assert!(f.act(SidebarAction::OpenDirectories {
            workspace: two,
            directories: vec!["/tmp".into(), "/opt".into()],
        }));
        let order = f.order(two);
        assert_eq!(order.len(), 2);
        assert_eq!(f.store().selected_session_id(), order.last().copied());
        assert_eq!(f.store().session(order[1]).unwrap().effective_cwd(), "/opt");
        assert!(!f.act(SidebarAction::OpenDirectories {
            workspace: Uuid::new_v4(),
            directories: vec!["/tmp".into()],
        }));
    }

    #[test]
    fn drops_move_sessions_and_workspaces() {
        let mut f = fixture(AppSettings::default());
        let [one, two] = f.workspaces;
        let [first, second, third] = f.sessions;
        assert!(f.act(SidebarAction::DropSessions {
            sessions: vec![third],
            workspace: one,
            destination: 0,
        }));
        assert_eq!(f.order(one), [third, first, second]);
        assert!(f.act(SidebarAction::MoveSessions {
            sessions: vec![first, second],
            workspace: two,
        }));
        assert_eq!(f.order(two), [first, second]);
        assert!(f.act(SidebarAction::DropWorkspace {
            workspace: two,
            destination: 0,
        }));
        assert_eq!(f.store().workspaces()[0].id(), two);
    }

    #[test]
    fn footer_toggles_flip_the_filter_and_the_view() {
        let mut f = fixture(AppSettings::default());
        let [one, _] = f.workspaces;
        let [first, ..] = f.sessions;
        f.act(SidebarAction::SetFocusMembership {
            workspace: one,
            member: true,
        });
        assert!(!f.store().focus().enabled());
        f.act(SidebarAction::ToggleFocusFilter);
        assert!(f.store().focus().enabled());
        f.act(SidebarAction::ToggleFocus(one));
        assert!(!f.store().focus().enabled(), "unfocus the sole applied member");

        f.store_mut().set_flag(true, first);
        f.act(SidebarAction::ToggleFlaggedView);
        assert_eq!(f.store().sidebar().mode, SidebarMode::Flagged);
        f.act(SidebarAction::ToggleFlaggedView);
        assert_eq!(f.store().sidebar().mode, SidebarMode::Tree);
        assert!(f.act(SidebarAction::NewWorkspace));
        assert_eq!(f.store().workspaces().len(), 3);
    }

    #[test]
    fn renames_expansion_and_duplicate_reach_the_store() {
        let mut f = fixture(AppSettings::default());
        let [one, two] = f.workspaces;
        let [first, ..] = f.sessions;
        f.act(SidebarAction::RenameSession {
            session: first,
            name: "build".into(),
        });
        f.act(SidebarAction::RenameWorkspace {
            workspace: two,
            name: "ops".into(),
        });
        f.act(SidebarAction::SetWorkspaceExpanded {
            workspace: one,
            expanded: false,
        });
        assert_eq!(f.store().session(first).unwrap().display_name(), "build");
        assert_eq!(f.store().workspace(two).unwrap().name, "ops");
        assert!(!f.store().workspace(one).unwrap().is_expanded);
        assert!(f.act(SidebarAction::Duplicate(first)));
        assert_eq!(f.order(one)[1], f.store().selected_session_id().unwrap());
    }
}
