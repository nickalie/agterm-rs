//! Interface: which title-bar and sidebar chrome elements show, the sidebar in inactive windows, and the quick
//! terminal's size (upstream's `InterfaceSettingsView`). Upstream lays the element toggles out two per row to fit its
//! fixed window; a preferences page scrolls, so each is a row of its own here.

use adw::prelude::*;
use agterm_core::persist::settings::InterfaceElement;
use agterm_core::quick_metrics::SIZE_PERCENT_CHOICES;
use agterm_core::settings_model::{InterfaceChange, SettingsChange};

use super::Rows;
use super::rows;

fn change(change: InterfaceChange) -> SettingsChange {
    SettingsChange::Interface(change)
}

pub fn page(rows: &mut Rows) -> adw::PreferencesPage {
    let page = rows::page("Interface", "interface", "view-grid-symbolic");
    page.add(&elements(rows, "Title Bar", true));
    page.add(&elements(rows, "Sidebar", false));
    let windows = rows::group("Multiple Windows");
    windows.add(&rows::switch(
        rows,
        "settings-auto-hide-inactive-sidebars",
        "Show sidebar only in the active window",
        |snapshot| snapshot.settings.auto_hide_sidebar_inactive_windows.unwrap_or(false),
        |on| change(InterfaceChange::AutoHideSidebarInactiveWindows(on)),
    ));
    page.add(&windows);
    page.add(&quick_terminal(rows));
    page
}

/// One element's toggle per row: on is shown, so hiding a default-shown element is the edit that writes.
fn elements(rows: &mut Rows, title: &str, title_bar: bool) -> adw::PreferencesGroup {
    let group = rows::group(title);
    for element in InterfaceElement::ALL.iter().copied() {
        if element.in_title_bar() != title_bar {
            continue;
        }
        group.add(&rows::switch(
            rows,
            &format!("settings-interface-{}", element.raw()),
            element.display_name(),
            move |snapshot| !snapshot.settings.is_interface_element_hidden(element),
            move |visible| change(InterfaceChange::ElementVisible { element, visible }),
        ));
    }
    group
}

fn quick_terminal(rows: &mut Rows) -> adw::PreferencesGroup {
    let group = rows::group("Quick Terminal");
    let sizes: Vec<(String, Option<i64>)> = std::iter::once(("Default".to_owned(), None))
        .chain(
            SIZE_PERCENT_CHOICES
                .iter()
                .map(|percent| (format!("{percent}% of screen"), Some(*percent))),
        )
        .collect();
    group.add(&rows::choice(
        rows,
        "settings-quick-terminal-size",
        "Size",
        move |_| sizes.clone(),
        |snapshot| snapshot.settings.effective_quick_terminal_size_percent(),
        |percent| change(InterfaceChange::QuickTerminalSizePercent(percent)),
    ));
    group
}
