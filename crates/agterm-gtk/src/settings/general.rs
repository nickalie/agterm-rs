//! General: Mouse (scroll speed, right-click paste, where links open, the workspace-row click) and Sessions (where new
//! sessions open and land, the restore mode, the close confirmations, the flagged view layout, the global ghostty
//! config) (upstream's `GeneralSettingsView`).

use adw::prelude::*;
use agterm_core::persist::RestoreMode;
use agterm_core::persist::settings::{
    FlaggedViewLayout, LinkOpenMode, NewSessionDirectory, NewSessionPlacementSetting,
};
use agterm_core::settings_model::{GeneralChange, SCROLL_SPEED_RANGE, SettingsChange};

use super::rows::{self, Slider};
use super::{Rows, Snapshot, send};

fn change(change: GeneralChange) -> SettingsChange {
    SettingsChange::General(change)
}

pub fn page(rows: &mut Rows) -> adw::PreferencesPage {
    let page = rows::page("General", "general", "preferences-system-symbolic");
    page.add(&mouse(rows));
    page.add(&sessions(rows));
    page
}

fn mouse(rows: &mut Rows) -> adw::PreferencesGroup {
    let group = rows::group("Mouse");
    group.add(&rows::slider(
        rows,
        "settings-scroll-speed",
        "Scroll speed",
        Slider {
            range: SCROLL_SPEED_RANGE,
            get: |snapshot: &Snapshot| snapshot.settings.effective_scroll_speed(),
            label: |snapshot: &Snapshot| format!("{}x", snapshot.settings.effective_scroll_speed()),
            set: |speed| change(GeneralChange::ScrollSpeed(speed)),
        },
    ));
    group.add(&rows::switch(
        rows,
        "settings-right-click-paste",
        "Right-click pastes",
        |snapshot| snapshot.settings.right_click_paste.unwrap_or(true),
        |on| change(GeneralChange::RightClickPaste(on)),
    ));
    group.add(&rows::picker(
        rows,
        "settings-link-open-mode",
        "Open links in",
        &[
            ("Browser", LinkOpenMode::Browser),
            ("Session overlay", LinkOpenMode::Overlay),
        ],
        |snapshot| snapshot.settings.effective_link_open_mode(),
        |mode| change(GeneralChange::LinkOpenMode(mode)),
    ));
    group.add(&rows::switch(
        rows,
        "settings-workspace-row-click-expands",
        "Click a workspace row to expand or collapse",
        |snapshot| snapshot.settings.workspace_row_click_expands.unwrap_or(true),
        |on| change(GeneralChange::WorkspaceRowClickExpands(on)),
    ));
    group
}

fn sessions(rows: &mut Rows) -> adw::PreferencesGroup {
    let group = rows::group("Sessions");
    group.add(&rows::picker(
        rows,
        "settings-new-session-directory",
        "New sessions open in",
        &[
            ("Home directory", NewSessionDirectory::Home),
            ("Current session's directory", NewSessionDirectory::CurrentSession),
            ("Custom directory", NewSessionDirectory::Custom),
        ],
        |snapshot| snapshot.settings.effective_new_session_directory(),
        |mode| change(GeneralChange::NewSessionDirectory(mode)),
    ));
    group.add(&custom_directory(rows));
    group.add(&rows::picker(
        rows,
        "settings-new-session-placement",
        "New sessions are added",
        &[
            ("At the end of the workspace", NewSessionPlacementSetting::End),
            ("After the current session", NewSessionPlacementSetting::AfterCurrent),
        ],
        |snapshot| snapshot.settings.effective_new_session_placement(),
        |placement| change(GeneralChange::NewSessionPlacement(placement)),
    ));
    group.add(&restore_mode(rows));
    add_close_and_layout(rows, &group);
    group
}

/// The `custom` mode's fixed directory, shown only in that mode; "Not set" for an unset or blank one.
fn custom_directory(rows: &mut Rows) -> adw::ActionRow {
    let row = rows::info(
        rows,
        "settings-new-session-custom-dir",
        "Custom directory",
        |snapshot| custom_dir(snapshot).unwrap_or("Not set").to_owned(),
    );
    let choose = rows::button("Choose…", "settings-new-session-choose");
    row.add_suffix(&choose);
    let current = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
    let poster = rows.poster().clone();
    let start = std::rc::Rc::clone(&current);
    choose.connect_clicked(move |button| {
        let poster = poster.clone();
        rows::choose_directory(
            button.upcast_ref(),
            "Choose a directory for new sessions",
            start.borrow().as_deref(),
            move |path| send(&poster, change(GeneralChange::NewSessionCustomDirectory(path))),
        );
    });
    rows.refresh(move |snapshot| *current.borrow_mut() = custom_dir(snapshot).map(str::to_owned));
    rows::show_when(rows, &row, |snapshot| {
        snapshot.settings.effective_new_session_directory() == NewSessionDirectory::Custom
    });
    row
}

fn custom_dir(snapshot: &Snapshot) -> Option<&str> {
    snapshot
        .settings
        .new_session_custom_directory
        .as_deref()
        .filter(|dir| !dir.is_empty())
}

/// The restore mode, which applies after a restart; under a refused `live` the subtitle says why.
fn restore_mode(rows: &mut Rows) -> adw::ComboRow {
    let modes: Vec<(&str, RestoreMode)> = [RestoreMode::None, RestoreMode::Rerun, RestoreMode::Live]
        .into_iter()
        .map(|mode| (mode.display_name(), mode))
        .collect();
    let row = rows::picker(
        rows,
        "settings-restore-mode",
        "Restore sessions",
        &modes,
        |snapshot| snapshot.settings.effective_restore_mode(),
        |mode| change(GeneralChange::RestoreMode(mode)),
    );
    let shown = row.clone();
    rows.refresh(move |snapshot| {
        let mut hint = "Changes apply after restarting agterm.".to_owned();
        if snapshot.settings.effective_restore_mode() == RestoreMode::Live
            && let Some(reason) = &snapshot.live_unavailable
        {
            hint.push(' ');
            hint.push_str(reason);
        }
        shown.set_subtitle(&rows::glib_escape(&hint));
    });
    row
}

fn add_close_and_layout(rows: &mut Rows, group: &adw::PreferencesGroup) {
    group.add(&rows::switch(
        rows,
        "settings-confirm-close-session",
        "Confirm before closing a session",
        |snapshot| snapshot.settings.confirm_close_session.unwrap_or(false),
        |on| change(GeneralChange::ConfirmCloseSession(on)),
    ));
    group.add(&rows::switch(
        rows,
        "settings-close-grace-undo",
        "Allow undo after closing sessions and workspaces",
        |snapshot| snapshot.settings.close_grace_undo_enabled.unwrap_or(true),
        |on| change(GeneralChange::CloseGraceUndo(on)),
    ));
    group.add(&rows::picker(
        rows,
        "settings-flagged-view-layout",
        "Flagged view layout",
        &[
            ("Flat list", FlaggedViewLayout::Flat),
            ("Flat list, names only", FlaggedViewLayout::Plain),
            ("Workspace tree", FlaggedViewLayout::Tree),
        ],
        |snapshot| snapshot.settings.effective_flagged_view_layout(),
        |layout| change(GeneralChange::FlaggedViewLayout(layout)),
    ));
    let global = rows::switch(
        rows,
        "settings-inherit-global-ghostty",
        "Use my global Ghostty config",
        |snapshot| snapshot.settings.inherit_global_ghostty_config.unwrap_or(false),
        |on| change(GeneralChange::InheritGlobalGhosttyConfig(on)),
    );
    global.set_subtitle("Also loads ~/.config/ghostty/config.");
    group.add(&global);
}
