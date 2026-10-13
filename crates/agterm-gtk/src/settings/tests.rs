//! The dialog's pages and how its controls follow the settings. A dialog builds its widget tree only once presented,
//! so the tests look into the pages themselves.

use agterm_core::persist::AppSettings;

use super::*;
use crate::gtk_test::on_gtk_thread;

fn snapshot(settings: AppSettings) -> Snapshot {
    Snapshot {
        settings,
        dark: false,
        config_dir: "/home/u/.config/agterm".to_owned(),
        keymap_diagnostics: Vec::new(),
        hooks_diagnostics: vec!["line 3: unknown event".to_owned()],
        live_unavailable: None,
    }
}

/// The pages, refreshed once from default settings, and how to refresh them again.
fn built() -> ([adw::PreferencesPage; 6], Open) {
    let mut rows = Rows::new(state::detached_poster());
    let catalogs = Catalogs {
        fonts: vec!["Mono".to_owned()],
        themes: ["Builtin Light".to_owned(), "Nord".to_owned()].into(),
    };
    let pages = pages(&mut rows, &catalogs);
    let open = Open {
        dialog: adw::PreferencesDialog::new(),
        refreshers: rows.refreshers,
        last: RefCell::new(None),
    };
    open.refresh(snapshot(AppSettings::default()));
    (pages, open)
}

/// The descendant of `root` named `name` (upstream's accessibility identifier), depth first.
fn find(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find(&widget, name) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn named(pages: &[adw::PreferencesPage], name: &str) -> gtk::Widget {
    pages
        .iter()
        .find_map(|page| find(page.upcast_ref(), name))
        .unwrap_or_else(|| panic!("no widget named {name}"))
}

fn combo(pages: &[adw::PreferencesPage], name: &str) -> adw::ComboRow {
    named(pages, name)
        .downcast::<adw::ComboRow>()
        .unwrap_or_else(|_| panic!("{name} is no combo row"))
}

#[test]
fn the_dialog_has_upstreams_six_pages_and_their_controls() {
    on_gtk_thread(|| {
        let (pages, _open) = built();
        let names: Vec<String> = pages
            .iter()
            .map(|page| page.name().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "general",
                "appearance",
                "interface",
                "notifications",
                "agent-status",
                "key-mapping"
            ]
        );
        for id in [
            "settings-scroll-speed",
            "settings-restore-mode",
            "settings-font-family",
            "settings-interface-split",
            "settings-quick-terminal-size",
            "settings-dock-bounce",
            "settings-status-shape-blocked",
            "settings-status-blocked",
            "settings-auto-follow",
            "settings-keymap-reload",
            "settings-hooks-diagnostics",
        ] {
            named(&pages, id);
        }
    });
}

#[test]
fn rows_appear_with_the_settings_that_need_them() {
    on_gtk_thread(|| {
        let (pages, open) = built();
        let custom = named(&pages, "settings-new-session-custom-dir");
        let alternate = combo(&pages, "settings-theme-dark");
        let use_default = named(&pages, "settings-keymap-default");
        assert!(!custom.is_visible());
        assert!(!alternate.is_visible());
        assert!(!use_default.is_visible());
        open.refresh(snapshot(AppSettings {
            new_session_directory: Some("custom".to_owned()),
            theme: Some("Builtin Light".to_owned()),
            dark_theme: Some("Nord".to_owned()),
            follow_system_appearance: Some(true),
            config_directory: Some("/elsewhere".to_owned()),
            ..AppSettings::default()
        }));
        assert!(custom.is_visible());
        assert!(alternate.is_visible());
        assert!(use_default.is_visible());
        assert_eq!(alternate.title(), "Dark theme");
        assert_eq!(alternate.selected(), 1, "Nord, the second bundled theme");
    });
}

#[test]
fn pickers_show_the_stored_value_and_the_theme_picker_offers_the_builtin_only_unfollowed() {
    on_gtk_thread(|| {
        let (pages, open) = built();
        let theme = combo(&pages, "settings-theme");
        let items = |row: &adw::ComboRow| row.model().map_or(0, |model| model.n_items());
        assert_eq!(items(&theme), 3, "default ghostty and two themes");
        assert_eq!(theme.selected(), 0);
        open.refresh(snapshot(AppSettings {
            theme: Some("Nord".to_owned()),
            dark_theme: Some("Nord".to_owned()),
            follow_system_appearance: Some(true),
            flagged_view_layout: Some("tree".to_owned()),
            ..AppSettings::default()
        }));
        assert_eq!(items(&theme), 2);
        assert_eq!(theme.selected(), 1);
        assert_eq!(combo(&pages, "settings-flagged-view-layout").selected(), 2);
        assert!(!refreshing(), "a refresh unmutes the handlers");
    });
}

#[test]
fn diagnostics_read_as_lines_or_no_issues() {
    on_gtk_thread(|| {
        let (pages, _open) = built();
        let label = |name| named(&pages, name).downcast::<gtk::Label>().expect("a label");
        assert_eq!(label("settings-keymap-diagnostics").label(), "No issues.");
        assert_eq!(label("settings-hooks-diagnostics").label(), "line 3: unknown event");
    });
    assert_eq!(
        diagnostic_line(&KeymapDiagnostic::new(0, "could not read keymap.conf")),
        "could not read keymap.conf"
    );
    assert_eq!(
        diagnostic_line(&KeymapDiagnostic::new(4, "unknown action")),
        "line 4: unknown action"
    );
}
