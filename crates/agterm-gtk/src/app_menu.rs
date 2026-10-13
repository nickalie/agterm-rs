//! The app menu (upstream's `agtermApp+Menus.swift`, as a GTK menu): File, View and Navigate in each window's primary
//! menu, every item an app action named after its row, enabled by the frontmost window's palette context (the one
//! predicate the palette and the keymap read), and every keyed item showing its resolved chord as its accelerator.
//!
//! The accelerators are display only: the keymap monitor (`keymap_dispatch`) dispatches the chords before any widget
//! sees them, from the same resolved keymap. `keymap.list` reads them back from this menu model (`menu_items`).

use std::cell::RefCell;

use agterm_core::Core;
use agterm_core::keymap::{BuiltinAction, Chord};
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
    fn title(self, context: PaletteContext, sidebar_shown: bool, scratch_shown: bool) -> String {
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
                if sidebar_shown {
                    "Hide Sidebar"
                } else {
                    "Show Sidebar"
                }
            }
            Self::Row(PaletteCommand::ToggleScratch) => {
                if scratch_shown {
                    "Hide Scratch"
                } else {
                    "Show Scratch"
                }
            }
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

/// The items a keymap shows accelerators on: upstream's menu carries none for Reopen Closed Item, which its own
/// monitor delivers so text undo keeps working.
fn shows_accelerator(item: Item) -> bool {
    item != Item::Row(PaletteCommand::UndoClose)
}

/// The menu model, its actions, and what it was last built from.
pub struct AppMenu {
    model: gio::Menu,
    group: gio::SimpleActionGroup,
    /// The titles and accelerators the model was built with, so an unchanged state rebuilds nothing.
    built: RefCell<Vec<(String, Option<String>)>>,
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
        Self {
            model: gio::Menu::new(),
            group,
            built: RefCell::new(Vec::new()),
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
        let sidebar_shown = store.is_none_or(|store| store.sidebar().visible);
        let scratch_shown = store
            .and_then(|store| store.active_session())
            .is_some_and(|session| session.scratch_active);
        let can_remove_window = core.library().can_remove_window();
        let entries: Vec<(String, Option<String>)> = all_items()
            .map(|item| {
                let accel = item
                    .builtin()
                    .filter(|_| shows_accelerator(item))
                    .and_then(|action| core.keymap().equivalent(action))
                    .and_then(accelerator);
                (item.title(context, sidebar_shown, scratch_shown), accel)
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
        if *self.built.borrow() != entries {
            self.rebuild(&entries);
            *self.built.borrow_mut() = entries;
        }
    }

    fn rebuild(&self, entries: &[(String, Option<String>)]) {
        self.model.remove_all();
        let mut entries = entries.iter();
        for (menu, sections) in MENUS {
            let submenu = gio::Menu::new();
            for section in sections {
                let part = gio::Menu::new();
                for (item, (title, accel)) in section.iter().zip(entries.by_ref()) {
                    part.append_item(&menu_item(*item, title, accel.as_deref()));
                }
                submenu.append_section(None, &part);
            }
            self.model.append_submenu(Some(menu), &submenu);
        }
    }

    /// What `keymap.list` reads the live accelerators with.
    #[must_use]
    pub fn reader(&self) -> agterm_core::MenuReader {
        let (model, group) = (self.model.clone(), self.group.clone());
        Box::new(move || menu_items(&model, &group))
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
}
