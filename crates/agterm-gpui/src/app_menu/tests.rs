//! The app menu's model (titles, chords, enablement, the live parts, Help and the app's items), what its items run
//! through the core, and the `keymap.list` read-back.

use agterm_core::keymap::{BuiltinAction, linux_defaults, parse_keymap};
use agterm_core::menu_lists::MenuListAction;
use agterm_core::model::WindowLibrary;
use agterm_core::palette_catalog::{PaletteCommand, PaletteContext};
use agterm_core::persist::AppSettings;
use agterm_core::{Core, CoreConfig, CoreState};
use uuid::Uuid;

use super::*;

fn core_with(library: WindowLibrary) -> (Core, Uuid) {
    let window = library.active_window_id().unwrap();
    let core = Core::new(
        CoreConfig {
            app: agterm_proto::AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings: AppSettings::default(),
            persistence: None,
        },
    );
    (core, window)
}

fn seeded() -> (Core, Uuid) {
    core_with(WindowLibrary::seeded("/home/u"))
}

/// Every title of `entries` and their submenus, in order, a submenu's title before its entries.
fn titles(entries: &[Entry], into: &mut Vec<String>) {
    for entry in entries {
        match entry {
            Entry::Item(item) => into.push(item.title.clone()),
            Entry::Submenu { title, entries } => {
                into.push(title.clone());
                titles(entries, into);
            }
            Entry::Label(label) => into.push(label.clone()),
            Entry::Separator => {}
        }
    }
}

fn all_titles(menu: &AppMenu) -> Vec<String> {
    let mut all = Vec::new();
    for (title, entries) in &menu.menus {
        all.push(title.clone());
        titles(entries, &mut all);
    }
    all.extend(menu.app.iter().map(|item| item.title.clone()));
    all
}

fn find<'a>(menu: &'a AppMenu, id: &str) -> &'a MenuItem {
    menu.items()
        .into_iter()
        .map(|(_, item)| item)
        .find(|item| item.id == id)
        .unwrap_or_else(|| panic!("no item {id}"))
}

#[test]
fn the_menus_come_in_upstreams_order_with_the_app_items_last() {
    let (core, window) = seeded();
    let menu = AppMenu::of(&core, window);
    let tops: Vec<&str> = menu.menus.iter().map(|(title, _)| title.as_str()).collect();
    assert_eq!(tops, ["File", "View", "Navigate", "Help"]);
    let app: Vec<&str> = menu.app.iter().map(|item| item.title.as_str()).collect();
    assert_eq!(app, ["Settings…", "Quit"]);
    assert_eq!(find(&menu, "settings").action, MenuAction::App(AppAction::Settings));
    assert_eq!(find(&menu, "quit").action, MenuAction::App(AppAction::Quit));
}

#[test]
fn reset_live_sessions_sits_between_settings_and_quit_only_while_live_runs() {
    let (core, window) = seeded();
    assert!(
        AppMenu::of(&core, window)
            .items()
            .iter()
            .all(|(_, item)| item.id != RESET_LIVE_SESSIONS),
        "outside Live"
    );
    let dir = tempfile::tempdir().unwrap();
    let core = crate::zmx_runtime::tests::live_core(dir.path());
    let window = core.library().active_window_id().unwrap();
    let menu = AppMenu::of(&core, window);
    let app: Vec<&str> = menu.app.iter().map(|item| item.title.as_str()).collect();
    assert_eq!(app, ["Settings…", "Reset Live Sessions…", "Quit"]);
    let item = find(&menu, RESET_LIVE_SESSIONS);
    assert_eq!(item.action, MenuAction::App(AppAction::ResetLiveSessions));
    assert!(item.enabled);
    assert_eq!(item.chord, None, "no keymap action runs it");
}

#[test]
fn every_built_in_action_has_a_menu_item() {
    let items: Vec<Item> = MENUS
        .iter()
        .flat_map(|(_, sections)| sections.iter().flat_map(|section| section.iter().copied()))
        .collect();
    for action in BuiltinAction::all() {
        assert!(
            items.iter().any(|item| item.builtin() == Some(action)),
            "{} has no menu item",
            action.name()
        );
    }
}

#[test]
fn file_carries_edit_keymap_and_hooks_beside_their_reloads() {
    let (core, window) = seeded();
    let all = all_titles(&AppMenu::of(&core, window));
    let position = |label: &str| {
        all.iter()
            .position(|item| item == label)
            .unwrap_or_else(|| panic!("{label} missing: {all:?}"))
    };
    assert_eq!(position("Reload Keymap"), position("Edit Keymap…") + 1);
    assert_eq!(position("Reload Hooks"), position("Edit Hooks…") + 1);
    assert!(position("Edit ghostty.conf…") < position("View"));
    let edit = find(&AppMenu::of(&core, window), PaletteCommand::EditKeymap.id()).clone();
    assert!(edit.enabled);
    assert_eq!(edit.action, MenuAction::Command(Item::Row(PaletteCommand::EditKeymap)));
}

#[test]
fn file_carries_the_live_lists_and_help_the_docs_installers_and_about() {
    let mut library = WindowLibrary::seeded("/home/u");
    let closed = library.new_window(Some("work"));
    library.close_window(closed);
    let (core, window) = core_with(library);
    let menu = AppMenu::of(&core, window);
    let all = all_titles(&menu);
    let position = |label: &str| {
        all.iter()
            .position(|item| item == label)
            .unwrap_or_else(|| panic!("{label} missing: {all:?}"))
    };
    assert_eq!(position("Open Window"), position("New Window") + 1);
    assert_eq!(
        position("work"),
        position("Open Window") + 2,
        "the seeded window, then the closed one"
    );
    assert!(position("Open Recent") < position("No Recent Items"));
    assert_eq!(position("Reveal in Files"), position("Duplicate Session") + 1);
    assert!(position("Help") < position("Developer Documentation…"));
    assert!(position("Developer Documentation…") < position("Install Command Line Tool…"));
    assert!(position("Install Agent Status Hooks…") < position("Install Agent Skill…"));
    assert!(position("Install Agent Skill…") < position("About Agterm"));
    let work = find(&menu, &format!("window-{closed}"));
    assert!(!work.checked && work.enabled, "a closed window is listed unchecked");
    assert_eq!(work.action, MenuAction::List(MenuListAction::OpenWindow(closed)));
    assert!(
        find(&menu, &format!("window-{window}")).checked,
        "the open one is checked"
    );
    assert!(!find(&menu, "no-recent").enabled, "No Recent Items is inert");
}

#[test]
fn a_closed_session_is_listed_under_open_recent_with_clear_menu() {
    let (mut core, window) = seeded();
    let response = core.dispatch(&agterm_proto::Request {
        cmd: agterm_proto::Command::Session(agterm_proto::command::SessionCmd::New),
        target: None,
        args: Some(agterm_proto::Args {
            name: Some("doomed".to_owned()),
            ..agterm_proto::Args::default()
        }),
    });
    assert!(response.ok, "{response:?}");
    let id = response.result.unwrap()["id"].as_str().unwrap().to_owned();
    let closed = core.dispatch(&agterm_proto::Request {
        cmd: agterm_proto::Command::Session(agterm_proto::command::SessionCmd::Close),
        target: Some(id),
        args: None,
    });
    assert!(closed.ok, "{closed:?}");
    let _ = core.commit();
    let menu = AppMenu::of(&core, window);
    let all = all_titles(&menu);
    let sessions = all
        .iter()
        .position(|label| label == "Sessions")
        .expect("a Sessions heading");
    assert!(all[sessions + 1].starts_with("doomed"), "{all:?}");
    assert!(all.iter().any(|label| label == "Clear Menu"));
    assert!(!all.iter().any(|label| label == "No Recent Items"));
}

#[test]
fn the_toggles_name_what_they_will_do() {
    let item = Item::Row(PaletteCommand::FocusWorkspace);
    let context = PaletteContext::default();
    assert_eq!(item.title(context, Toggles::default()), "Focus Workspace");
    let sole = Toggles {
        sole_focus: true,
        ..Toggles::default()
    };
    assert_eq!(item.title(context, sole), "Unfocus Workspace");
    let (core, window) = seeded();
    let menu = AppMenu::of(&core, window);
    assert_eq!(find(&menu, PaletteCommand::ToggleSidebar.id()).title, "Hide Sidebar");
    assert_eq!(find(&menu, PaletteCommand::ToggleScratch.id()).title, "Show Scratch");
    assert_eq!(find(&menu, PaletteCommand::ResetFontSize.id()).title, "Actual Size");
}

#[test]
fn items_carry_their_live_chords_and_reopen_carries_none() {
    let (mut core, window) = seeded();
    let (keymap, _) = parse_keymap("", &linux_defaults());
    let expected = keymap.equivalent(BuiltinAction::NewSession).cloned();
    assert!(expected.is_some(), "new session has a default chord");
    let menu = AppMenu::of(&core, window);
    assert_eq!(find(&menu, PaletteCommand::NewSession.id()).chord, expected);
    assert_eq!(find(&menu, PaletteCommand::UndoClose.id()).chord, None);
    // a keymap override shows at once
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("config")).unwrap();
    std::fs::write(
        dir.path().join("config/keymap.conf"),
        "map ctrl+shift+f12 new_session\n",
    )
    .unwrap();
    let env = agterm_core::paths::MapEnv::new().with("AGTERM_STATE_DIR", dir.path().as_os_str());
    core.set_config_locator(agterm_core::paths::ConfigLocator::capture(&env));
    core.reload_keymap();
    let menu = AppMenu::of(&core, window);
    let chord = find(&menu, PaletteCommand::NewSession.id()).chord.clone().unwrap();
    assert_eq!(chord.display_string(), "ctrl+shift+f12");
}

#[test]
fn the_keymap_read_back_lists_the_keyed_items_with_their_menu() {
    let (core, window) = seeded();
    let listed = AppMenu::of(&core, window).keymap_items();
    assert!(!listed.is_empty());
    let new_session = listed
        .iter()
        .find(|item| item.selector.as_deref() == Some("menu.menu-newSession"))
        .expect("New Session is keyed");
    assert_eq!(new_session.menu, "File");
    assert_eq!(new_session.title, "New Session");
    assert_eq!(new_session.enabled, None, "enabled reads as absent");
    // every listed item reads its enablement off the menu, a disabled one as false
    let menu = AppMenu::of(&core, window);
    for item in &listed {
        let id = item.selector.as_deref().unwrap().strip_prefix("menu.menu-").unwrap();
        assert_eq!(item.enabled, (!find(&menu, id).enabled).then_some(false), "{id}");
    }
    assert!(
        listed.iter().any(|item| item.enabled == Some(false)),
        "one window and no split leave keyed items disabled: {listed:?}"
    );
    assert!(
        listed
            .iter()
            .all(|item| !item.menu.is_empty() && !item.chord.is_empty())
    );
}

#[test]
fn delete_window_disables_with_the_last_window_and_a_cover_disables_the_rest() {
    let (mut core, window) = seeded();
    let menu = AppMenu::of(&core, window);
    assert!(
        !find(&menu, BuiltinAction::DeleteWindow.name()).enabled,
        "one window is always kept"
    );
    let response = core.dispatch(&agterm_proto::Request {
        cmd: agterm_proto::Command::Dashboard,
        target: None,
        args: Some(agterm_proto::Args {
            mru: Some(true),
            ..agterm_proto::Args::default()
        }),
    });
    assert!(response.ok, "{response:?}");
    let menu = AppMenu::of(&core, window);
    assert!(
        !find(&menu, PaletteCommand::NewSession.id()).enabled,
        "the grid covers the window"
    );
    assert!(
        find(&menu, PaletteCommand::Dashboard.id()).enabled,
        "its own escape hatch"
    );
    assert!(
        !find(&menu, &format!("window-{window}")).enabled,
        "Open Window stands down under a cover"
    );
}

#[test]
fn a_chosen_item_runs_through_the_core_in_its_window() {
    let (core, window) = seeded();
    let mut state = AppState::new(core);
    let before = state.core.library().store(window).unwrap().sessions().count();
    state.menu_activated(window, Item::Row(PaletteCommand::NewSession));
    assert_eq!(
        state.core.library().store(window).unwrap().sessions().count(),
        before + 1
    );
    // a built-in with a frontend half leaves it to the landing
    state.menu_activated(window, Item::Builtin(BuiltinAction::RenameWindow));
    let work = state.take_frontend_work();
    assert_eq!(work.actions.len(), 1, "{:?}", work.actions);
    // a live entry: Open Window raises the open one
    state.menu_list_activated(window, MenuListAction::OpenWindow(window));
    assert!(state.core.library().is_open(window));
}

#[test]
fn reveal_in_files_reaches_the_frontend() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_str().unwrap();
    let (core, window) = core_with(WindowLibrary::seeded(home));
    let mut state = AppState::new(core);
    assert!(state.core.can_reveal_active_session(window));
    state.menu_list_activated(window, MenuListAction::RevealSession);
    let work = state.take_frontend_work();
    assert_eq!(work.reveals, [dir.path().to_path_buf()]);
}

#[test]
fn about_names_the_build_and_links_the_website() {
    let alert = about();
    assert_eq!(alert.heading, "Agterm");
    assert!(
        alert
            .body
            .starts_with(&format!("Version {}", env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(alert.default_button(), Some(0));
    assert_eq!(alert.buttons[ABOUT_WEBSITE].id.as_deref(), Some("about-website"));
}
