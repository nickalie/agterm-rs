//! The row context menus (upstream's `WorkspaceSidebar+ContextMenu.swift` `menu(forRow:)`), as a pure model over a
//! snapshot of the window the sidebar took on its last sync (ported unchanged from the GTK build's `sidebar/menu.rs`),
//! and the gpui-kit popup menu drawn from it ([`build`]).

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{Context, Window};
use uuid::Uuid;

/// What a session row's menu reads of its session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    pub name: String,
    pub workspace: Uuid,
    pub flagged: bool,
    /// The agent status is not idle.
    pub has_status: bool,
    /// The focused pane's cwd, what Reveal in Files shows.
    pub cwd: String,
}

/// What a workspace row's menu reads of its workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceFacts {
    pub id: Uuid,
    pub name: String,
    pub session_count: usize,
    pub focus_member: bool,
    /// The only marked workspace, with the filter applied (upstream's `isSoleFocus`).
    pub sole_focus: bool,
}

/// The window as the menus and the sidebar's other handlers see it, taken on each refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MenuContext {
    /// The multi-selection in visible order (empty unless it holds the selected session).
    pub selection: Vec<Uuid>,
    pub sessions: HashMap<Uuid, SessionFacts>,
    /// Every workspace in tree order.
    pub workspaces: Vec<WorkspaceFacts>,
    pub can_remove_workspace: bool,
}

impl MenuContext {
    /// What a session row's menu acts on: the selection when the clicked row is in it, else the clicked row (upstream's
    /// `sidebarSelectionTargets(forContextSession:)` for a rendered row).
    #[must_use]
    pub fn targets(&self, clicked: Uuid) -> Vec<Uuid> {
        if self.selection.contains(&clicked) {
            self.selection.clone()
        } else {
            vec![clicked]
        }
    }

    fn workspace(&self, id: Uuid) -> Option<&WorkspaceFacts> {
        self.workspaces.iter().find(|workspace| workspace.id == id)
    }
}

/// What a menu item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    ClearStatus(Vec<Uuid>),
    /// Start the inline rename of a row.
    Rename(Uuid),
    /// Copy a name to the clipboard; `None` (a blank workspace name) leaves the clipboard alone.
    CopyName(Option<String>),
    Duplicate(Uuid),
    MoveTo {
        sessions: Vec<Uuid>,
        workspace: Uuid,
    },
    ToggleFlags(Vec<Uuid>),
    /// Reveal a directory in the file manager.
    Reveal(String),
    Close(Vec<Uuid>),
    NewSession(Uuid),
    OpenDirectory(Uuid),
    ToggleFocus(Uuid),
    FocusMembership {
        workspace: Uuid,
        member: bool,
    },
    DeleteWorkspace(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub label: String,
    pub action: MenuAction,
    pub enabled: bool,
}

impl MenuItem {
    fn new(label: impl Into<String>, action: MenuAction) -> Self {
        Self {
            label: label.into(),
            action,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    Item(MenuItem),
    Submenu { label: String, items: Vec<MenuItem> },
    Separator,
}

/// A session row's menu: Clear Status first while a target has a status, Rename and Copy Name and Duplicate Session
/// for one target, Move to the workspaces the targets are not all in, Flag or Unflag, Reveal for one target, Close.
#[must_use]
pub fn session_menu(context: &MenuContext, clicked: Uuid, reveal_enabled: impl Fn(&str) -> bool) -> Vec<MenuEntry> {
    let targets = context.targets(clicked);
    let facts: Vec<&SessionFacts> = targets.iter().filter_map(|id| context.sessions.get(id)).collect();
    let single = targets.len() == 1;
    let mut menu = Vec::new();
    if facts.iter().any(|facts| facts.has_status) {
        let label = if single { "Clear Status" } else { "Clear Statuses" };
        menu.push(MenuEntry::Item(MenuItem::new(
            label,
            MenuAction::ClearStatus(targets.clone()),
        )));
        menu.push(MenuEntry::Separator);
    }
    if targets.len() <= 1 {
        menu.push(MenuEntry::Item(MenuItem::new("Rename", MenuAction::Rename(clicked))));
    }
    if single {
        let name = context.sessions.get(&clicked).map(|facts| facts.name.clone());
        menu.push(MenuEntry::Item(MenuItem::new("Copy Name", MenuAction::CopyName(name))));
        menu.push(MenuEntry::Item(MenuItem::new(
            "Duplicate Session",
            MenuAction::Duplicate(clicked),
        )));
    }
    let destinations: Vec<MenuItem> = context
        .workspaces
        .iter()
        .filter(|workspace| facts.iter().any(|facts| facts.workspace != workspace.id))
        .map(|workspace| {
            MenuItem::new(
                workspace.name.clone(),
                MenuAction::MoveTo {
                    sessions: targets.clone(),
                    workspace: workspace.id,
                },
            )
        })
        .collect();
    if !destinations.is_empty() {
        menu.push(MenuEntry::Submenu {
            label: "Move to".to_owned(),
            items: destinations,
        });
    }
    let all_flagged = !facts.is_empty() && facts.iter().all(|facts| facts.flagged);
    let flag = match (single, all_flagged) {
        (true, true) => "Unflag",
        (true, false) => "Flag",
        (false, true) => "Unflag Sessions",
        (false, false) => "Flag Sessions",
    };
    menu.push(MenuEntry::Item(MenuItem::new(
        flag,
        MenuAction::ToggleFlags(targets.clone()),
    )));
    if single && let Some(facts) = context.sessions.get(&clicked) {
        menu.push(MenuEntry::Item(MenuItem {
            enabled: reveal_enabled(&facts.cwd),
            ..MenuItem::new("Reveal in Files", MenuAction::Reveal(facts.cwd.clone()))
        }));
    }
    let close = if single {
        "Close Session".to_owned()
    } else {
        format!("Close {} Sessions", targets.len())
    };
    menu.push(MenuEntry::Item(MenuItem::new(close, MenuAction::Close(targets))));
    menu
}

/// A workspace row's menu: Rename, Copy Name, the session creators, the focus pair, Delete Workspace (disabled at the
/// last workspace).
#[must_use]
pub fn workspace_menu(context: &MenuContext, id: Uuid) -> Vec<MenuEntry> {
    let facts = context.workspace(id);
    let name = facts
        .map(|facts| facts.name.trim().to_owned())
        .filter(|name| !name.is_empty());
    let focused = facts.is_some_and(|facts| facts.sole_focus);
    let member = facts.is_some_and(|facts| facts.focus_member);
    vec![
        MenuEntry::Item(MenuItem::new("Rename", MenuAction::Rename(id))),
        MenuEntry::Item(MenuItem::new("Copy Name", MenuAction::CopyName(name))),
        MenuEntry::Item(MenuItem::new("New Session", MenuAction::NewSession(id))),
        MenuEntry::Item(MenuItem::new("Open Directory…", MenuAction::OpenDirectory(id))),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem::new(
            if focused { "Unfocus" } else { "Focus" },
            MenuAction::ToggleFocus(id),
        )),
        MenuEntry::Item(MenuItem::new(
            if member { "Remove from Focus" } else { "Add to Focus" },
            MenuAction::FocusMembership {
                workspace: id,
                member: !member,
            },
        )),
        MenuEntry::Separator,
        MenuEntry::Item(MenuItem {
            enabled: context.can_remove_workspace,
            ..MenuItem::new("Delete Workspace", MenuAction::DeleteWorkspace(id))
        }),
    ]
}

/// What a menu item runs its action with.
pub type Perform = Rc<dyn Fn(MenuAction, &mut Window, &mut gpui_kit::App)>;

/// Fills `menu` with `entries`, each item running `perform` with its action; a disabled item shows but does nothing.
pub fn build(
    mut menu: PopupMenu,
    entries: Vec<MenuEntry>,
    perform: &Perform,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    for entry in entries {
        menu = match entry {
            MenuEntry::Separator => menu.separator(),
            MenuEntry::Item(item) => menu.item(item_of(item, perform)),
            MenuEntry::Submenu { label, items } => {
                let perform = Rc::clone(perform);
                menu.submenu(label, window, cx, move |mut submenu, _, _| {
                    for item in items.clone() {
                        submenu = submenu.item(item_of(item, &perform));
                    }
                    submenu
                })
            }
        };
    }
    menu
}

fn item_of(item: MenuItem, perform: &Perform) -> PopupMenuItem {
    let perform = Rc::clone(perform);
    let action = item.action;
    PopupMenuItem::new(item.label)
        .disabled(!item.enabled)
        .on_click(move |_, window, cx| perform(action.clone(), window, cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(entries: &[MenuEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                MenuEntry::Item(item) if item.enabled => item.label.clone(),
                MenuEntry::Item(item) => format!("({})", item.label),
                MenuEntry::Submenu { label, items } => format!("{label} [{}]", items.len()),
                MenuEntry::Separator => "-".to_owned(),
            })
            .collect()
    }

    /// Workspaces "one" (sessions a, b) and "two" (session c).
    fn context() -> (MenuContext, [Uuid; 2], [Uuid; 3]) {
        let [one, two] = [Uuid::new_v4(), Uuid::new_v4()];
        let [a, b, c] = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let facts = |name: &str, workspace| SessionFacts {
            name: name.to_owned(),
            workspace,
            flagged: false,
            has_status: false,
            cwd: "/tmp".to_owned(),
        };
        let workspace = |id, name: &str, count| WorkspaceFacts {
            id,
            name: name.to_owned(),
            session_count: count,
            focus_member: false,
            sole_focus: false,
        };
        let context = MenuContext {
            selection: Vec::new(),
            sessions: HashMap::from([(a, facts("a", one)), (b, facts("b", one)), (c, facts("c", two))]),
            workspaces: vec![workspace(one, "one", 2), workspace(two, "two", 1)],
            can_remove_workspace: true,
        };
        (context, [one, two], [a, b, c])
    }

    #[test]
    fn a_single_session_menu_offers_the_single_target_items() {
        let (context, [_, two], [a, ..]) = context();
        let menu = session_menu(&context, a, |_| true);
        assert_eq!(
            labels(&menu),
            [
                "Rename",
                "Copy Name",
                "Duplicate Session",
                "Move to [1]",
                "Flag",
                "Reveal in Files",
                "Close Session"
            ]
        );
        assert!(menu.contains(&MenuEntry::Submenu {
            label: "Move to".into(),
            items: vec![MenuItem::new(
                "two",
                MenuAction::MoveTo {
                    sessions: vec![a],
                    workspace: two
                }
            )],
        }));
        assert_eq!(labels(&session_menu(&context, a, |_| false))[5], "(Reveal in Files)");
    }

    #[test]
    fn a_multi_selection_menu_acts_on_every_target() {
        let (mut context, _, [a, b, c]) = context();
        context.selection = vec![a, c];
        context.sessions.get_mut(&a).unwrap().flagged = true;
        context.sessions.get_mut(&c).unwrap().has_status = true;
        let menu = session_menu(&context, c, |_| true);
        assert_eq!(
            labels(&menu),
            [
                "Clear Statuses",
                "-",
                "Move to [2]",
                "Flag Sessions",
                "Close 2 Sessions"
            ]
        );
        assert!(menu.contains(&MenuEntry::Item(MenuItem::new(
            "Close 2 Sessions",
            MenuAction::Close(vec![a, c])
        ))));
        // outside the selection the menu narrows to the clicked row
        assert_eq!(context.targets(b), [b]);
        context.sessions.get_mut(&c).unwrap().flagged = true;
        assert!(labels(&session_menu(&context, a, |_| true)).contains(&"Unflag Sessions".to_owned()));
    }

    #[test]
    fn a_workspace_menu_reflects_focus_and_the_last_workspace() {
        let (mut context, [one, _], _) = context();
        assert_eq!(
            labels(&workspace_menu(&context, one)),
            [
                "Rename",
                "Copy Name",
                "New Session",
                "Open Directory…",
                "-",
                "Focus",
                "Add to Focus",
                "-",
                "Delete Workspace"
            ]
        );
        context.workspaces[0].focus_member = true;
        context.workspaces[0].sole_focus = true;
        context.workspaces[0].name = "  ".into();
        context.can_remove_workspace = false;
        let menu = workspace_menu(&context, one);
        assert_eq!(
            labels(&menu)[5..],
            ["Unfocus", "Remove from Focus", "-", "(Delete Workspace)"]
        );
        assert_eq!(
            menu[1],
            MenuEntry::Item(MenuItem::new("Copy Name", MenuAction::CopyName(None)))
        );
    }
}
