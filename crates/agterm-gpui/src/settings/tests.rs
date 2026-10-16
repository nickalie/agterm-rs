//! The Settings popup: upstream's six tabs and their controls (ported from the GTK build's tests), every control on
//! every page writing through the core and reading back what the core then holds, and the popup's view in GPUI's test
//! platform over a real core with a state directory of its own: the pointer on its tabs, toggles, steppers, picker
//! menus and buttons, its sliders and color wells, a change made elsewhere showing in it, an edit the core leaves
//! unchanged snapping a moved slider back, its size in a window, and Escape and its close button asking to close it.
//! The popup over an agterm window (opening, closing, the keyboard, one at a time) is `crate::window`'s and
//! `crate::windows`' to test. No test presses a Choose… button: it opens the desktop's folder chooser.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agterm_core::model::{AgentStatus, StatusShape};
use agterm_core::paths::{ConfigLocator, MapEnv};
use agterm_core::persist::settings::{LinkOpenMode, NewSessionDirectory};
use agterm_core::persist::{AppSettings, LibraryPersistence, RestoreMode};
use agterm_core::settings_model::{AppearanceChange, GeneralChange};
use agterm_core::{CoreConfig, CoreState};
use agterm_host::{MainLoop, MainLoopClosed, MainTask};
use agterm_proto::AppIdentity;
use gpui_kit::component::slider::SliderValue;
use gpui_kit::{Bounds, Modifiers, Pixels, TestAppContext, VisualTestContext, px, size};

use super::form::Run;
use super::*;

// MARK: - the pages

fn catalogs() -> Catalogs {
    Catalogs {
        fonts: vec!["Iosevka".to_owned(), "Mono".to_owned()].into(),
        themes: vec!["Builtin Light".to_owned(), "Nord".to_owned()].into(),
    }
}

fn snapshot(settings: AppSettings) -> Snapshot {
    Snapshot {
        settings,
        dark: false,
        config_dir: "/home/u/.config/agterm".to_owned(),
        keymap_diagnostics: Vec::new(),
        hooks_diagnostics: vec!["line 3: unknown event".to_owned()],
        live_unavailable: None,
        session_cwd: None,
    }
}

/// Every row of every page, by tab.
fn every_item(snapshot: &Snapshot) -> Vec<(Tab, Item)> {
    let catalogs = catalogs();
    Tab::ALL
        .into_iter()
        .flat_map(|tab| {
            tab.sections(snapshot, &catalogs)
                .into_iter()
                .flat_map(move |section| section.items.into_iter().map(move |item| (tab, item)))
        })
        .collect()
}

fn item<'a>(items: &'a [(Tab, Item)], id: &str) -> &'a Item {
    items
        .iter()
        .map(|(_, item)| item)
        .find(|item| item.id == id)
        .unwrap_or_else(|| panic!("no row {id}"))
}

fn has(snapshot: &Snapshot, id: &str) -> bool {
    every_item(snapshot).iter().any(|(_, item)| item.id == id)
}

fn picker_of(item: &Item) -> &Picker {
    match &item.control {
        Control::Picker(picker) | Control::Glyph { shape: picker, .. } => picker,
        _ => panic!("{} is no picker", item.id),
    }
}

/// The settings upstream's seeded UI test shows every row under: the custom directory, following with both slots, a
/// config directory set.
fn everything_shown() -> AppSettings {
    AppSettings {
        new_session_directory: Some("custom".to_owned()),
        new_session_custom_directory: Some("/srv".to_owned()),
        theme: Some("Builtin Light".to_owned()),
        dark_theme: Some("Nord".to_owned()),
        follow_system_appearance: Some(true),
        config_directory: Some("/elsewhere".to_owned()),
        ..AppSettings::default()
    }
}

#[test]
fn the_window_has_upstreams_six_tabs_in_order() {
    let titles: Vec<&str> = Tab::ALL.iter().map(|tab| tab.title()).collect();
    assert_eq!(
        titles,
        [
            "General",
            "Appearance",
            "Interface",
            "Notifications",
            "Agent Status",
            "Key Mapping"
        ]
    );
    let names: Vec<&str> = Tab::ALL.iter().map(|tab| tab.name()).collect();
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
    for glyph in GLYPHS {
        assert!(crate::titlebar::serves(&glyph.path()), "{} is served", glyph.path());
    }
    for tab in Tab::ALL {
        assert!(
            crate::titlebar::serves(&tab.glyph().path()),
            "{tab:?}'s glyph is served"
        );
    }
}

#[test]
fn every_upstream_control_id_names_a_row() {
    let shown = snapshot(everything_shown());
    let mut ids = vec![
        "settings-scroll-speed",
        "settings-right-click-paste",
        "settings-link-open-mode",
        "settings-workspace-row-click-expands",
        "settings-new-session-directory",
        "settings-new-session-custom-dir",
        "settings-new-session-placement",
        "settings-restore-mode",
        "settings-restore-restart-hint",
        "settings-confirm-close-session",
        "settings-close-grace-undo",
        "settings-flagged-view-layout",
        "settings-inherit-global-ghostty",
        "settings-font-family",
        "settings-font-size",
        "settings-cursor-style",
        "settings-cursor-blink",
        "settings-theme",
        "settings-follow-appearance",
        "settings-theme-dark",
        "settings-toolbar-mode",
        "settings-bg-opacity",
        "settings-bg-blur",
        "settings-sidebar-shift",
        "settings-sidebar-font-size",
        "settings-interface-font-size",
        "settings-inactive-pane-mute",
        "settings-auto-hide-inactive-sidebars",
        "settings-quick-terminal-size",
        "settings-notifications",
        "settings-notification-badges",
        "settings-dock-bounce",
        "settings-notification-sound",
        "settings-attention-button",
        "settings-status-blocked-sound",
        "settings-status-clear",
        "settings-auto-follow",
        "settings-auto-follow-stay-active",
        "settings-status-reset",
        "settings-keymap-directory",
        "settings-keymap-diagnostics",
        "settings-keymap-reload",
        "settings-hooks-diagnostics",
        "settings-hooks-reload",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    ids.extend(
        agterm_core::persist::settings::InterfaceElement::ALL
            .iter()
            .map(|element| format!("settings-interface-{}", element.raw())),
    );
    ids.extend(
        GLYPH_STATUSES
            .iter()
            .map(|status| format!("settings-status-shape-{}", status.wire_name())),
    );
    for id in &ids {
        assert!(has(&shown, id), "{id}");
    }
    // the buttons of the path rows
    let items = every_item(&shown);
    let buttons = |id: &str| -> Vec<&'static str> {
        match &item(&items, id).control {
            Control::Path { buttons, .. } => buttons.iter().map(|action| action.id).collect(),
            _ => panic!("{id} is no path"),
        }
    };
    assert_eq!(
        buttons("settings-new-session-custom-dir"),
        ["settings-new-session-choose"]
    );
    assert_eq!(
        buttons("settings-keymap-directory"),
        ["settings-keymap-choose", "settings-keymap-default"]
    );
}

#[test]
fn every_page_but_interface_lays_its_rows_one_per_row() {
    let shown = snapshot(AppSettings::default());
    let catalogs = catalogs();
    for tab in Tab::ALL {
        for section in tab.sections(&shown, &catalogs) {
            let expected = if tab == Tab::Interface && matches!(section.title, Some("Title Bar" | "Sidebar")) {
                2
            } else {
                1
            };
            assert_eq!(section.columns, expected, "{tab:?} {:?}", section.title);
        }
    }
}

#[test]
fn rows_appear_with_the_settings_that_need_them() {
    let plain = snapshot(AppSettings::default());
    for id in [
        "settings-new-session-custom-dir",
        "settings-theme-dark",
        "settings-restore-live-unavailable",
    ] {
        assert!(!has(&plain, id), "{id} hides by default");
    }
    let items = every_item(&plain);
    let Control::Path { buttons, .. } = &item(&items, "settings-keymap-directory").control else {
        panic!("a path");
    };
    assert_eq!(buttons.len(), 1, "no Use Default without a directory set");

    let shown = snapshot(everything_shown());
    let items = every_item(&shown);
    let alternate = item(&items, "settings-theme-dark");
    assert_eq!(alternate.title, "Dark theme");
    assert_eq!(picker_of(alternate).selected, Some(1), "Nord, the second bundled theme");
    let Control::Path { text, unset, .. } = &item(&items, "settings-new-session-custom-dir").control else {
        panic!("a path");
    };
    assert_eq!((text.as_str(), *unset), ("/srv", false));
    // in dark mode the second picker edits the light slot
    let dark = Snapshot {
        dark: true,
        ..shown.clone()
    };
    let items = every_item(&dark);
    assert_eq!(item(&items, "settings-theme-dark").title, "Light theme");
    assert_eq!(
        item(&items, "settings-theme-dark-hint").title,
        "Used when the desktop is in light mode."
    );
}

#[test]
fn a_blank_custom_directory_reads_not_set() {
    let blank = snapshot(AppSettings {
        new_session_directory: Some("custom".to_owned()),
        new_session_custom_directory: Some(String::new()),
        ..AppSettings::default()
    });
    let items = every_item(&blank);
    let Control::Path { text, unset, .. } = &item(&items, "settings-new-session-custom-dir").control else {
        panic!("a path");
    };
    assert_eq!((text.as_str(), *unset), ("Not set", true));
}

#[test]
fn the_live_refusal_reason_follows_the_restart_hint() {
    let live = Snapshot {
        live_unavailable: Some("zmx is missing.".to_owned()),
        ..snapshot(AppSettings {
            restore_mode: Some(RestoreMode::Live),
            ..AppSettings::default()
        })
    };
    let items = every_item(&live);
    let ids: Vec<&str> = items.iter().map(|(_, item)| item.id.as_str()).collect();
    let hint = ids
        .iter()
        .position(|id| *id == "settings-restore-restart-hint")
        .unwrap();
    assert_eq!(ids[hint + 1], "settings-restore-live-unavailable");
    assert_eq!(
        item(&items, "settings-restore-live-unavailable").title,
        "zmx is missing."
    );
    assert_eq!(
        item(&items, "settings-restore-restart-hint").title,
        "Changes apply after restarting agterm."
    );
}

#[test]
fn pickers_show_the_stored_value_and_the_theme_picker_offers_the_builtin_only_unfollowed() {
    let items = every_item(&snapshot(AppSettings::default()));
    let theme = picker_of(item(&items, "settings-theme"));
    assert_eq!(theme.options, ["default ghostty", "Builtin Light", "Nord"]);
    assert_eq!(theme.selected, Some(0));
    let items = every_item(&snapshot(AppSettings {
        theme: Some("Nord".to_owned()),
        dark_theme: Some("Nord".to_owned()),
        follow_system_appearance: Some(true),
        flagged_view_layout: Some("tree".to_owned()),
        ..AppSettings::default()
    }));
    let theme = picker_of(item(&items, "settings-theme"));
    assert_eq!(theme.options.len(), 2);
    assert_eq!(theme.selected, Some(1));
    assert_eq!(
        picker_of(item(&items, "settings-flagged-view-layout")).selected,
        Some(2)
    );
    assert_eq!(
        picker_of(item(&items, "settings-flagged-view-layout")).label(),
        "Workspace tree"
    );
}

#[test]
fn the_font_and_theme_pickers_keep_a_set_value_the_catalog_lacks() {
    let fonts = ["Iosevka".to_owned(), "Mono".to_owned()];
    let labels = |configured| -> Vec<String> {
        appearance::font_options(&fonts, configured)
            .into_iter()
            .map(|(label, _)| label)
            .collect()
    };
    assert_eq!(labels(None), ["Default", "Iosevka", "Mono"]);
    assert_eq!(labels(Some("Mono")), ["Default", "Iosevka", "Mono"]);
    assert_eq!(labels(Some("Gone")), ["Default", "Iosevka", "Mono", "Gone"]);
    let themes = ["Nord".to_owned()];
    let names: Vec<String> = appearance::theme_options(&themes, false, Some("Gone"))
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(names, ["Nord", "Gone"]);
}

#[test]
fn controls_that_need_another_setting_take_no_input_without_it() {
    let items = every_item(&snapshot(AppSettings::default()));
    assert!(
        !item(&items, "settings-bg-blur").enabled,
        "blur needs opacity below 100%"
    );
    assert!(
        !item(&items, "settings-auto-follow-stay-active").enabled,
        "auto-follow is off"
    );
    let items = every_item(&snapshot(AppSettings {
        background_opacity: Some(0.8),
        auto_follow_attention: Some("s10".to_owned()),
        ..AppSettings::default()
    }));
    assert!(item(&items, "settings-bg-blur").enabled);
    assert!(item(&items, "settings-auto-follow-stay-active").enabled);
}

#[test]
fn diagnostics_read_as_lines_or_no_issues() {
    let items = every_item(&snapshot(AppSettings::default()));
    let lines = |id| match &item(&items, id).control {
        Control::Diagnostics { lines } => lines.clone(),
        _ => panic!("{id} is no diagnostics"),
    };
    assert!(lines("settings-keymap-diagnostics").is_empty(), "drawn as No issues.");
    assert_eq!(lines("settings-hooks-diagnostics"), ["line 3: unknown event"]);
    assert_eq!(
        diagnostic_line(&KeymapDiagnostic::new(0, "could not read keymap.conf")),
        "could not read keymap.conf"
    );
    assert_eq!(
        diagnostic_line(&KeymapDiagnostic::new(4, "unknown action")),
        "line 4: unknown action"
    );
}

#[test]
fn sliders_read_their_setting_and_move_it_in_whole_steps() {
    let settings = AppSettings {
        background_opacity: Some(0.375),
        background_blur: Some(12),
        sidebar_background_shift: Some(3),
        ..AppSettings::default()
    };
    let read: Vec<(f64, String)> = SliderKey::ALL
        .iter()
        .map(|key| (key.value(&settings), key.readout(&settings)))
        .collect();
    assert_eq!(
        read,
        [
            (3.0, "3x".to_owned()),
            (38.0, "38%".to_owned()),
            (12.0, "12".to_owned()),
            (3.0, "Lighter 2".to_owned()),
            (5.0, "5".to_owned()),
        ]
    );
    assert_eq!(
        SliderKey::BackgroundOpacity.change(42.4),
        SettingsChange::Appearance(AppearanceChange::BackgroundOpacity(0.42))
    );
    assert_eq!(
        SliderKey::BackgroundBlur.change(4.6),
        SettingsChange::Appearance(AppearanceChange::BackgroundBlur(5))
    );
    assert_eq!(form::whole(9.5), 10);
}

#[test]
fn colors_are_stored_as_lower_case_hex_and_statuses_title_cased() {
    assert_eq!(hex_of(crate::status_glyph::hsla((1.0, 0.5, 0.0))), "#ff8000");
    assert_eq!(agent_status::title_case(AgentStatus::Blocked.wire_name()), "Blocked");
    assert_eq!(agent_status::title_case("diamond"), "Diamond");
    let settings = AppSettings {
        blocked_status_color_hex: Some("#102030".to_owned()),
        ..AppSettings::default()
    };
    let rgb = agent_status::glyph_rgb(&settings, AgentStatus::Blocked);
    assert_eq!(hex_of(crate::status_glyph::hsla(rgb)), "#102030");
    assert_eq!(
        agent_status::glyph_rgb(&AppSettings::default(), AgentStatus::Completed),
        crate::status_glyph::default_rgb(AgentStatus::Completed).unwrap()
    );
}

#[test]
fn a_chooser_starts_in_the_set_directory_else_the_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let shown = Snapshot {
        session_cwd: Some(cwd.clone()),
        ..snapshot(AppSettings::default())
    };
    assert_eq!(
        shown.chooser_start(Some("/no/such/dir")),
        Some(dir.path().to_path_buf())
    );
    assert_eq!(shown.chooser_start(Some("/")), Some(std::path::PathBuf::from("/")));
    assert_eq!(snapshot(AppSettings::default()).chooser_start(None), None);
}

// MARK: - every control round-trips through the core

fn core_in(dir: &Path, settings: AppSettings) -> Core {
    let mut persistence = LibraryPersistence::new(dir.join("state"));
    let library = persistence.bootstrap("/home/u");
    let mut core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings,
            persistence: Some(persistence),
        },
    );
    core.set_config_locator(ConfigLocator::capture(&MapEnv::new().with("HOME", dir.as_os_str())));
    core
}

/// What a row shows, to compare before and after an edit.
#[derive(Debug, Clone, PartialEq)]
enum Shown {
    On(bool),
    Row(Option<usize>),
    Value(f64),
    Text(String),
}

fn shown(item: &Item) -> Option<Shown> {
    match &item.control {
        Control::Toggle { on, .. } => Some(Shown::On(*on)),
        Control::Picker(picker) | Control::Glyph { shape: picker, .. } => Some(Shown::Row(picker.selected)),
        Control::Stepper { value, .. } | Control::Slider { value, .. } => Some(Shown::Value(*value)),
        Control::Path { text, .. } => Some(Shown::Text(text.clone())),
        _ => None,
    }
}

/// The edit that moves `item` off what it shows, and what it then shows; `None` for a row with nothing to edit.
fn moved(item: &Item, dir: &Path) -> Option<(SettingsChange, Shown)> {
    match &item.control {
        Control::Toggle { on, set } => Some((set(!on), Shown::On(!on))),
        Control::Picker(picker) | Control::Glyph { shape: picker, .. } => {
            let count = picker.options.len();
            (count > 1).then(|| {
                let next = picker.selected.map_or(0, |row| (row + 1) % count);
                (picker.pick(next).unwrap(), Shown::Row(Some(next)))
            })
        }
        Control::Stepper { value, range, set } => {
            let to = if value < range.end() { value + 1.0 } else { value - 1.0 };
            Some((set(to), Shown::Value(to)))
        }
        Control::Slider { key, value, .. } => {
            let to = if *value >= key.bounds().1 {
                value - 1.0
            } else {
                value + 1.0
            };
            Some((key.change(to), Shown::Value(to)))
        }
        Control::Path { buttons, .. } => buttons.iter().find_map(|action| match &action.run {
            Run::Choose { change, .. } => {
                let picked = dir.join(action.id).to_string_lossy().into_owned();
                Some((change(picked.clone()), Shown::Text(picked)))
            }
            _ => None,
        }),
        _ => None,
    }
}

/// Every editable row of every page, under default and under everything-shown settings: its edit goes through the
/// core, and the row then shows the new value, read back from what the core holds (and wrote to `settings.json`).
#[test]
fn every_control_writes_through_the_core_and_reads_back() {
    for start in [AppSettings::default(), everything_shown()] {
        let dir = tempfile::tempdir().unwrap();
        let mut core = core_in(dir.path(), start.clone());
        let ids: Vec<String> = every_item(&Snapshot::of(&core, false))
            .into_iter()
            .map(|(_, item)| item.id)
            .collect();
        let mut edited = 0;
        for id in ids {
            let before = Snapshot::of(&core, false);
            let items = every_item(&before);
            let Some(row) = items.iter().map(|(_, item)| item).find(|item| item.id == id) else {
                continue; // a row an earlier edit hid
            };
            let Some((change, expected)) = moved(row, dir.path()) else {
                continue;
            };
            core.change_settings(change.clone());
            let after = every_item(&Snapshot::of(&core, false));
            let row = after.iter().map(|(_, item)| item).find(|item| item.id == id);
            assert_eq!(row.and_then(shown), Some(expected), "{id} after {change:?}");
            edited += 1;
        }
        assert!(edited > 50, "only {edited} rows edited");
        let stored = agterm_core::persist::SettingsStore::new(dir.path().join("state")).load();
        assert_eq!(stored, *core.settings(), "every edit was written");
    }
}

#[test]
fn the_buttons_reset_the_glyphs_drop_the_config_directory_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core_in(
        dir.path(),
        AppSettings {
            blocked_status_color_hex: Some("#102030".to_owned()),
            completed_status_shape: Some("diamond".to_owned()),
            config_directory: Some(dir.path().join("keys").to_string_lossy().into_owned()),
            ..AppSettings::default()
        },
    );
    let run = |core: &mut Core, id: &str| {
        let items = every_item(&Snapshot::of(core, false));
        let action = items
            .iter()
            .find_map(|(_, item)| match &item.control {
                Control::Button(action) if action.id == id => Some(action.run.clone()),
                Control::Path { buttons, .. } => buttons.iter().find(|action| action.id == id).map(|a| a.run.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no button {id}"));
        match action {
            Run::Change(change) => {
                core.change_settings(change);
            }
            Run::Core(task) => task(core),
            Run::Choose { .. } => panic!("{id} chooses"),
        }
    };
    run(&mut core, "settings-status-reset");
    assert_eq!(core.settings().blocked_status_color_hex, None);
    assert_eq!(core.settings().completed_status_shape, None);
    run(&mut core, "settings-keymap-default");
    assert_eq!(core.settings().config_directory, None);
    assert!(
        !every_item(&Snapshot::of(&core, false))
            .iter()
            .any(|(_, item)| match &item.control {
                Control::Path { buttons, .. } => buttons.iter().any(|action| action.id == "settings-keymap-default"),
                _ => false,
            })
    );
    // a broken keymap.conf in the default directory shows after its Reload
    let config = core.config_dir();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("keymap.conf"), "map ctrl+q nonsense_action\n").unwrap();
    run(&mut core, "settings-keymap-reload");
    let shown = Snapshot::of(&core, false);
    assert_eq!(shown.keymap_diagnostics.len(), 1, "{:?}", shown.keymap_diagnostics);
    assert!(
        shown.keymap_diagnostics[0].starts_with("line 1: "),
        "{:?}",
        shown.keymap_diagnostics
    );
    std::fs::write(config.join("hooks.conf"), "nonsense\n").unwrap();
    run(&mut core, "settings-hooks-reload");
    assert!(!Snapshot::of(&core, false).hooks_diagnostics.is_empty());
}

// MARK: - the window

/// The main loop the window posts to: tasks wait for the test to run them; timers are not exercised.
#[derive(Default)]
struct Posted(Mutex<Vec<MainTask<AppState>>>);

impl MainLoop<AppState> for Posted {
    fn post(&self, task: MainTask<AppState>) -> Result<(), MainLoopClosed> {
        self.0.lock().unwrap().push(task);
        Ok(())
    }

    fn post_after(&self, _: Duration, _: MainTask<AppState>) -> Result<(), MainLoopClosed> {
        Ok(())
    }
}

/// The window over a real core with a state directory of its own.
struct Harness {
    state: AppState,
    posted: Arc<Posted>,
    view: Entity<SettingsView>,
    dir: tempfile::TempDir,
}

fn harness(cx: &mut TestAppContext, settings: AppSettings) -> (Harness, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    let dir = tempfile::tempdir().unwrap();
    let posted = Arc::new(Posted::default());
    let weak_loop: Poster = Arc::downgrade(&posted) as Poster;
    let mut state = AppState::new(core_in(dir.path(), settings));
    state.attach(weak_loop);
    let opening = Opening::of(&state);
    let (view, cx) = cx
        .add_window_view(move |window, cx| SettingsView::new(opening.poster, opening.snapshot, catalogs(), window, cx));
    let (width, height) = PANEL_SIZE;
    cx.simulate_resize(size(px(width), px(height)));
    let mut harness = Harness {
        state,
        posted,
        view,
        dir,
    };
    harness.settle(cx);
    (harness, cx)
}

impl Harness {
    /// Runs what the window posted against the app state and syncs the window after each round, as the windows' sync
    /// does after every task, until nothing more is posted.
    fn settle(&mut self, cx: &mut VisualTestContext) {
        for _ in 0..8 {
            cx.run_until_parked();
            let tasks: Vec<MainTask<AppState>> = std::mem::take(&mut *self.posted.0.lock().unwrap());
            for task in tasks {
                task(&mut self.state);
            }
            let snapshot = Snapshot::of(&self.state.core, self.state.applied_dark());
            self.view
                .update_in(cx, |view, window, cx| view.sync(snapshot, window, cx));
            cx.run_until_parked();
            if self.posted.0.lock().unwrap().is_empty() {
                return;
            }
        }
    }

    fn settings(&self) -> &AppSettings {
        self.state.core.settings()
    }

    /// `settings.json` as written.
    fn stored(&self) -> AppSettings {
        agterm_core::persist::SettingsStore::new(self.dir.path().join("state")).load()
    }

    fn item(&self, cx: &mut VisualTestContext, id: &str) -> Option<Shown> {
        self.view.read_with(cx, |view, _| {
            view.sections()
                .iter()
                .flat_map(|section| section.items.iter())
                .find(|item| item.id == id)
                .and_then(shown)
        })
    }

    fn tab(&mut self, cx: &mut VisualTestContext, tab: Tab) {
        let id: &'static str = Box::leak(format!("settings-tab-{}", tab.name()).into_boxed_str());
        click(cx, id);
        self.settle(cx);
        assert_eq!(self.view.read_with(cx, |view, _| view.tab()), tab);
    }
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"))
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = bounds(cx, selector).center();
    cx.simulate_click(at, Modifiers::default());
}

#[gpui_kit::test]
fn the_window_opens_on_general_and_its_tabs_switch_pages(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    assert_eq!(h.view.read_with(cx, |view, _| view.tab()), Tab::General);
    bounds(cx, "settings-scroll-speed");
    assert!(cx.debug_bounds("settings-font-size").is_none(), "one page at a time");
    for tab in Tab::ALL {
        let id: &'static str = Box::leak(format!("settings-tab-{}", tab.name()).into_boxed_str());
        bounds(cx, id);
    }
    h.tab(cx, Tab::Appearance);
    bounds(cx, "settings-font-size");
    h.tab(cx, Tab::AgentStatus);
    for status in GLYPH_STATUSES {
        let well: &'static str = Box::leak(format!("settings-status-{}", status.wire_name()).into_boxed_str());
        bounds(cx, well);
        let shape: &'static str = Box::leak(format!("settings-status-shape-{}", status.wire_name()).into_boxed_str());
        bounds(cx, shape);
    }
    h.tab(cx, Tab::KeyMapping);
    bounds(cx, "settings-keymap-reload");
    bounds(cx, "settings-hooks-diagnostics");
    assert!(cx.debug_bounds("settings-keymap-default").is_none(), "no directory set");
}

#[gpui_kit::test]
fn a_toggle_click_writes_through_the_core_and_the_switch_reads_it_back(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    assert_eq!(h.item(cx, "settings-right-click-paste"), Some(Shown::On(true)));
    click(cx, "settings-right-click-paste");
    h.settle(cx);
    assert_eq!(h.settings().right_click_paste, Some(false));
    assert_eq!(h.stored().right_click_paste, Some(false), "written to settings.json");
    assert_eq!(h.item(cx, "settings-right-click-paste"), Some(Shown::On(false)));
    // a custom directory row shows once its mode is picked elsewhere, here through the core
    assert!(cx.debug_bounds("settings-new-session-custom-dir").is_none());
    h.state
        .core
        .change_settings(SettingsChange::General(GeneralChange::NewSessionDirectory(
            NewSessionDirectory::Custom,
        )));
    h.settle(cx);
    bounds(cx, "settings-new-session-custom-dir");
    bounds(cx, "settings-new-session-choose");
}

#[gpui_kit::test]
fn a_stepper_steps_through_the_core_and_greys_at_its_bound(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    h.tab(cx, Tab::Appearance);
    click(cx, "settings-font-size-increment");
    h.settle(cx);
    assert_eq!(h.settings().font_size, Some(14.0));
    assert_eq!(h.item(cx, "settings-font-size"), Some(Shown::Value(14.0)));
    click(cx, "settings-sidebar-font-size-decrement");
    h.settle(cx);
    assert_eq!(h.settings().sidebar_font_size, Some(12.0));
    h.state
        .core
        .change_settings(SettingsChange::Appearance(AppearanceChange::FontSize(32.0)));
    h.settle(cx);
    // a greyed button posts nothing (the core would clamp a 33 back to 32, so the stored value cannot show it)
    click(cx, "settings-font-size-increment");
    cx.run_until_parked();
    assert!(h.posted.0.lock().unwrap().is_empty(), "+ is greyed at 32");
    click(cx, "settings-font-size-decrement");
    cx.run_until_parked();
    assert!(!h.posted.0.lock().unwrap().is_empty(), "- still steps at 32");
    h.settle(cx);
    assert_eq!(h.settings().font_size, Some(31.0));
    // and at the lower bound the - is greyed
    h.state
        .core
        .change_settings(SettingsChange::Appearance(AppearanceChange::FontSize(8.0)));
    h.settle(cx);
    click(cx, "settings-font-size-decrement");
    cx.run_until_parked();
    assert!(h.posted.0.lock().unwrap().is_empty(), "- is greyed at 8");
}

#[gpui_kit::test]
fn a_picked_menu_row_writes_through_the_core(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    assert_eq!(h.item(cx, "settings-link-open-mode"), Some(Shown::Row(Some(0))));
    click(cx, "settings-link-open-mode");
    h.settle(cx);
    // the menu opens with no row under the keyboard: the first Down lands on Browser, the second on Session overlay
    cx.simulate_keystrokes("down down enter");
    h.settle(cx);
    assert_eq!(h.settings().effective_link_open_mode(), LinkOpenMode::Overlay);
    assert_eq!(h.item(cx, "settings-link-open-mode"), Some(Shown::Row(Some(1))));
    // a picked sound is stored and previews (the bell, which the test platform has none of)
    h.tab(cx, Tab::Notifications);
    click(cx, "settings-notification-sound");
    h.settle(cx);
    cx.simulate_keystrokes("down down enter");
    h.settle(cx);
    let first = agterm_core::model::STANDARD_SOUND_NAMES[0];
    assert_eq!(h.settings().notification_sound_name.as_deref(), Some(first));
    assert_eq!(h.item(cx, "settings-notification-sound"), Some(Shown::Row(Some(1))));
}

#[gpui_kit::test]
fn a_button_runs_its_edit(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(
        cx,
        AppSettings {
            blocked_status_shape: Some("diamond".to_owned()),
            ..AppSettings::default()
        },
    );
    h.tab(cx, Tab::AgentStatus);
    let blocked = StatusShape::ALL.iter().position(|shape| *shape == StatusShape::Diamond);
    assert_eq!(h.item(cx, "settings-status-shape-blocked"), Some(Shown::Row(blocked)));
    click(cx, "settings-status-reset");
    h.settle(cx);
    assert_eq!(h.settings().blocked_status_shape, None);
    assert_eq!(h.item(cx, "settings-status-shape-blocked"), Some(Shown::Row(Some(0))));
}

#[gpui_kit::test]
fn a_slider_moved_writes_and_a_change_elsewhere_moves_it_back(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    let slider = h
        .view
        .read_with(cx, |view, _| view.slider(SliderKey::ScrollSpeed).cloned())
        .unwrap();
    slider.update(cx, |_, cx| cx.emit(SliderEvent::Change(SliderValue::Single(7.0))));
    h.settle(cx);
    assert_eq!(h.settings().mouse_scroll_multiplier, Some(7.0));
    assert_eq!(h.item(cx, "settings-scroll-speed"), Some(Shown::Value(7.0)));
    // a change made elsewhere moves the slider
    h.state
        .core
        .change_settings(SettingsChange::General(GeneralChange::ScrollSpeed(2.0)));
    h.settle(cx);
    assert_eq!(slider.read_with(cx, |state, _| state.value()), SliderValue::Single(2.0));
}

/// An edit the core leaves as it was (a slider dragged past what it stores) snaps the slider back to the stored value.
#[gpui_kit::test]
fn an_edit_the_core_leaves_unchanged_snaps_the_slider_back(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    let slider = h
        .view
        .read_with(cx, |view, _| view.slider(SliderKey::InactivePaneMute).cloned())
        .unwrap();
    // the slider shows 9, but the edit it sends normalises to the stored default
    h.view.update_in(cx, |_, window, cx| {
        slider.update(cx, |state, cx| state.set_value(9.0, window, cx));
    });
    h.view
        .read_with(cx, |view, _| view.edits().clone())
        .send(SliderKey::InactivePaneMute.change(5.0));
    h.settle(cx);
    assert_eq!(h.settings().inactive_pane_mute_strength, None, "unchanged");
    assert_eq!(slider.read_with(cx, |state, _| state.value()), SliderValue::Single(5.0));
}

#[gpui_kit::test]
fn a_picked_color_writes_the_glyphs_hex_and_reset_restores_the_default(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    h.tab(cx, Tab::AgentStatus);
    let well = h
        .view
        .read_with(cx, |view, _| view.well(AgentStatus::Blocked).cloned())
        .unwrap();
    let picked = crate::status_glyph::hsla((0.0, 0.5, 1.0));
    well.update(cx, |_, cx| cx.emit(ColorPickerEvent::Change(Some(picked))));
    h.settle(cx);
    assert_eq!(h.settings().blocked_status_color_hex.as_deref(), Some("#0080ff"));
    click(cx, "settings-status-reset");
    h.settle(cx);
    assert_eq!(h.settings().blocked_status_color_hex, None);
    let default = crate::status_glyph::default_rgb(AgentStatus::Blocked).unwrap();
    let shown = well.read_with(cx, |state, _| state.value()).map(hex_of);
    assert_eq!(shown, Some(hex_of(crate::status_glyph::hsla(default))));
}

/// The color well of `status`, on the Agent Status tab.
fn well(h: &mut Harness, cx: &mut VisualTestContext, status: AgentStatus) -> Entity<ColorPickerState> {
    h.tab(cx, Tab::AgentStatus);
    h.view.read_with(cx, |view, _| view.well(status).cloned()).unwrap()
}

/// A well whose color the core stores as it was committed is left alone: a drag's grey keeps its hue (`#rrggbb` has
/// none) and a preview shown in it survives an unrelated change.
#[gpui_kit::test]
fn a_color_well_the_core_agrees_with_is_not_rewritten(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    let well = well(&mut h, cx, AgentStatus::Active);
    // a saturation dragged to 0 over a blue hue, committed as a slider drag commits it
    let grey = gpui_kit::hsla(0.6, 0.0, 0.5, 1.0);
    h.view.update_in(cx, |_, window, cx| {
        well.update(cx, |state, cx| state.update_color(grey, window, cx));
    });
    h.settle(cx);
    assert_eq!(
        h.settings().active_status_color_hex.as_deref(),
        Some(hex_of(grey).as_str())
    );
    assert_eq!(
        well.read_with(cx, |state, _| state.value()),
        Some(grey),
        "the hue survives"
    );
    // a preview (a color being picked) survives a change to another setting
    let preview = gpui_kit::hsla(0.3, 0.8, 0.4, 1.0);
    h.view.update_in(cx, |_, window, cx| {
        well.update(cx, |state, cx| state.preview_color(preview, window, cx));
    });
    h.state
        .core
        .change_settings(SettingsChange::General(GeneralChange::RightClickPaste(false)));
    h.settle(cx);
    assert_eq!(h.settings().right_click_paste, Some(false));
    assert_eq!(well.read_with(cx, |state, _| state.preview()), Some(preview));
}

/// A well showing another color than the core holds is rewritten: a change made elsewhere, an edit the core refused.
#[gpui_kit::test]
fn a_color_well_the_core_disagrees_with_is_rewritten(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    let well = well(&mut h, cx, AgentStatus::Completed);
    h.state
        .core
        .change_settings(agent_status::color_change(AgentStatus::Completed, "#123456".to_owned()));
    h.settle(cx);
    assert_eq!(
        well.read_with(cx, |state, _| state.value()).map(hex_of).as_deref(),
        Some("#123456")
    );
    // the well shows a color, but the edit that goes out is refused (an invalid hex clears the slot to the default)
    h.view.update_in(cx, |_, window, cx| {
        well.update(cx, |state, cx| {
            state.set_value(gpui_kit::hsla(0.0, 1.0, 0.5, 1.0), window, cx);
        });
    });
    h.view
        .read_with(cx, |view, _| view.edits().clone())
        .send(agent_status::color_change(
            AgentStatus::Completed,
            "nonsense".to_owned(),
        ));
    h.settle(cx);
    assert_eq!(h.settings().completed_status_color_hex, None);
    let default = crate::status_glyph::default_rgb(AgentStatus::Completed).unwrap();
    assert_eq!(
        well.read_with(cx, |state, _| state.value()).map(hex_of),
        Some(hex_of(crate::status_glyph::hsla(default)))
    );
}

#[gpui_kit::test]
fn a_change_made_elsewhere_shows_in_the_open_window(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    let response = h.state.core.dispatch(&agterm_proto::Request {
        cmd: agterm_proto::Command::Browser(agterm_proto::command::BrowserCmd::Links),
        target: None,
        args: Some(agterm_proto::Args {
            mode: Some("overlay".to_owned()),
            ..agterm_proto::Args::default()
        }),
    });
    assert!(response.ok, "{:?}", response.error);
    h.settle(cx);
    assert_eq!(h.item(cx, "settings-link-open-mode"), Some(Shown::Row(Some(1))));
    // the terminals flipping to the dark side retitles the second theme picker
    h.state
        .core
        .change_settings(SettingsChange::Appearance(AppearanceChange::FollowSystemAppearance {
            on: true,
            dark: false,
        }));
    h.state.config_applied(true);
    h.settle(cx);
    h.tab(cx, Tab::Appearance);
    let title = h.view.read_with(cx, |view, _| {
        view.sections()
            .iter()
            .flat_map(|section| section.items.iter())
            .find(|item| item.id == "settings-theme-dark")
            .map(|item| item.title.clone())
    });
    assert_eq!(title.as_deref(), Some("Light theme"));
}

#[test]
fn the_popup_takes_upstreams_window_size_and_shrinks_to_fit_a_small_window() {
    let fits = |width: f32, height: f32| {
        let panel = panel_size(size(px(width), px(height)));
        (f32::from(panel.width), f32::from(panel.height))
    };
    assert_eq!(fits(1280., 800.), PANEL_SIZE, "upstream's 540x680");
    assert_eq!(fits(800., 500.), (540., 468.), "the margin kept under and over it");
    assert_eq!(fits(400., 300.), (368., 268.), "and beside it");
    assert_eq!(fits(20., 10.), (0., 0.), "never negative");
}

#[gpui_kit::test]
fn escape_and_the_close_button_ask_to_close_the_popup(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    let asked = Rc::new(Cell::new(0));
    let counted = Rc::clone(&asked);
    let _closing = cx.update(|_, cx| {
        cx.subscribe(&h.view, move |_, _: &gpui_kit::DismissEvent, _| {
            counted.set(counted.get() + 1);
        })
    });
    let focus = h.view.read_with(cx, Focusable::focus_handle);
    cx.update(|window, cx| window.focus(&focus, cx));
    cx.simulate_keystrokes("escape");
    assert_eq!(asked.get(), 1, "Escape");
    click(cx, "settings-close");
    assert_eq!(asked.get(), 2, "the close button");
    // a picker's menu keeps its own Escape: it closes the menu, not the popup
    click(cx, "settings-link-open-mode");
    h.settle(cx);
    cx.simulate_keystrokes("escape");
    h.settle(cx);
    assert_eq!(asked.get(), 2, "the menu closed, the popup stays");
    cx.simulate_keystrokes("escape");
    assert_eq!(asked.get(), 3, "then Escape closes the popup");
}

#[gpui_kit::test]
fn a_restore_mode_pick_reads_back_the_configured_mode(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, AppSettings::default());
    let picker = h.view.read_with(cx, |view, _| {
        view.sections()
            .into_iter()
            .flat_map(|section| section.items)
            .find(|item| item.id == "settings-restore-mode")
            .map(|item| picker_of(&item).pick(1))
    });
    let change = picker.flatten().unwrap();
    h.view.read_with(cx, |view, _| view.edits().clone()).send(change);
    h.settle(cx);
    assert_eq!(h.settings().effective_restore_mode(), RestoreMode::Rerun);
    assert_eq!(h.item(cx, "settings-restore-mode"), Some(Shown::Row(Some(1))));
}
