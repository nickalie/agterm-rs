//! The app menu (upstream's `agtermApp+Menus.swift`; port of the GTK build's `app_menu.rs`): File, View, Navigate and
//! Help as submenus of the window's menu button at the title bar's trailing end, every item named after its palette
//! row or built-in action, enabled by the window's palette context (the one predicate the palette and the keymap
//! read), and every keyed item showing its resolved chord. The app's own items close the menu: Settings…, Reset Live
//! Sessions… while live sessions are both configured and running (upstream's Quit group; its dialogs and the reopen
//! are `zmx_runtime.rs`'s), and Quit (Linux has no global menu bar, so the in-window menu is their only mouse path).
//!
//! File also carries upstream's live parts ([`agterm_core::menu_lists`]): Open Window, listing the library with the open
//! windows checked; Open Recent, the closed sessions and workspaces; and Reveal in Files. Help holds the developer
//! documentation, the agent integrations' installers and the About alert.
//!
//! The chords are display only: the key monitor (`keymap_dispatch`) dispatches them before any view sees them, from the
//! same resolved keymap. `keymap.list` reads them back from the frontmost window's menu ([`AppMenu::keymap_items`]),
//! as the GTK build reads its menu model.
//!
//! The menu is a pure model of the core ([`AppMenu::of`]), built into gpui-kit's popup menu each time it opens
//! ([`build`]), so its titles, chords and enablement are tested without a window.

use std::rc::Rc;

use agterm_core::Core;
use agterm_core::app_actions::{ActionEffect, ActionOrigin};
use agterm_core::keymap::{BuiltinAction, Chord};
use agterm_core::menu_lists::{MenuListAction, RecentEntries, WindowEntry};
use agterm_core::palette_catalog::{PaletteCommand, PaletteContext};
use agterm_proto::ControlKeymapMenuItem;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{
    App, Context, Hsla, InteractiveElement as _, ParentElement as _, SharedString, Styled as _, Window, div,
};
use uuid::Uuid;

use crate::modal::{AlertButton, AlertSpec, open_alert};
use crate::state::{AppState, Poster, post};

/// The developer documentation Help opens (the GTK build's `DEVELOPER_DOCS_URL`).
pub const DEVELOPER_DOCS_URL: &str = "https://agterm.com/docs#agtermctl";
/// The website the About alert links (upstream's About panel).
pub const WEBSITE_URL: &str = "https://agterm.com";

/// Reset Live Sessions…'s id (the GTK build's action name).
pub const RESET_LIVE_SESSIONS: &str = "reset-live-sessions";

/// One menu item: the palette row it runs, or a built-in with no palette row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
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
    // no AppKit-appended full screen item exists off macOS, so the action gets one of its own
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
    /// The item's id: its palette row's or built-in's name.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Row(command) => command.id(),
            Self::Builtin(action) => action.name(),
        }
    }

    fn builtin(self) -> Option<BuiltinAction> {
        match self {
            Self::Row(command) => command.builtin_action(),
            Self::Builtin(action) => Some(action),
        }
    }

    /// The item's title as upstream's menu spells it in `context`.
    fn title(self, context: PaletteContext, toggles: Toggles) -> &'static str {
        match self {
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
            Self::Row(PaletteCommand::ToggleSidebar) if toggles.sidebar_shown => "Hide Sidebar",
            Self::Row(PaletteCommand::ToggleSidebar) => "Show Sidebar",
            Self::Row(PaletteCommand::ToggleScratch) if toggles.scratch_shown => "Hide Scratch",
            Self::Row(PaletteCommand::ToggleScratch) => "Show Scratch",
            Self::Row(PaletteCommand::FocusWorkspace) if toggles.sole_focus => "Unfocus Workspace",
            Self::Row(command) => command.title(&context),
        }
    }

    /// Whether the item can run in `context`.
    fn enabled(self, context: PaletteContext, can_remove_window: bool) -> bool {
        match self {
            Self::Row(command) => command.is_enabled(&context),
            Self::Builtin(BuiltinAction::DeleteWindow) => can_remove_window && !context.modal_active(),
            Self::Builtin(_) => !context.modal_active(),
        }
    }

    /// Whether the menu shows the item's chord: upstream's menu carries none for Reopen Closed Item, which its own
    /// monitor delivers so text undo keeps working.
    fn shows_chord(self) -> bool {
        self != Self::Row(PaletteCommand::UndoClose)
    }
}

/// The window's state the toggling items name: Hide/Show Sidebar and Scratch, and Focus Workspace reading Unfocus
/// Workspace while the current workspace is the sole focus (upstream's `isCurrentWorkspaceSoleFocus`).
#[derive(Debug, Clone, Copy, Default)]
struct Toggles {
    sidebar_shown: bool,
    scratch_shown: bool,
    sole_focus: bool,
}

/// What a menu item runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// A palette row or a built-in, in the menu's window.
    Command(Item),
    /// A live entry (Open Window, Open Recent, Clear Menu, Reveal in Files), in the menu's window.
    List(MenuListAction),
    /// The app's own items and Help's.
    App(AppAction),
}

/// The items no keymap action runs: the app's and Help's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    Settings,
    ResetLiveSessions,
    Quit,
    DeveloperDocs,
    InstallCli,
    InstallHooks,
    InstallSkill,
    About,
}

/// One clickable entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// Its element id's tail (`app-menu-item-<id>`) and its `keymap.list` selector's (`menu.menu-<id>`).
    pub id: String,
    pub title: String,
    /// The chord shown beside the title.
    pub chord: Option<Chord>,
    pub enabled: bool,
    /// A check mark (Open Window's open windows).
    pub checked: bool,
    pub action: MenuAction,
}

impl MenuItem {
    fn new(id: impl Into<String>, title: impl Into<String>, action: MenuAction) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            chord: None,
            enabled: true,
            checked: false,
            action,
        }
    }

    fn disabled(mut self, disabled: bool) -> Self {
        self.enabled = !disabled;
        self
    }
}

/// One entry of a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Item(MenuItem),
    Submenu {
        title: String,
        entries: Vec<Entry>,
    },
    /// An inert heading (Open Recent's Sessions and Workspaces).
    Label(String),
    Separator,
}

/// The whole menu of one window: the titled submenus, then the app's own items.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppMenu {
    pub menus: Vec<(String, Vec<Entry>)>,
    pub app: Vec<MenuItem>,
}

impl AppMenu {
    /// Window `window`'s menu as the core stands.
    #[must_use]
    pub fn of(core: &Core, window: Uuid) -> Self {
        let context = core.palette_context(window);
        let store = core.library().store(window);
        let toggles = Toggles {
            sidebar_shown: store.is_none_or(|store| store.sidebar().visible),
            scratch_shown: store
                .and_then(|store| store.active_session())
                .is_some_and(|session| session.scratch_active),
            sole_focus: store.is_some_and(agterm_core::model::AppStore::is_current_workspace_sole_focus),
        };
        let live = Live {
            windows: core.window_entries(),
            recent: core.recent_entries(),
            available: store.is_some() && !context.modal_active(),
            reveal: core.can_reveal_active_session(window),
        };
        let can_remove_window = core.library().can_remove_window();
        let open = store.is_some();
        let item = |item: Item| {
            let chord = item
                .builtin()
                .filter(|_| item.shows_chord())
                .and_then(|action| core.keymap().equivalent(action))
                .cloned();
            MenuItem {
                chord,
                ..MenuItem::new(item.id(), item.title(context, toggles), MenuAction::Command(item))
                    .disabled(!(open && item.enabled(context, can_remove_window)))
            }
        };
        let mut menus: Vec<(String, Vec<Entry>)> = MENUS
            .iter()
            .map(|(title, sections)| {
                let mut entries = Vec::new();
                for (index, section) in sections.iter().enumerate() {
                    if index > 0 {
                        entries.push(Entry::Separator);
                    }
                    for each in section.iter().copied() {
                        with_live_parts(&mut entries, each, item(each), &live);
                    }
                }
                ((*title).to_owned(), entries)
            })
            .collect();
        menus.push(("Help".to_owned(), help()));
        let mut app = vec![MenuItem::new(
            "settings",
            "Settings…",
            MenuAction::App(AppAction::Settings),
        )];
        // shares Quit's section, as upstream's `CommandGroup(before: .appTermination)`: the reset quits and reopens
        if core.live_reset_menu_visible() {
            app.push(MenuItem::new(
                RESET_LIVE_SESSIONS,
                "Reset Live Sessions…",
                MenuAction::App(AppAction::ResetLiveSessions),
            ));
        }
        app.push(MenuItem::new("quit", "Quit", MenuAction::App(AppAction::Quit)));
        Self { menus, app }
    }

    /// Every item, the submenus' and the app's, in menu order with its top-level menu's title (`""` for the app's).
    #[must_use]
    pub fn items(&self) -> Vec<(&str, &MenuItem)> {
        fn walk<'a>(menu: &'a str, entries: &'a [Entry], into: &mut Vec<(&'a str, &'a MenuItem)>) {
            for entry in entries {
                match entry {
                    Entry::Item(item) => into.push((menu, item)),
                    Entry::Submenu { entries, .. } => walk(menu, entries, into),
                    Entry::Label(_) | Entry::Separator => {}
                }
            }
        }
        let mut items = Vec::new();
        for (title, entries) in &self.menus {
            walk(title, entries, &mut items);
        }
        items.extend(self.app.iter().map(|item| ("", item)));
        items
    }

    /// The live chords, as `keymap.list` reports them (upstream's `liveMenuKeyEquivalents`): every keyed item with its
    /// top-level menu, a disabled one marked so.
    #[must_use]
    pub fn keymap_items(&self) -> Vec<ControlKeymapMenuItem> {
        self.items()
            .into_iter()
            .filter_map(|(menu, item)| {
                let chord = item.chord.as_ref()?;
                Some(ControlKeymapMenuItem {
                    menu: menu.to_owned(),
                    title: item.title.clone(),
                    chord: chord.display_string(),
                    selector: Some(format!("menu.menu-{}", item.id)),
                    enabled: (!item.enabled).then_some(false),
                })
            })
            .collect()
    }
}

/// The live parts of File as the core stands.
struct Live {
    windows: Vec<WindowEntry>,
    recent: RecentEntries,
    /// Opening and reopening stand down under a cover.
    available: bool,
    reveal: bool,
}

/// Appends `item` to `entries`, with the live parts upstream places around it: Open Window after New Window, Open Recent
/// before Reopen Last Closed Item, Reveal in Files after Duplicate Session.
fn with_live_parts(entries: &mut Vec<Entry>, which: Item, item: MenuItem, live: &Live) {
    if which == Item::Row(PaletteCommand::ReopenRecent) {
        entries.push(Entry::Submenu {
            title: "Open Recent".to_owned(),
            entries: recent_menu(&live.recent, live.available),
        });
    }
    entries.push(Entry::Item(item));
    match which {
        Item::Builtin(BuiltinAction::NewWindow) => entries.push(Entry::Submenu {
            title: "Open Window".to_owned(),
            entries: window_menu(&live.windows, live.available),
        }),
        Item::Row(PaletteCommand::DuplicateSession) => entries.push(Entry::Item(
            MenuItem::new(
                "reveal-session",
                "Reveal in Files",
                MenuAction::List(MenuListAction::RevealSession),
            )
            .disabled(!live.reveal),
        )),
        _ => {}
    }
}

/// Open Window: every window of the library, the open ones checked.
fn window_menu(windows: &[WindowEntry], available: bool) -> Vec<Entry> {
    windows
        .iter()
        .map(|entry| {
            Entry::Item(MenuItem {
                checked: entry.open,
                ..MenuItem::new(
                    format!("window-{}", entry.id),
                    entry.name.clone(),
                    MenuAction::List(MenuListAction::OpenWindow(entry.id)),
                )
                .disabled(!available)
            })
        })
        .collect()
}

/// Open Recent: the closed sessions, then the closed workspaces, each under its heading, then Clear Menu; an inert "No
/// Recent Items" when empty.
fn recent_menu(recent: &RecentEntries, available: bool) -> Vec<Entry> {
    if recent.is_empty() {
        let none = MenuItem::new(
            "no-recent",
            "No Recent Items",
            MenuAction::List(MenuListAction::ClearRecent),
        );
        return vec![Entry::Item(none.disabled(true))];
    }
    let mut entries = Vec::new();
    for (label, items) in [("Sessions", &recent.sessions), ("Workspaces", &recent.workspaces)] {
        if items.is_empty() {
            continue;
        }
        entries.push(Entry::Label(label.to_owned()));
        entries.extend(items.iter().map(|entry| {
            Entry::Item(
                MenuItem::new(
                    format!("recent-{}", entry.id),
                    entry.title.clone(),
                    MenuAction::List(MenuListAction::OpenRecent(entry.id)),
                )
                .disabled(!available),
            )
        }));
        entries.push(Entry::Separator);
    }
    entries.push(Entry::Item(MenuItem::new(
        "clear-recent",
        "Clear Menu",
        MenuAction::List(MenuListAction::ClearRecent),
    )));
    entries
}

/// Help (upstream's `CommandGroup(replacing: .help)`): the developer documentation, the installers, then the About
/// alert in a section of its own (upstream keeps About in the app menu, which an in-window menu has no counterpart of).
fn help() -> Vec<Entry> {
    let app = |id: &str, title: &str, action| Entry::Item(MenuItem::new(id, title, MenuAction::App(action)));
    vec![
        app("developer-docs", "Developer Documentation…", AppAction::DeveloperDocs),
        Entry::Separator,
        app("install-cli", "Install Command Line Tool…", AppAction::InstallCli),
        app("install-hooks", "Install Agent Status Hooks…", AppAction::InstallHooks),
        app("install-skill", "Install Agent Skill…", AppAction::InstallSkill),
        Entry::Separator,
        app("about", "About Agterm", AppAction::About),
    ]
}

/// What a clicked item runs, with the window its menu belongs to.
pub type Perform = Rc<dyn Fn(MenuAction, &mut Window, &mut App)>;

/// The colors the items draw their chords in.
#[derive(Debug, Clone, Copy)]
pub struct Ink {
    pub chord: Hsla,
}

/// Fills `popup` with `menu`: each submenu, then a separator and the app's items; each item runs `perform`.
pub fn build(
    popup: PopupMenu,
    menu: &AppMenu,
    perform: &Perform,
    ink: Ink,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let mut popup = fill(popup, &submenus(menu), perform, ink, window, cx).separator();
    for item in &menu.app {
        popup = popup.item(item_of(item, perform, ink));
    }
    popup
}

/// The top-level menus as submenu entries.
fn submenus(menu: &AppMenu) -> Vec<Entry> {
    menu.menus
        .iter()
        .map(|(title, entries)| Entry::Submenu {
            title: title.clone(),
            entries: entries.clone(),
        })
        .collect()
}

fn fill(
    mut popup: PopupMenu,
    entries: &[Entry],
    perform: &Perform,
    ink: Ink,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    for entry in entries {
        popup = match entry {
            Entry::Item(item) => popup.item(item_of(item, perform, ink)),
            Entry::Label(label) => popup.label(label.clone()),
            Entry::Separator => popup.separator(),
            Entry::Submenu { title, entries } => {
                let (entries, perform) = (entries.clone(), Rc::clone(perform));
                popup.submenu(title.clone(), window, cx, move |submenu, window, cx| {
                    fill(submenu, &entries, &perform, ink, window, cx)
                })
            }
        };
    }
    popup
}

/// One item: its title and chord in a row of its own (element id `app-menu-item-<id>`), greyed and inert while
/// disabled.
fn item_of(item: &MenuItem, perform: &Perform, ink: Ink) -> PopupMenuItem {
    let id = SharedString::from(format!("app-menu-item-{}", item.id));
    let title = SharedString::from(item.title.clone());
    let chord = item
        .chord
        .as_ref()
        .map(|chord| SharedString::from(crate::titlebar::chord_label(chord)));
    let (perform, action) = (Rc::clone(perform), item.action);
    PopupMenuItem::element(move |_, _| {
        let selector = id.to_string();
        div()
            .id(id.clone())
            .debug_selector(move || selector.clone())
            .flex()
            .flex_row()
            .flex_1()
            .gap_4()
            .items_center()
            .justify_between()
            .child(title.clone())
            .children(chord.clone().map(|chord| div().text_color(ink.chord).child(chord)))
    })
    .disabled(!item.enabled)
    .checked(item.checked)
    .on_click(move |_, window, cx| perform(action, window, cx))
}

/// What the menu of model window `id` runs: a row, a built-in or a live entry as a main-loop task in that window; the
/// app's and Help's items with the window the menu showed over.
#[must_use]
pub fn perform(poster: &Poster, id: Uuid) -> Perform {
    let poster = poster.clone();
    Rc::new(move |action, window, cx| match action {
        MenuAction::Command(item) => post(&poster, move |state: &mut AppState| state.menu_activated(id, item)),
        MenuAction::List(list) => post(&poster, move |state: &mut AppState| state.menu_list_activated(id, list)),
        MenuAction::App(app) => run_app(app, &poster, id, window, cx),
    })
}

/// Runs one of the app's or Help's items over `window`, model window `id`'s.
fn run_app(action: AppAction, poster: &Poster, id: Uuid, window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    match action {
        AppAction::Settings => post(poster, move |state: &mut AppState| {
            state.open_settings(id);
        }),
        AppAction::ResetLiveSessions => post(poster, AppState::reset_live_sessions),
        AppAction::Quit => post(poster, AppState::quit_from_menu),
        AppAction::DeveloperDocs => cx.open_url(DEVELOPER_DOCS_URL),
        AppAction::InstallCli => crate::integrations::run_cli(handle, cx),
        AppAction::InstallHooks => crate::integrations::run_hooks(handle, cx),
        AppAction::InstallSkill => crate::integrations::run_skill(handle, cx),
        AppAction::About => {
            open_alert(
                window,
                about(),
                |answer, _, cx| {
                    if answer.button == ABOUT_WEBSITE {
                        cx.open_url(WEBSITE_URL);
                    }
                },
                cx,
            );
        }
    }
}

/// The About alert's website button.
pub const ABOUT_WEBSITE: usize = 1;

/// The About alert (upstream's standard About panel): the app's name, the build's version and commit, OK and the
/// website.
#[must_use]
pub fn about() -> AlertSpec {
    let mut version = env!("CARGO_PKG_VERSION").to_owned();
    if let Some(commit) = option_env!("AGTERM_BUILD_COMMIT").filter(|commit| !commit.is_empty()) {
        version = format!("{version} ({commit})");
    }
    AlertSpec {
        buttons: vec![
            AlertButton::cancel("OK").as_default().suggested(),
            AlertButton::plain("agterm.com").with_id("about-website"),
        ],
        ..AlertSpec::info("Agterm", format!("Version {version}"))
    }
}

impl AppState {
    /// A menu item of window `window` was chosen: a palette row or a built-in runs through the core there, its
    /// frontend half lands with the app.
    pub fn menu_activated(&mut self, window: Uuid, item: Item) {
        if self.is_quitting() {
            return;
        }
        if matches!(item, Item::Row(PaletteCommand::ClearFlagged)) {
            // upstream's `clearFlags` confirms first
            self.clear_flags_requested(window);
            return;
        }
        let effect = match item {
            Item::Row(row) => self.core.perform_menu_command(window, row),
            Item::Builtin(action) => self.core.perform_builtin(ActionOrigin { window, quick: false }, action),
        };
        if let ActionEffect::Frontend(action) = effect {
            self.frontend_action(action);
        }
        self.settle();
    }

    /// A live menu entry of window `window` was chosen.
    pub fn menu_list_activated(&mut self, window: Uuid, action: MenuListAction) {
        if self.is_quitting() {
            return;
        }
        self.core.perform_menu_list(window, action);
        self.settle();
    }
}

#[cfg(test)]
mod tests;
