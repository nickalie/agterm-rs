//! The app menu (upstream's `agtermApp+Menus.swift`, as a GTK menu): File, View and Navigate in each window's primary
//! menu, every item an app action named after its row, enabled by the frontmost window's palette context (the one
//! predicate the palette and the keymap read), and every keyed item showing its resolved chord as its accelerator.
//!
//! File also carries upstream's live parts ([`agterm_core::menu_lists`]): Open Window, listing the library with the open
//! windows checked; Open Recent, the closed sessions and workspaces; and Reveal in Files. A Help menu holds the
//! developer documentation, the agent integrations' installers and the About window, and the app's own Preferences,
//! Reset Live Sessions… (live sessions only) and Quit close the menu.
//!
//! The accelerators are display only: the keymap monitor (`keymap_dispatch`) dispatches the chords before any widget
//! sees them, from the same resolved keymap. `keymap.list` reads them back from this menu model (`menu_items`).

use std::cell::RefCell;

use agterm_core::Core;
use agterm_core::keymap::{BuiltinAction, Chord};
use agterm_core::menu_lists::{MenuListAction, RecentEntries, WindowEntry};
use agterm_core::palette_catalog::{PaletteCommand, PaletteContext};
use agterm_proto::ControlKeymapMenuItem;
use gtk::prelude::*;
use gtk::{gdk, gio};

use crate::state::{self, AppState, Poster};

/// One menu entry: the palette row it runs, or a built-in with no palette row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Row(PaletteCommand),
    Builtin(BuiltinAction),
}

/// The menus in upstream's order, sections separated.
const MENUS: [(&str, &[&[Item]]); 3] = [("File", FILE), ("View", VIEW), ("Navigate", NAVIGATE)];

const FILE: &[&[Item]] = &[
    &[
        Item::Builtin(BuiltinAction::NewWindow),
        Item::Builtin(BuiltinAction::RenameWindow),
        Item::Builtin(BuiltinAction::DeleteWindow),
    ],
    &[
        Item::Row(PaletteCommand::NewWorkspace),
        Item::Row(PaletteCommand::RenameWorkspace),
        Item::Row(PaletteCommand::DeleteWorkspace),
    ],
    &[
        Item::Row(PaletteCommand::NewSession),
        Item::Row(PaletteCommand::OpenDirectory),
        Item::Row(PaletteCommand::AttachRemote),
        Item::Row(PaletteCommand::ReopenRecent),
        Item::Row(PaletteCommand::RenameSession),
        Item::Row(PaletteCommand::DuplicateSession),
        Item::Row(PaletteCommand::CloseSession),
        Item::Row(PaletteCommand::UndoClose),
        Item::Row(PaletteCommand::ClearStatus),
    ],
    &[
        Item::Row(PaletteCommand::EditKeymap),
        Item::Row(PaletteCommand::ReloadKeymap),
        Item::Row(PaletteCommand::EditHooks),
        Item::Row(PaletteCommand::ReloadHooks),
        Item::Row(PaletteCommand::EditGhosttyConfig),
        Item::Row(PaletteCommand::ReloadConfig),
    ],
];

const VIEW: &[&[Item]] = &[
    &[
        Item::Row(PaletteCommand::IncreaseFontSize),
        Item::Row(PaletteCommand::DecreaseFontSize),
        Item::Row(PaletteCommand::ResetFontSize),
        Item::Row(PaletteCommand::SelectTheme),
    ],
    &[
        Item::Row(PaletteCommand::ToggleSidebar),
        Item::Row(PaletteCommand::ExpandWorkspaces),
        Item::Row(PaletteCommand::CollapseWorkspaces),
        Item::Row(PaletteCommand::ToggleWorkspaceCollapse),
        Item::Row(PaletteCommand::ToggleFlaggedView),
        Item::Row(PaletteCommand::ToggleFlag),
        Item::Row(PaletteCommand::ClearFlagged),
        Item::Row(PaletteCommand::FocusWorkspace),
        Item::Row(PaletteCommand::AddWorkspaceToFocus),
        Item::Row(PaletteCommand::ToggleWorkspaceFilter),
        Item::Row(PaletteCommand::ClearFocus),
        Item::Row(PaletteCommand::ToggleSplit),
        Item::Row(PaletteCommand::ToggleHorizontalSplit),
        Item::Row(PaletteCommand::SwapPanes),
        Item::Row(PaletteCommand::ToggleScratch),
        Item::Row(PaletteCommand::Find),
        Item::Row(PaletteCommand::QuickTerminal),
        Item::Row(PaletteCommand::ToggleTerminalZoom),
    ],
    // no AppKit-appended full screen item exists on GTK, so the action gets one of its own
    &[Item::Row(PaletteCommand::ToggleFullscreen)],
];

const NAVIGATE: &[&[Item]] = &[
    &[
        Item::Builtin(BuiltinAction::SessionPalette),
        Item::Builtin(BuiltinAction::CommandPalette),
        Item::Builtin(BuiltinAction::CustomCommandPalette),
        Item::Row(PaletteCommand::ShowAttention),
        Item::Row(PaletteCommand::Dashboard),
    ],
    &[
        Item::Row(PaletteCommand::PreviousSession),
        Item::Row(PaletteCommand::NextSession),
        Item::Row(PaletteCommand::PreviousAttentionSession),
        Item::Row(PaletteCommand::NextAttentionSession),
        Item::Row(PaletteCommand::FirstSession),
        Item::Row(PaletteCommand::LastSession),
        Item::Row(PaletteCommand::PreviousWorkspace),
        Item::Row(PaletteCommand::NextWorkspace),
        Item::Row(PaletteCommand::PreviousWindow),
        Item::Row(PaletteCommand::NextWindow),
    ],
    &[
        Item::Row(PaletteCommand::FocusLeftPane),
        Item::Row(PaletteCommand::FocusRightPane),
    ],
];

impl Item {
    /// The app action's name.
    fn action_name(self) -> String {
        match self {
            Self::Row(command) => format!("menu-{}", command.id()),
            Self::Builtin(action) => format!("menu-{}", action.name()),
        }
    }

    fn builtin(self) -> Option<BuiltinAction> {
        match self {
            Self::Row(command) => command.builtin_action(),
            Self::Builtin(action) => Some(action),
        }
    }

    /// The item's title as upstream's menu spells it in `context`.
    fn title(self, context: PaletteContext, toggles: Toggles) -> String {
        let title = match self {
            Self::Builtin(BuiltinAction::NewWindow) => "New Window",
            Self::Builtin(BuiltinAction::RenameWindow) => "Rename Window…",
            Self::Builtin(BuiltinAction::DeleteWindow) => "Delete Window",
            Self::Builtin(BuiltinAction::SessionPalette) => "Go to Session",
            Self::Builtin(BuiltinAction::CommandPalette) => "Command Palette",
            Self::Builtin(_) => "Custom Commands",
            Self::Row(PaletteCommand::ResetFontSize) => "Actual Size",
            Self::Row(PaletteCommand::EditKeymap) => "Edit Keymap…",
            Self::Row(PaletteCommand::EditHooks) => "Edit Hooks…",
            Self::Row(PaletteCommand::EditGhosttyConfig) => "Edit ghostty.conf…",
            Self::Row(PaletteCommand::ShowAttention) => "Go to Attention…",
            Self::Row(PaletteCommand::ToggleSidebar) => {
                if toggles.sidebar_shown {
                    "Hide Sidebar"
                } else {
                    "Show Sidebar"
                }
            }
            Self::Row(PaletteCommand::ToggleScratch) => {
                if toggles.scratch_shown {
                    "Hide Scratch"
                } else {
                    "Show Scratch"
                }
            }
            Self::Row(PaletteCommand::FocusWorkspace) if toggles.sole_focus => "Unfocus Workspace",
            Self::Row(command) => command.title(&context),
        };
        title.to_owned()
    }

    /// Whether the item can run in `context`.
    fn enabled(self, context: PaletteContext, can_remove_window: bool) -> bool {
        match self {
            Self::Row(command) => command.is_enabled(&context),
            Self::Builtin(BuiltinAction::DeleteWindow) => can_remove_window && !context.modal_active(),
            Self::Builtin(_) => !context.modal_active(),
        }
    }
}

/// The frontmost window's state the toggling items name: Hide/Show Sidebar and Scratch, and Focus Workspace reading
/// Unfocus Workspace while the current workspace is the sole focus (upstream's `isCurrentWorkspaceSoleFocus`).
#[derive(Debug, Clone, Copy, Default)]
struct Toggles {
    sidebar_shown: bool,
    scratch_shown: bool,
    sole_focus: bool,
}

/// An app item: its action name, its title and what it runs.
type AppItem = (&'static str, &'static str, fn(&mut AppState));

/// The app's own items closing the menu, after File, View, Navigate and Help: no keymap action runs them, so they carry
/// no accelerator (upstream's app menu Settings… and Quit). Reset Live Sessions… shares Quit's section, as upstream's
/// `CommandGroup(before: .appTermination)`, and is shown only while live sessions are both configured and running.
const APP_ITEMS: [AppItem; 3] = [
    ("preferences", "Preferences", |state| crate::settings::present(state)),
    (
        LIVE_RESET_ACTION,
        "Reset Live Sessions…",
        crate::zmx_runtime::reset_from_menu,
    ),
    ("quit", "Quit", |state| crate::welcome::request_quit(state)),
];

/// Reset Live Sessions…'s action.
const LIVE_RESET_ACTION: &str = "reset-live-sessions";

/// The Help menu after File, View and Navigate (upstream's `CommandGroup(replacing: .help)`), sections separated: the
/// developer documentation, then the installers. No keymap action runs them, so they carry no accelerator.
const HELP_ITEMS: [&[AppItem]; 2] = [
    &[("developer-docs", "Developer Documentation…", |state| {
        crate::integrations::ui::open_developer_docs(state.frontmost_gtk_window().as_ref());
    })],
    &[
        ("install-cli", "Install Command Line Tool…", |state| {
            crate::integrations::ui::run_cli(state.frontmost_gtk_window());
        }),
        ("install-hooks", "Install Agent Status Hooks…", |state| {
            crate::integrations::ui::run_hooks(state.frontmost_gtk_window());
        }),
        ("install-skill", "Install Agent Skill…", |state| {
            crate::integrations::ui::run_skill(state.frontmost_gtk_window());
        }),
    ],
];

/// The items a keymap shows accelerators on: upstream's menu carries none for Reopen Closed Item, which its own
/// monitor delivers so text undo keeps working.
fn shows_accelerator(item: Item) -> bool {
    item != Item::Row(PaletteCommand::UndoClose)
}

/// The live parts of the menu, rebuilt when they change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Lists {
    windows: Vec<WindowEntry>,
    recent: RecentEntries,
    /// Whether Reset Live Sessions… is shown.
    live_reset: bool,
}

/// What the model was built from: the static items' titles and accelerators, and the live lists.
type Built = (Vec<(String, Option<String>)>, Lists);

/// The menu model, its actions, and what it was last built from.
pub struct AppMenu {
    model: gio::Menu,
    group: gio::SimpleActionGroup,
    /// What the model was built with, so an unchanged state rebuilds nothing.
    built: RefCell<Built>,
    /// The Open Window entries' actions, one checkable action per window, replaced on a rebuild.
    window_actions: RefCell<Vec<String>>,
    poster: Poster,
}

impl AppMenu {
    /// The menu, with its actions posting through `poster`.
    #[must_use]
    pub fn new(poster: &Poster) -> Self {
        let group = gio::SimpleActionGroup::new();
        for item in all_items() {
            let action = gio::SimpleAction::new(&item.action_name(), None);
            let posting = poster.clone();
            action.connect_activate(move |_, _| {
                state::post(&posting, move |state: &mut AppState| {
                    state.menu_activated(item_command(item));
                });
            });
            group.add_action(&action);
        }
        for (name, _, run) in APP_ITEMS.iter().chain(HELP_ITEMS.iter().copied().flatten()).copied() {
            let action = gio::SimpleAction::new(name, None);
            let posting = poster.clone();
            action.connect_activate(move |_, _| state::post(&posting, run));
            group.add_action(&action);
        }
        add_list_actions(&group, poster);
        Self {
            model: gio::Menu::new(),
            group,
            built: RefCell::new(Built::default()),
            window_actions: RefCell::new(Vec::new()),
            poster: poster.clone(),
        }
    }

    /// The model the windows' menu buttons show.
    #[must_use]
    pub fn model(&self) -> &gio::Menu {
        &self.model
    }

    /// The action group the items activate, inserted into each window as `menu`.
    #[must_use]
    pub fn group(&self) -> &gio::SimpleActionGroup {
        &self.group
    }

    /// Brings titles, accelerators and enablement in line with the frontmost window.
    pub fn sync(&self, core: &Core) {
        let window = core.library().active_window_id();
        let context = window.map(|id| core.palette_context(id)).unwrap_or_default();
        let store = window.and_then(|id| core.library().store(id));
        let toggles = Toggles {
            sidebar_shown: store.is_none_or(|store| store.sidebar().visible),
            scratch_shown: store
                .and_then(|store| store.active_session())
                .is_some_and(|session| session.scratch_active),
            sole_focus: store.is_some_and(agterm_core::model::AppStore::is_current_workspace_sole_focus),
        };
        let can_remove_window = core.library().can_remove_window();
        let entries: Vec<(String, Option<String>)> = all_items()
            .map(|item| {
                let accel = item
                    .builtin()
                    .filter(|_| shows_accelerator(item))
                    .and_then(|action| core.keymap().equivalent(action))
                    .and_then(accelerator);
                (item.title(context, toggles), accel)
            })
            .collect();
        for item in all_items() {
            if let Some(action) = self
                .group
                .lookup_action(&item.action_name())
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(window.is_some() && item.enabled(context, can_remove_window));
            }
        }
        let lists = Lists {
            windows: core.window_entries(),
            recent: core.recent_entries(),
            live_reset: core.live_reset_menu_visible(),
        };
        if let Some(action) = self
            .group
            .lookup_action(LIVE_RESET_ACTION)
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(lists.live_reset);
        }
        let built = (entries, lists);
        if *self.built.borrow() != built {
            self.rebuild(&built);
            *self.built.borrow_mut() = built;
        }
        self.sync_lists(core, window, context.modal_active());
    }

    fn rebuild(&self, (entries, lists): &Built) {
        self.model.remove_all();
        self.replace_window_actions(&lists.windows);
        let mut entries = entries.iter();
        for (menu, sections) in MENUS {
            let submenu = gio::Menu::new();
            for section in sections {
                let part = gio::Menu::new();
                for (item, (title, accel)) in section.iter().zip(entries.by_ref()) {
                    append_with_live_parts(&part, *item, (title, accel.as_deref()), lists);
                }
                submenu.append_section(None, &part);
            }
            self.model.append_submenu(Some(menu), &submenu);
        }
        self.model.append_submenu(Some("Help"), &help_menu());
        let app = gio::Menu::new();
        for (name, title, _) in APP_ITEMS {
            if name != LIVE_RESET_ACTION || lists.live_reset {
                app.append(Some(title), Some(&format!("menu.{name}")));
            }
        }
        self.model.append_section(None, &app);
    }

    /// One checkable action per window for Open Window, replacing the previous build's.
    fn replace_window_actions(&self, windows: &[WindowEntry]) {
        for name in self.window_actions.borrow_mut().drain(..) {
            self.group.remove_action(&name);
        }
        let mut names = self.window_actions.borrow_mut();
        for entry in windows {
            let name = window_action_name(entry.id);
            let action = gio::SimpleAction::new_stateful(&name, None, &entry.open.to_variant());
            let posting = self.poster.clone();
            let id = entry.id;
            action.connect_activate(move |_, _| {
                state::post(&posting, move |state: &mut AppState| {
                    state.menu_list_activated(MenuListAction::OpenWindow(id));
                });
            });
            self.group.add_action(&action);
            names.push(name);
        }
    }

    /// The live items' check marks and enablement: opening and reopening stand down under a cover.
    fn sync_lists(&self, core: &Core, window: Option<uuid::Uuid>, modal: bool) {
        let available = window.is_some() && !modal;
        for entry in &self.built.borrow().1.windows {
            if let Some(action) = self
                .group
                .lookup_action(&window_action_name(entry.id))
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_state(&entry.open.to_variant());
                action.set_enabled(available);
            }
        }
        let reveal = window.is_some_and(|id| core.can_reveal_active_session(id));
        for (name, enabled) in [
            ("menu-open-recent", available),
            ("menu-clear-recent", window.is_some()),
            ("menu-reveal-session", reveal),
        ] {
            if let Some(action) = self.group.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                action.set_enabled(enabled);
            }
        }
    }

    /// What `keymap.list` reads the live accelerators with.
    #[must_use]
    pub fn reader(&self) -> agterm_core::MenuReader {
        let (model, group) = (self.model.clone(), self.group.clone());
        Box::new(move || menu_items(&model, &group))
    }
}

/// Appends `item` to `part`, with the live parts upstream places around it: Open Window after New Window, Open Recent
/// before Reopen Last Closed Item, Reveal in Files after Duplicate Session.
fn append_with_live_parts(part: &gio::Menu, item: Item, (title, accel): (&str, Option<&str>), lists: &Lists) {
    if item == Item::Row(PaletteCommand::ReopenRecent) {
        part.append_submenu(Some("Open Recent"), &recent_menu(&lists.recent));
    }
    part.append_item(&menu_item(item, title, accel));
    match item {
        Item::Builtin(BuiltinAction::NewWindow) => {
            part.append_submenu(Some("Open Window"), &window_menu(&lists.windows));
        }
        Item::Row(PaletteCommand::DuplicateSession) => {
            part.append(Some("Reveal in Files"), Some("menu.menu-reveal-session"));
        }
        _ => {}
    }
}

/// The Open Window entry action of window `id`.
fn window_action_name(id: uuid::Uuid) -> String {
    format!("menu-window-{id}")
}

/// Open Window: every window of the library, the open ones checked.
fn window_menu(windows: &[WindowEntry]) -> gio::Menu {
    let menu = gio::Menu::new();
    for entry in windows {
        menu.append(
            Some(&entry.name),
            Some(&format!("menu.{}", window_action_name(entry.id))),
        );
    }
    menu
}

/// Help: upstream's sections (the developer documentation, then the installers), then the About window in a section of
/// its own, last (upstream keeps About in the app menu, which a GTK window menu has no counterpart of; its action is
/// added by [`add_list_actions`]).
fn help_menu() -> gio::Menu {
    let help = gio::Menu::new();
    for section in HELP_ITEMS {
        let part = gio::Menu::new();
        for (name, title, _) in section {
            part.append(Some(title), Some(&format!("menu.{name}")));
        }
        help.append_section(None, &part);
    }
    let about = gio::Menu::new();
    about.append(Some("About Agterm"), Some("menu.menu-about"));
    help.append_section(None, &about);
    help
}

/// Open Recent: the closed sessions, then the closed workspaces, then Clear Menu; "No Recent Items" when empty.
fn recent_menu(recent: &RecentEntries) -> gio::Menu {
    let menu = gio::Menu::new();
    if recent.is_empty() {
        menu.append(Some("No Recent Items"), Some("menu.menu-no-recent"));
        return menu;
    }
    for (label, entries) in [("Sessions", &recent.sessions), ("Workspaces", &recent.workspaces)] {
        if entries.is_empty() {
            continue;
        }
        let section = gio::Menu::new();
        for entry in entries {
            let item = gio::MenuItem::new(Some(&entry.title), None);
            item.set_action_and_target_value(Some("menu.menu-open-recent"), Some(&entry.id.to_string().to_variant()));
            section.append_item(&item);
        }
        menu.append_section(Some(label), &section);
    }
    let clear = gio::Menu::new();
    clear.append(Some("Clear Menu"), Some("menu.menu-clear-recent"));
    menu.append_section(None, &clear);
    menu
}

/// The live items' fixed actions: Open Recent's entries (by id), Clear Menu, Reveal in Files, the inert "No Recent
/// Items", and Help's About.
fn add_list_actions(group: &gio::SimpleActionGroup, poster: &Poster) {
    let open = gio::SimpleAction::new("menu-open-recent", Some(gtk::glib::VariantTy::STRING));
    let posting = poster.clone();
    open.connect_activate(move |_, target| {
        let Some(id) = target
            .and_then(gtk::glib::Variant::get::<String>)
            .and_then(|id| uuid::Uuid::parse_str(&id).ok())
        else {
            return;
        };
        state::post(&posting, move |state: &mut AppState| {
            state.menu_list_activated(MenuListAction::OpenRecent(id));
        });
    });
    group.add_action(&open);
    for (name, chosen) in [
        ("menu-clear-recent", MenuListAction::ClearRecent),
        ("menu-reveal-session", MenuListAction::RevealSession),
    ] {
        let action = gio::SimpleAction::new(name, None);
        let posting = poster.clone();
        action.connect_activate(move |_, _| {
            state::post(&posting, move |state: &mut AppState| state.menu_list_activated(chosen));
        });
        group.add_action(&action);
    }
    let none = gio::SimpleAction::new("menu-no-recent", None);
    none.set_enabled(false);
    group.add_action(&none);
    let about = gio::SimpleAction::new("menu-about", None);
    about.connect_activate(|_, _| show_about());
    group.add_action(&about);
}

/// The About window (upstream's standard About panel with its agterm.com link and the build's commit).
fn show_about() {
    let app = gio::Application::default().and_downcast::<gtk::Application>();
    let parent = app.as_ref().and_then(GtkApplicationExt::active_window);
    let mut version = env!("CARGO_PKG_VERSION").to_owned();
    if let Some(commit) = option_env!("AGTERM_BUILD_COMMIT").filter(|commit| !commit.is_empty()) {
        version = format!("{version} ({commit})");
    }
    let icon = app
        .as_ref()
        .and_then(gio::prelude::ApplicationExt::application_id)
        .map(|id| id.to_string())
        .unwrap_or_default();
    let about = adw::AboutDialog::builder()
        .application_name("Agterm")
        .application_icon(icon)
        .version(version)
        .website("https://agterm.com")
        .build();
    adw::prelude::AdwDialogExt::present(&about, parent.as_ref());
}

impl AppState {
    /// A live menu entry was chosen: it acts on the frontmost window.
    pub fn menu_list_activated(&mut self, action: MenuListAction) {
        let Some(window) = self.core.library().active_window_id() else {
            return;
        };
        self.core.perform_menu_list(window, action);
        self.settle();
    }
}

/// One menu entry, its accelerator shown when it has one.
fn menu_item(item: Item, title: &str, accel: Option<&str>) -> gio::MenuItem {
    let entry = gio::MenuItem::new(Some(title), Some(&format!("menu.{}", item.action_name())));
    if let Some(accel) = accel {
        entry.set_attribute_value("accel", Some(&accel.to_variant()));
    }
    entry
}

/// The live accelerators of `model`, as `keymap.list` reports them (upstream's `liveMenuKeyEquivalents`).
#[must_use]
pub fn menu_items(model: &gio::Menu, group: &gio::SimpleActionGroup) -> Vec<ControlKeymapMenuItem> {
    let mut items = Vec::new();
    for index in 0..model.n_items() {
        let menu = string_attribute(model.upcast_ref(), index, "label").unwrap_or_default();
        let Some(submenu) = model.item_link(index, "submenu") else {
            continue;
        };
        collect(&submenu, &menu, group, &mut items);
    }
    items
}

/// Every item with an accelerator in `model` and its sections, attributed to the top-level `menu`.
fn collect(model: &gio::MenuModel, menu: &str, group: &gio::SimpleActionGroup, items: &mut Vec<ControlKeymapMenuItem>) {
    for index in 0..model.n_items() {
        for link in ["section", "submenu"] {
            if let Some(nested) = model.item_link(index, link) {
                collect(&nested, menu, group, items);
            }
        }
        let Some(chord) = string_attribute(model, index, "accel").and_then(|accel| chord_of_accelerator(&accel)) else {
            continue;
        };
        let action = string_attribute(model, index, "action");
        let enabled = action
            .as_deref()
            .and_then(|name| name.strip_prefix("menu."))
            .and_then(|name| group.lookup_action(name))
            .is_none_or(|action| action.is_enabled());
        items.push(ControlKeymapMenuItem {
            menu: menu.to_owned(),
            title: string_attribute(model, index, "label").unwrap_or_default(),
            chord: chord.display_string(),
            selector: action,
            enabled: (!enabled).then_some(false),
        });
    }
}

fn string_attribute(model: &gio::MenuModel, index: i32, name: &str) -> Option<String> {
    model
        .item_attribute_value(index, name, Some(gtk::glib::VariantTy::STRING))
        .and_then(|value| value.get::<String>())
}

fn all_items() -> impl Iterator<Item = Item> {
    MENUS
        .into_iter()
        .flat_map(|(_, sections)| sections.iter().flat_map(|section| section.iter().copied()))
}

/// What activating `item` runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    Row(PaletteCommand),
    Builtin(BuiltinAction),
}

fn item_command(item: Item) -> MenuCommand {
    match item {
        Item::Row(command) => MenuCommand::Row(command),
        Item::Builtin(action) => MenuCommand::Builtin(action),
    }
}

/// A chord as a GTK accelerator (`<Control><Shift>t`), `None` for a key GTK cannot name.
#[must_use]
pub fn accelerator(chord: &Chord) -> Option<String> {
    let key = match chord.key.as_str() {
        "return" => gdk::Key::Return,
        "tab" => gdk::Key::Tab,
        "space" => gdk::Key::space,
        "delete" => gdk::Key::BackSpace,
        "left" => gdk::Key::Left,
        "right" => gdk::Key::Right,
        "up" => gdk::Key::Up,
        "down" => gdk::Key::Down,
        name if agterm_core::keymap::is_function_key(name) => gdk::Key::from_name(name.to_uppercase())?,
        other => gdk::Key::from_name(keysym_name(other.chars().next()?))?,
    };
    let mods = [
        (agterm_core::keymap::Modifiers::CONTROL, gdk::ModifierType::CONTROL_MASK),
        (agterm_core::keymap::Modifiers::COMMAND, gdk::ModifierType::SUPER_MASK),
        (agterm_core::keymap::Modifiers::OPTION, gdk::ModifierType::ALT_MASK),
        (agterm_core::keymap::Modifiers::SHIFT, gdk::ModifierType::SHIFT_MASK),
    ]
    .into_iter()
    .filter(|(bit, _)| chord.mods.contains(*bit))
    .fold(gdk::ModifierType::empty(), |mods, (_, mask)| mods | mask);
    Some(gtk::accelerator_name(key, mods).to_string())
}

/// The keysym name of a single-character key: the punctuation's own names, else the character itself or its
/// `U<hex>` form.
fn keysym_name(ch: char) -> String {
    let name = match ch {
        '=' => "equal",
        '-' => "minus",
        '`' => "grave",
        '[' => "bracketleft",
        ']' => "bracketright",
        ';' => "semicolon",
        '\'' => "apostrophe",
        ',' => "comma",
        '.' => "period",
        '/' => "slash",
        '\\' => "backslash",
        _ if ch.is_ascii_alphanumeric() => return ch.to_string(),
        _ => return format!("U{:04X}", u32::from(ch)),
    };
    name.to_owned()
}

/// The chord a GTK accelerator spells, the inverse of [`accelerator`].
#[must_use]
pub fn chord_of_accelerator(accel: &str) -> Option<Chord> {
    let (key, mods) = gtk::accelerator_parse(accel)?;
    let name = crate::keymap_dispatch::named_key(key)
        .map(str::to_owned)
        .or_else(|| key.to_unicode().map(|ch| ch.to_lowercase().collect()))?;
    Some(Chord::new(crate::keymap_dispatch::modifiers(mods), name))
}

#[cfg(test)]
mod tests {
    use agterm_core::keymap::{linux_defaults, parse_keymap};

    use super::*;

    #[test]
    fn every_keyed_linux_default_round_trips_through_an_accelerator() {
        crate::gtk_test::on_gtk_thread(round_trips);
    }

    fn round_trips() {
        let (keymap, _) = parse_keymap("", &linux_defaults());
        for action in BuiltinAction::all() {
            let Some(chord) = keymap.equivalent(action) else {
                continue;
            };
            let accel = accelerator(chord).unwrap_or_else(|| panic!("{} has no accelerator", action.name()));
            assert_eq!(chord_of_accelerator(&accel).as_ref(), Some(chord), "{accel}");
        }
    }

    #[test]
    fn every_keyed_action_has_a_menu_item_or_is_monitor_delivered() {
        let items: Vec<Item> = all_items().collect();
        for action in BuiltinAction::all() {
            let listed = items.iter().any(|item| item.builtin() == Some(action));
            assert!(listed, "{} has no menu item", action.name());
        }
    }

    #[test]
    fn focus_workspace_reads_unfocus_while_the_current_workspace_is_the_sole_focus() {
        let item = Item::Row(PaletteCommand::FocusWorkspace);
        let context = PaletteContext::default();
        assert_eq!(item.title(context, Toggles::default()), "Focus Workspace");
        let sole = Toggles {
            sole_focus: true,
            ..Toggles::default()
        };
        assert_eq!(item.title(context, sole), "Unfocus Workspace");
    }

    #[test]
    fn the_help_menu_offers_the_docs_and_the_three_installers() {
        let titles: Vec<&str> = HELP_ITEMS
            .iter()
            .flat_map(|section| section.iter().map(|item| item.1))
            .collect();
        assert_eq!(
            titles,
            [
                "Developer Documentation…",
                "Install Command Line Tool…",
                "Install Agent Status Hooks…",
                "Install Agent Skill…"
            ]
        );
    }

    #[test]
    fn accelerators_use_gtk_names() {
        crate::gtk_test::on_gtk_thread(accelerator_names);
    }

    fn accelerator_names() {
        let chord = |text: &str| agterm_core::keymap::parse_keybind(text).unwrap().remove(0);
        assert_eq!(
            accelerator(&chord("ctrl+shift+t")).as_deref(),
            Some("<Shift><Control>t")
        );
        assert_eq!(accelerator(&chord("ctrl+=")).as_deref(), Some("<Control>equal"));
        assert_eq!(accelerator(&chord("f11")).as_deref(), Some("F11"));
        assert_eq!(accelerator(&chord("opt+shift+up")).as_deref(), Some("<Shift><Alt>Up"));
        assert_eq!(accelerator(&chord("cmd+n")).as_deref(), Some("<Super>n"));
    }

    /// Every label of `model` and its sections and submenus, in order.
    fn labels(model: &gio::MenuModel, into: &mut Vec<String>) {
        for index in 0..model.n_items() {
            if let Some(label) = string_attribute(model, index, "label") {
                into.push(label);
            }
            for link in ["section", "submenu"] {
                if let Some(nested) = model.item_link(index, link) {
                    labels(&nested, into);
                }
            }
        }
    }

    #[test]
    fn file_carries_the_live_lists_and_help_the_docs_and_about() {
        crate::gtk_test::on_gtk_thread(live_lists);
    }

    fn live_lists() {
        let mut library = agterm_core::model::WindowLibrary::seeded("/home/u");
        let closed = library.new_window(Some("work"));
        library.close_window(closed);
        let core = Core::new(
            agterm_core::CoreConfig {
                app: agterm_proto::AppIdentity::from_recorded("0.1.0", None),
                capabilities: Vec::new(),
            },
            agterm_core::CoreState {
                library,
                settings: agterm_core::persist::AppSettings::default(),
                persistence: None,
            },
        );
        let menu = AppMenu::new(&crate::state::detached_poster());
        menu.sync(&core);
        let mut all = Vec::new();
        labels(menu.model().upcast_ref(), &mut all);
        for expected in [
            "Open Window",
            "work",
            "Open Recent",
            "No Recent Items",
            "Reveal in Files",
            "Help",
            "Developer Documentation…",
            "About Agterm",
        ] {
            assert!(all.iter().any(|label| label == expected), "{expected} missing: {all:?}");
        }
        let position = |label: &str| all.iter().position(|item| item == label).unwrap();
        assert_eq!(position("Open Window"), position("New Window") + 1);
        assert_eq!(position("Reveal in Files"), position("Duplicate Session") + 1);
        let docs = all.iter().filter(|label| *label == "Developer Documentation…").count();
        assert_eq!(docs, 1, "one Developer Documentation item: {all:?}");
        assert!(position("Help") < position("Developer Documentation…"));
        assert!(position("Developer Documentation…") < position("Install Command Line Tool…"));
        assert!(position("Install Agent Skill…") < position("About Agterm"));
        let window = menu
            .group()
            .lookup_action(&window_action_name(closed))
            .expect("one action per window");
        assert_eq!(window.state().and_then(|state| state.get::<bool>()), Some(false));
        assert!(window.is_enabled());
        assert!(
            !menu_items(menu.model(), menu.group()).is_empty(),
            "the accelerators still read back"
        );
    }

    #[test]
    fn reset_live_sessions_sits_before_quit_only_under_live_sessions() {
        crate::gtk_test::on_gtk_thread(live_reset_item);
    }

    fn live_reset_item() {
        use agterm_core::persist::{AppSettings, RestoreMode};
        use agterm_core::restore::mode::RestoreLaunchDecision;

        let core = |configured: RestoreMode, active: RestoreMode| {
            let mut core = Core::new(
                agterm_core::CoreConfig {
                    app: agterm_proto::AppIdentity::from_recorded("0.1.0", None),
                    capabilities: Vec::new(),
                },
                agterm_core::CoreState {
                    library: agterm_core::model::WindowLibrary::seeded("/home/u"),
                    settings: AppSettings {
                        restore_mode: Some(configured),
                        ..AppSettings::default()
                    },
                    persistence: None,
                },
            );
            core.set_restore_launch(RestoreLaunchDecision::new(active, None));
            core
        };
        let menu = AppMenu::new(&crate::state::detached_poster());
        let shown = |menu: &AppMenu| {
            let mut all = Vec::new();
            labels(menu.model().upcast_ref(), &mut all);
            all
        };
        let action = |menu: &AppMenu| {
            menu.group()
                .lookup_action(LIVE_RESET_ACTION)
                .expect("the item's action")
        };

        menu.sync(&core(RestoreMode::Live, RestoreMode::Live));
        let all = shown(&menu);
        let position = |label: &str| all.iter().position(|item| item == label).unwrap();
        assert_eq!(position("Reset Live Sessions…"), position("Preferences") + 1);
        assert_eq!(position("Quit"), position("Reset Live Sessions…") + 1);
        assert!(action(&menu).is_enabled());
        let wired = (0..menu.model().n_items())
            .filter_map(|index| menu.model().item_link(index, "section"))
            .any(|section| {
                (0..section.n_items()).any(|index| {
                    string_attribute(&section, index, "action").as_deref() == Some("menu.reset-live-sessions")
                })
            });
        assert!(wired, "the item activates its action");

        for (configured, active) in [
            (RestoreMode::None, RestoreMode::None),
            (RestoreMode::Live, RestoreMode::None),
            (RestoreMode::None, RestoreMode::Live),
        ] {
            menu.sync(&core(configured, active));
            assert!(
                !shown(&menu).iter().any(|label| label == "Reset Live Sessions…"),
                "{configured:?}/{active:?}"
            );
            assert!(!action(&menu).is_enabled());
        }
    }
}
