//! Interface: which title-bar and sidebar chrome elements show, two toggles per row as upstream lays them out, the
//! sidebar in inactive windows, and the quick terminal's size (upstream's `InterfaceSettingsView`).

use agterm_core::persist::settings::InterfaceElement;
use agterm_core::quick_metrics::SIZE_PERCENT_CHOICES;
use agterm_core::settings_model::{InterfaceChange, SettingsChange};

use super::Snapshot;
use super::form::{Control, Item, Picker, Section, toggle};

fn change(change: InterfaceChange) -> SettingsChange {
    SettingsChange::Interface(change)
}

/// The page's sections.
#[must_use]
pub fn sections(snapshot: &Snapshot) -> Vec<Section> {
    let settings = &snapshot.settings;
    vec![
        Section::new("Title Bar", elements(snapshot, true)).two_columns(),
        Section::new("Sidebar", elements(snapshot, false)).two_columns(),
        Section::new(
            "Multiple Windows",
            vec![toggle(
                "settings-auto-hide-inactive-sidebars",
                "Show sidebar only in the active window",
                settings.auto_hide_sidebar_inactive_windows.unwrap_or(false),
                |on| change(InterfaceChange::AutoHideSidebarInactiveWindows(on)),
            )],
        ),
        Section::new("Quick Terminal", vec![quick_terminal_size(snapshot)]),
    ]
}

/// One toggle per element of the title bar (or the sidebar): on is shown, so hiding a default-shown element is the edit
/// that writes.
fn elements(snapshot: &Snapshot, title_bar: bool) -> Vec<Item> {
    InterfaceElement::ALL
        .iter()
        .copied()
        .filter(|element| element.in_title_bar() == title_bar)
        .map(|element| {
            toggle(
                &format!("settings-interface-{}", element.raw()),
                element.display_name(),
                !snapshot.settings.is_interface_element_hidden(element),
                move |visible| change(InterfaceChange::ElementVisible { element, visible }),
            )
        })
        .collect()
}

/// The quick terminal's share of the screen; Default is the built-in size, not a percentage.
fn quick_terminal_size(snapshot: &Snapshot) -> Item {
    let sizes: Vec<(String, Option<i64>)> = std::iter::once(("Default".to_owned(), None))
        .chain(
            SIZE_PERCENT_CHOICES
                .iter()
                .map(|percent| (format!("{percent}% of screen"), Some(*percent))),
        )
        .collect();
    Item::new(
        "settings-quick-terminal-size",
        "Size",
        Control::Picker(Picker::new(
            sizes,
            &snapshot.settings.effective_quick_terminal_size_percent(),
            |percent| change(InterfaceChange::QuickTerminalSizePercent(percent)),
        )),
    )
}
