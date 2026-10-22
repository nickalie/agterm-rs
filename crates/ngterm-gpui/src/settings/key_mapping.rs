//! Key Mapping: the config directory holding `keymap.conf` and `hooks.conf` (Choose…, and Use Default while one is
//! set), and each file's diagnostics with its Reload (upstream's `KeyMappingSettingsView`).

use ngterm_core::settings_model::SettingsChange;

use super::Snapshot;
use super::form::{Action, Control, Item, Run, Section};
use crate::folder_chooser::FolderRequest;

/// The page's sections.
#[must_use]
pub fn sections(snapshot: &Snapshot) -> Vec<Section> {
    vec![
        Section::new("Config Directory", directory(snapshot)),
        Section::new(
            "Diagnostics",
            diagnostics("settings-keymap", &snapshot.keymap_diagnostics, |core| {
                core.reload_keymap();
            }),
        ),
        Section::new(
            "Hooks",
            diagnostics("settings-hooks", &snapshot.hooks_diagnostics, |core| {
                core.reload_hooks();
            }),
        ),
    ]
}

/// The resolved directory, Choose… and, while one is set, Use Default; a change reloads both files.
fn directory(snapshot: &Snapshot) -> Vec<Item> {
    let mut buttons = vec![Action {
        id: "settings-keymap-choose",
        label: "Choose…",
        run: Run::Choose {
            request: FolderRequest {
                title: "Choose a directory for keymap.conf",
                accept: "Choose",
                start: snapshot.chooser_start(Some(&snapshot.config_dir)),
            },
            change: |path| SettingsChange::ConfigDirectory(Some(path)),
        },
    }];
    if snapshot.settings.config_directory.is_some() {
        buttons.push(Action {
            id: "settings-keymap-default",
            label: "Use Default",
            run: Run::Change(SettingsChange::ConfigDirectory(None)),
        });
    }
    vec![
        Item::new(
            "settings-keymap-directory",
            "",
            Control::Path {
                text: snapshot.config_dir.clone(),
                unset: false,
                buttons,
            },
        ),
        Item::hint(
            "settings-keymap-directory-hint",
            "The directory holding keymap.conf and hooks.conf. Changing it reloads both.",
        ),
    ]
}

/// One file's diagnostics (`<id>-diagnostics`) and its Reload (`<id>-reload`).
fn diagnostics(id: &'static str, lines: &[String], reload: fn(&mut ngterm_core::Core)) -> Vec<Item> {
    let (list_id, reload_id): (&'static str, &'static str) = match id {
        "settings-keymap" => ("settings-keymap-diagnostics", "settings-keymap-reload"),
        _ => ("settings-hooks-diagnostics", "settings-hooks-reload"),
    };
    vec![
        Item::new(list_id, "", Control::Diagnostics { lines: lines.to_vec() }),
        Item::new(
            reload_id,
            "",
            Control::Button(Action {
                id: reload_id,
                label: "Reload",
                run: Run::Core(reload),
            }),
        ),
    ]
}
