//! Key Mapping: the config directory holding `keymap.conf` and `hooks.conf`, and each file's diagnostics with its
//! Reload (upstream's `KeyMappingSettingsView`).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use agterm_core::settings_model::SettingsChange;

use super::{Rows, Snapshot, act, rows, send};

pub fn page(rows: &mut Rows) -> adw::PreferencesPage {
    let page = rows::page("Key Mapping", "key-mapping", "input-keyboard-symbolic");
    page.add(&directory(rows));
    page.add(&diagnostics(
        rows,
        Diagnostics {
            title: "Diagnostics",
            id: "settings-keymap",
            lines: |snapshot| &snapshot.keymap_diagnostics,
            reload: |core| {
                core.reload_keymap();
            },
        },
    ));
    page.add(&diagnostics(
        rows,
        Diagnostics {
            title: "Hooks",
            id: "settings-hooks",
            lines: |snapshot| &snapshot.hooks_diagnostics,
            reload: |core| {
                core.reload_hooks();
            },
        },
    ));
    page
}

/// The resolved directory, Choose… and, while one is set, Use Default; a change reloads both files.
fn directory(rows: &mut Rows) -> adw::PreferencesGroup {
    let group = rows::group("Config Directory");
    group.set_description(Some(
        "The directory holding keymap.conf and hooks.conf. Changing it reloads both.",
    ));
    let row = rows::info(rows, "settings-keymap-directory", "Directory", |snapshot| {
        snapshot.config_dir.clone()
    });
    let choose = rows::button("Choose…", "settings-keymap-choose");
    let default = rows::button("Use Default", "settings-keymap-default");
    row.add_suffix(&choose);
    row.add_suffix(&default);
    let current = Rc::new(RefCell::new(String::new()));
    let poster = rows.poster().clone();
    let start = Rc::clone(&current);
    choose.connect_clicked(move |button| {
        let poster = poster.clone();
        rows::choose_directory(
            button.upcast_ref(),
            "Choose a directory for keymap.conf",
            Some(start.borrow().as_str()),
            move |path| send(&poster, SettingsChange::ConfigDirectory(Some(path))),
        );
    });
    let poster = rows.poster().clone();
    default.connect_clicked(move |_| send(&poster, SettingsChange::ConfigDirectory(None)));
    rows.refresh(move |snapshot| current.borrow_mut().clone_from(&snapshot.config_dir));
    rows::show_when(rows, &default, |snapshot| snapshot.settings.config_directory.is_some());
    group.add(&row);
    group
}

/// One file's diagnostics section.
#[derive(Clone, Copy)]
struct Diagnostics {
    title: &'static str,
    /// The id prefix: `<id>-diagnostics`, `<id>-reload`.
    id: &'static str,
    lines: fn(&Snapshot) -> &Vec<String>,
    reload: fn(&mut agterm_core::Core),
}

/// The diagnostics as one selectable text, "No issues." when there are none, and the file's Reload.
fn diagnostics(rows: &mut Rows, section: Diagnostics) -> adw::PreferencesGroup {
    let Diagnostics {
        title,
        id,
        lines,
        reload,
    } = section;
    let group = rows::group(title);
    let text = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .name(format!("{id}-diagnostics"))
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    text.add_css_class("monospace");
    let shown = text.clone();
    rows.refresh(move |snapshot| {
        let lines = lines(snapshot);
        let empty = lines.is_empty();
        shown.set_label(&if empty {
            "No issues.".to_owned()
        } else {
            lines.join("\n")
        });
        if empty {
            shown.add_css_class("dim-label");
        } else {
            shown.remove_css_class("dim-label");
        }
    });
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
    list.add_css_class("boxed-list");
    list.append(&text);
    group.add(&list);
    let button = rows::button("Reload", &format!("{id}-reload"));
    let poster = rows.poster().clone();
    button.connect_clicked(move |_| act(&poster, reload));
    group.set_header_suffix(Some(&button));
    group
}
