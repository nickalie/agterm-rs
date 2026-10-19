//! General: Mouse (scroll speed, right-click paste, where links open, the workspace-row click) and Sessions (where new
//! sessions open and land, what names an unnamed session, the restore mode, the close confirmations, the flagged view
//! layout, the global ghostty config) (upstream's `GeneralSettingsView`).

use agterm_core::persist::RestoreMode;
use agterm_core::persist::settings::{
    FlaggedViewLayout, LinkOpenMode, NewSessionDirectory, NewSessionPlacementSetting,
};
use agterm_core::settings_model::{GeneralChange, SettingsChange};

use super::Snapshot;
use super::form::{Action, Control, Item, Run, Section, SliderKey, picker, toggle};
use crate::folder_chooser::FolderRequest;

fn change(change: GeneralChange) -> SettingsChange {
    SettingsChange::General(change)
}

/// The page's sections.
#[must_use]
pub fn sections(snapshot: &Snapshot) -> Vec<Section> {
    vec![
        Section::new("Mouse", mouse(snapshot)),
        Section::new("Sessions", sessions(snapshot)),
    ]
}

fn mouse(snapshot: &Snapshot) -> Vec<Item> {
    let settings = &snapshot.settings;
    vec![
        SliderKey::ScrollSpeed.item("Scroll speed", settings),
        toggle(
            "settings-right-click-paste",
            "Right-click pastes",
            settings.right_click_paste.unwrap_or(true),
            |on| change(GeneralChange::RightClickPaste(on)),
        ),
        picker(
            "settings-link-open-mode",
            "Open links in",
            &[
                ("Browser", LinkOpenMode::Browser),
                ("Session overlay", LinkOpenMode::Overlay),
            ],
            &settings.effective_link_open_mode(),
            |mode| change(GeneralChange::LinkOpenMode(mode)),
        ),
        toggle(
            "settings-workspace-row-click-expands",
            "Click a workspace row to expand or collapse",
            settings.workspace_row_click_expands.unwrap_or(true),
            |on| change(GeneralChange::WorkspaceRowClickExpands(on)),
        ),
    ]
}

fn sessions(snapshot: &Snapshot) -> Vec<Item> {
    let settings = &snapshot.settings;
    let mut items = vec![picker(
        "settings-new-session-directory",
        "New sessions open in",
        &[
            ("Home directory", NewSessionDirectory::Home),
            ("Current session's directory", NewSessionDirectory::CurrentSession),
            ("Custom directory", NewSessionDirectory::Custom),
        ],
        &settings.effective_new_session_directory(),
        |mode| change(GeneralChange::NewSessionDirectory(mode)),
    )];
    if settings.effective_new_session_directory() == NewSessionDirectory::Custom {
        items.push(custom_directory(snapshot));
    }
    items.push(toggle(
        "settings-session-name-from-title",
        "Name sessions after the terminal title",
        settings.session_name_from_terminal_title.unwrap_or(false),
        |on| change(GeneralChange::SessionNameFromTerminalTitle(on)),
    ));
    items.push(Item::hint(
        "settings-session-name-from-title-hint",
        "Otherwise an unnamed session is named after its directory.",
    ));
    items.push(picker(
        "settings-new-session-placement",
        "New sessions are added",
        &[
            ("At the end of the workspace", NewSessionPlacementSetting::End),
            ("After the current session", NewSessionPlacementSetting::AfterCurrent),
        ],
        &settings.effective_new_session_placement(),
        |placement| change(GeneralChange::NewSessionPlacement(placement)),
    ));
    items.extend(restore_mode(snapshot));
    items.extend(close_and_layout(snapshot));
    items
}

/// The `custom` mode's fixed directory, "Not set" for an unset or blank one, with Choose….
fn custom_directory(snapshot: &Snapshot) -> Item {
    let custom = custom_dir(snapshot);
    let start = snapshot.chooser_start(custom);
    Item::new(
        "settings-new-session-custom-dir",
        "",
        Control::Path {
            text: custom.unwrap_or("Not set").to_owned(),
            unset: custom.is_none(),
            buttons: vec![Action {
                id: "settings-new-session-choose",
                label: "Choose…",
                run: Run::Choose {
                    request: FolderRequest {
                        title: "Choose a directory for new sessions",
                        accept: "Choose",
                        start,
                    },
                    change: |path| change(GeneralChange::NewSessionCustomDirectory(path)),
                },
            }],
        },
    )
}

/// The directory to show, treating an unset or blank one alike (upstream's `customDirectory`).
fn custom_dir(snapshot: &Snapshot) -> Option<&str> {
    snapshot
        .settings
        .new_session_custom_directory
        .as_deref()
        .filter(|dir| !dir.is_empty())
}

/// The restore mode, which applies after a restart, and under a refused `live` the reason.
fn restore_mode(snapshot: &Snapshot) -> Vec<Item> {
    let mode = snapshot.settings.effective_restore_mode();
    let modes: Vec<(&str, RestoreMode)> = [RestoreMode::None, RestoreMode::Rerun, RestoreMode::Live]
        .into_iter()
        .map(|mode| (mode.display_name(), mode))
        .collect();
    let mut items = vec![
        picker("settings-restore-mode", "Restore sessions", &modes, &mode, |mode| {
            change(GeneralChange::RestoreMode(mode))
        }),
        Item::hint(
            "settings-restore-restart-hint",
            "Changes apply after restarting agterm.",
        ),
    ];
    if mode == RestoreMode::Live
        && let Some(reason) = &snapshot.live_unavailable
    {
        items.push(Item::hint("settings-restore-live-unavailable", reason.clone()));
    }
    items
}

fn close_and_layout(snapshot: &Snapshot) -> Vec<Item> {
    let settings = &snapshot.settings;
    vec![
        toggle(
            "settings-confirm-close-session",
            "Confirm before closing a session",
            settings.confirm_close_session.unwrap_or(false),
            |on| change(GeneralChange::ConfirmCloseSession(on)),
        ),
        toggle(
            "settings-close-grace-undo",
            "Allow undo after closing sessions and workspaces",
            settings.close_grace_undo_enabled.unwrap_or(true),
            |on| change(GeneralChange::CloseGraceUndo(on)),
        ),
        picker(
            "settings-flagged-view-layout",
            "Flagged view layout",
            &[
                ("Flat list", FlaggedViewLayout::Flat),
                ("Flat list, names only", FlaggedViewLayout::Plain),
                ("Workspace tree", FlaggedViewLayout::Tree),
            ],
            &settings.effective_flagged_view_layout(),
            |layout| change(GeneralChange::FlaggedViewLayout(layout)),
        ),
        toggle(
            "settings-inherit-global-ghostty",
            "Use my global Ghostty config",
            settings.inherit_global_ghostty_config.unwrap_or(false),
            |on| change(GeneralChange::InheritGlobalGhosttyConfig(on)),
        ),
        Item::hint(
            "settings-inherit-global-ghostty-hint",
            "Also loads ~/.config/ghostty/config.",
        ),
    ]
}
