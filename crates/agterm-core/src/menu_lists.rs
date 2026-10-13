//! The app menu's live parts (upstream's `agtermApp+Menus.swift` File menu): Open Window, listing the library with
//! the open windows checked; Open Recent, the closed sessions and workspaces with Clear Menu; and Reveal in Files for
//! the active session. Host-free, so the entries and what choosing one does are tested without a menu.

use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Command, Request};
use uuid::Uuid;

use crate::model::{AppStore, Session};
use crate::pane_env::uuid_string;
use crate::persist::{RecentClosedItem, RecentClosedKind};
use crate::{Core, WindowRequest};

/// One Open Window entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowEntry {
    pub id: Uuid,
    pub name: String,
    /// Open windows carry a check mark; choosing one raises it.
    pub open: bool,
}

/// One Open Recent entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentEntry {
    pub id: Uuid,
    /// "title - subtitle" (upstream's `recentTitle`).
    pub title: String,
}

/// Open Recent's two sections, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentEntries {
    pub sessions: Vec<RecentEntry>,
    pub workspaces: Vec<RecentEntry>,
}

impl RecentEntries {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty() && self.workspaces.is_empty()
    }
}

/// A live menu entry chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuListAction {
    /// Open a closed window, or raise an open one.
    OpenWindow(Uuid),
    /// Reopen an Open Recent entry.
    OpenRecent(Uuid),
    /// Open Recent > Clear Menu.
    ClearRecent,
    /// Reveal the active session's directory in the file manager.
    RevealSession,
}

/// An entry's menu title.
fn recent_title(item: &RecentClosedItem) -> String {
    match item.subtitle.as_deref().filter(|subtitle| !subtitle.is_empty()) {
        Some(subtitle) => format!("{} - {subtitle}", item.title),
        None => item.title.clone(),
    }
}

impl Core {
    /// Open Window's entries, in library order.
    #[must_use]
    pub fn window_entries(&self) -> Vec<WindowEntry> {
        let library = self.library();
        library
            .windows()
            .iter()
            .map(|info| WindowEntry {
                id: info.id,
                name: info.name.clone(),
                open: library.is_open(info.id),
            })
            .collect()
    }

    /// Open Recent's entries.
    #[must_use]
    pub fn recent_entries(&self) -> RecentEntries {
        let mut entries = RecentEntries::default();
        for item in self.library().recent_closed_items() {
            let entry = RecentEntry {
                id: item.id,
                title: recent_title(&item),
            };
            match item.kind {
                RecentClosedKind::Session => entries.sessions.push(entry),
                RecentClosedKind::Workspace => entries.workspaces.push(entry),
            }
        }
        entries
    }

    /// Whether Reveal in Files can show window `window`'s active session: its focused directory exists.
    #[must_use]
    pub fn can_reveal_active_session(&self, window: Uuid) -> bool {
        let home = self.library().home();
        self.library()
            .store(window)
            .and_then(AppStore::active_session)
            .is_some_and(|session| crate::commands::session::existing_directory(session.focused_cwd(), home).is_some())
    }

    /// Runs a live menu entry chosen in window `window` (the frontmost). Opening and reopening stand down under a
    /// cover, as upstream's `uiActionsEnabled` gates them. Commit afterwards.
    pub fn perform_menu_list(&mut self, window: Uuid, action: MenuListAction) {
        match action {
            MenuListAction::OpenWindow(id) => {
                if !self.palette_context(window).modal_active() {
                    self.open_or_raise_window(id);
                }
            }
            MenuListAction::OpenRecent(item) => {
                if !self.ui_actions_enabled(window) {
                    return;
                }
                if let Some(owner) = self
                    .library_mut()
                    .reopen_recent_closed_reporting_window(item, Some(window))
                {
                    self.reveal_restored_window(owner);
                }
            }
            MenuListAction::ClearRecent => self.library_mut().clear_recent_closed_items(),
            MenuListAction::RevealSession => {
                let Some(session) = self
                    .library()
                    .store(window)
                    .and_then(AppStore::active_session)
                    .map(Session::id)
                else {
                    return;
                };
                let response = self.dispatch(&Request {
                    cmd: Command::Session(SessionCmd::Reveal),
                    target: Some(uuid_string(session)),
                    args: Some(Args {
                        window: Some(uuid_string(window)),
                        ..Args::default()
                    }),
                });
                if !response.ok {
                    tracing::info!(error = ?response.error, "reveal in files refused");
                }
            }
        }
    }

    /// Open Window's choice (upstream's `openWindow`): a closed window opens from its file, an open one comes forward.
    fn open_or_raise_window(&mut self, id: Uuid) {
        if !self.library().windows().iter().any(|info| info.id == id) {
            return;
        }
        if !self.library().is_open(id) {
            self.open_window(id);
        }
        if self.library().is_open(id) {
            self.reveal_restored_window(id);
        }
    }

    /// Brings window `id` forward with its active terminal focused (upstream's `revealRestoredWindow`).
    fn reveal_restored_window(&mut self, id: Uuid) {
        self.take_frontmost(id);
        self.request_window(WindowRequest::Raise(id));
        if let Some(session) = self
            .library()
            .store(id)
            .and_then(AppStore::active_session)
            .map(Session::id)
        {
            self.request_pane_focus(session);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use agterm_proto::AppIdentity;

    use super::*;
    use crate::model::WindowLibrary;
    use crate::persist::AppSettings;
    use crate::{CoreConfig, CoreState};

    fn core(library: WindowLibrary) -> Core {
        Core::new(
            CoreConfig {
                app: AppIdentity::from_recorded("0.1.0", None),
                capabilities: Vec::new(),
            },
            CoreState {
                library,
                settings: AppSettings::default(),
                persistence: None,
            },
        )
    }

    fn item(kind: RecentClosedKind, title: &str, subtitle: Option<&str>) -> RecentClosedItem {
        RecentClosedItem {
            id: Uuid::new_v4(),
            kind,
            title: title.to_owned(),
            subtitle: subtitle.map(str::to_owned),
            closed_at: SystemTime::now(),
            session: None,
            workspace: None,
        }
    }

    #[test]
    fn open_window_lists_the_library_with_the_open_ones_marked() {
        let mut library = WindowLibrary::seeded("/home/u");
        let first = library.active_window_id().unwrap();
        let second = library.new_window(Some("work"));
        library.close_window(second);
        let core = core(library);
        let entries = core.window_entries();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| entry.id == first && entry.open));
        assert!(
            entries
                .iter()
                .any(|entry| entry.id == second && !entry.open && entry.name == "work")
        );
    }

    #[test]
    fn open_recent_splits_sessions_from_workspaces_and_joins_the_subtitle() {
        let mut library = WindowLibrary::seeded("/home/u");
        library.set_recent_closed_items(vec![
            item(RecentClosedKind::Session, "build", Some("api")),
            item(RecentClosedKind::Workspace, "infra", Some("2 sessions")),
            item(RecentClosedKind::Session, "logs", Some("")),
        ]);
        let mut core = core(library);
        let entries = core.recent_entries();
        let titles = |list: &[RecentEntry]| list.iter().map(|entry| entry.title.clone()).collect::<Vec<_>>();
        assert_eq!(titles(&entries.sessions), ["build - api", "logs"]);
        assert_eq!(titles(&entries.workspaces), ["infra - 2 sessions"]);
        let window = core.library().active_window_id().unwrap();
        core.perform_menu_list(window, MenuListAction::ClearRecent);
        assert!(core.recent_entries().is_empty());
    }

    #[test]
    fn choosing_a_closed_window_opens_it_and_makes_it_frontmost() {
        let mut library = WindowLibrary::seeded("/home/u");
        let first = library.active_window_id().unwrap();
        let second = library.new_window(Some("work"));
        library.close_window(second);
        let mut core = core(library);
        core.perform_menu_list(first, MenuListAction::OpenWindow(second));
        assert!(core.library().is_open(second));
        assert_eq!(core.library().active_window_id(), Some(second));
        assert!(core.take_window_requests().contains(&WindowRequest::Raise(second)));
    }

    #[test]
    fn reveal_needs_an_existing_directory() {
        let library = WindowLibrary::seeded("/");
        let window = library.active_window_id().unwrap();
        let core_with_root = core(library);
        assert!(core_with_root.can_reveal_active_session(window), "/ exists");
        let mut library = WindowLibrary::seeded("/no/such/agterm/dir");
        let window = library.active_window_id().unwrap();
        let session = library.store(window).unwrap().active_session().unwrap().id();
        library
            .store_mut(window)
            .unwrap()
            .session_mut(session)
            .unwrap()
            .current_cwd = Some("/no/such/agterm/dir".to_owned());
        assert!(!core(library).can_reveal_active_session(window));
    }
}
