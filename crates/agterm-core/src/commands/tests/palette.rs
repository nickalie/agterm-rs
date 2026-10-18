//! The command palettes ([`crate::palette`], upstream's `AppActionsPaletteTests`, `PaletteCatalogTests` and the
//! palette's `AppActions` paths): where a palette opens and what closes it, the rows each mode lists, what a row runs,
//! the live theme preview, and the Ctrl-Tab switcher's recency order.

use agterm_proto::command::{AskCmd, PickCmd, SurfaceCmd};
use agterm_proto::{AskButton, Command, PickItem};
use uuid::Uuid;

use super::keymap::Host;
use super::{Fixture, args, wire};
use crate::app_actions::{ActionEffect, ActionOrigin};
use crate::keymap::BuiltinAction;
use crate::model::{AgentIndicator, AgentStatus, SidebarMode};
use crate::palette::{PaletteAction, PaletteEntry, PaletteMode, PaletteShortcut, filter_entries};
use crate::palette_catalog::PaletteCommand;
use crate::persist::{AppSettings, LibraryPersistence, SettingsStore};
use crate::{Core, CoreConfig, CoreState};

fn ids(entries: &[PaletteEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.id.as_str()).collect()
}

fn titles(entries: &[PaletteEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.title.as_str()).collect()
}

fn entry<'a>(entries: &'a [PaletteEntry], title: &str) -> &'a PaletteEntry {
    entries
        .iter()
        .find(|entry| entry.title == title)
        .unwrap_or_else(|| panic!("no row {title}: {:?}", titles(entries)))
}

fn open_pick(fixture: &mut Fixture) {
    let response = fixture.send(
        Command::Pick(PickCmd::Open),
        None,
        args(|a| {
            a.items = Some(vec![PickItem {
                id: "a".to_owned(),
                label: "Alpha".to_owned(),
                subtitle: None,
            }]);
        }),
    );
    assert!(response.ok, "{response:?}");
}

fn blocked() -> AgentIndicator {
    AgentIndicator {
        status: AgentStatus::Blocked,
        ..AgentIndicator::default()
    }
}

// MARK: - open and close

#[test]
fn a_launcher_toggles_its_own_mode_and_switches_from_another() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    assert_eq!(fixture.core.palette_mode(), None);
    fixture.core.toggle_palette(window, PaletteMode::Sessions);
    assert_eq!(fixture.core.palette_mode(), Some(PaletteMode::Sessions));
    assert!(fixture.core.palette_holds(window));
    let first = fixture.core.palette_generation();
    fixture.core.toggle_palette(window, PaletteMode::Actions);
    assert_eq!(fixture.core.palette_mode(), Some(PaletteMode::Actions));
    assert!(fixture.core.palette_generation() > first, "a mode switch builds afresh");
    fixture.core.toggle_palette(window, PaletteMode::Actions);
    assert_eq!(fixture.core.palette_mode(), None, "the same launcher again closes it");
    fixture.core.open_palette(window, PaletteMode::Themes);
    fixture.core.open_palette(window, PaletteMode::Themes);
    assert_eq!(
        fixture.core.palette_mode(),
        Some(PaletteMode::Themes),
        "open never toggles"
    );
    fixture.core.close_palette();
    assert_eq!(fixture.core.palette_mode(), None);
}

#[test]
fn the_launcher_chords_open_the_palettes_and_the_theme_picker_only_opens() {
    let mut fixture = Fixture::new(0);
    let origin = ActionOrigin {
        window: fixture.window,
        quick: false,
    };
    for (action, mode) in [
        (BuiltinAction::SessionPalette, PaletteMode::Sessions),
        (BuiltinAction::CommandPalette, PaletteMode::Actions),
        (BuiltinAction::CustomCommandPalette, PaletteMode::CustomCommands),
        (BuiltinAction::ShowAttention, PaletteMode::Attention),
    ] {
        assert_eq!(fixture.core.perform_builtin(origin, action), ActionEffect::Done);
        assert_eq!(fixture.core.palette_mode(), Some(mode), "{action:?}");
        fixture.core.perform_builtin(origin, action);
        assert_eq!(fixture.core.palette_mode(), None, "{action:?} toggles");
    }
    fixture.core.perform_builtin(origin, BuiltinAction::SelectTheme);
    fixture.core.perform_builtin(origin, BuiltinAction::SelectTheme);
    assert_eq!(fixture.core.palette_mode(), Some(PaletteMode::Themes));
}

#[test]
fn a_palette_opens_only_in_the_frontmost_window_taking_ui_actions() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    let other = fixture.core.library_mut().new_window(None);
    fixture.core.report_frontmost(window);
    fixture.core.toggle_palette(other, PaletteMode::Actions);
    assert_eq!(fixture.core.palette_mode(), None, "not the frontmost window");

    open_pick(&mut fixture);
    assert!(fixture.core.modal_pending(window));
    assert!(!fixture.core.ui_actions_enabled(window));
    fixture.core.toggle_palette(window, PaletteMode::Actions);
    assert_eq!(fixture.core.palette_mode(), None, "the modal slot is held");
}

#[test]
fn a_pick_or_a_gui_ask_on_the_frontmost_window_closes_its_palette() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    fixture.core.toggle_palette(window, PaletteMode::Sessions);
    open_pick(&mut fixture);
    let _ = fixture.core.commit();
    assert_eq!(fixture.core.palette_mode(), None);

    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    fixture.core.toggle_palette(window, PaletteMode::Actions);
    let response = fixture.send(
        Command::Ask(AskCmd::Open),
        None,
        args(|a| {
            a.title = Some("Continue?".to_owned());
            a.style = Some("gui".to_owned());
            a.buttons = Some(vec![AskButton {
                id: "yes".to_owned(),
                label: "Yes".to_owned(),
                hotkey: None,
            }]);
        }),
    );
    assert!(response.ok, "{response:?}");
    let _ = fixture.core.commit();
    assert_eq!(fixture.core.palette_mode(), None, "a GUI ask holds the slot too");
}

#[test]
fn a_pick_in_a_background_window_closes_the_palette_once_that_window_comes_to_the_front() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    let other = fixture.core.library_mut().new_window(None);
    fixture.core.report_frontmost(other);
    open_pick(&mut fixture);
    let _ = fixture.core.commit();
    fixture.core.report_frontmost(window);
    fixture.core.toggle_palette(window, PaletteMode::Actions);
    let _ = fixture.core.commit();
    assert_eq!(
        fixture.core.palette_mode(),
        Some(PaletteMode::Actions),
        "the pick is elsewhere"
    );
    fixture.core.report_frontmost(other);
    let _ = fixture.core.commit();
    assert_eq!(fixture.core.palette_mode(), None);
}

#[test]
fn a_zoom_or_the_dashboard_closes_the_palette() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    fixture.core.toggle_palette(window, PaletteMode::Actions);
    let response = fixture.send(
        Command::Surface(SurfaceCmd::Zoom),
        None,
        args(|a| a.mode = Some("show".into())),
    );
    assert!(response.ok, "{response:?}");
    let _ = fixture.core.commit();
    assert_eq!(fixture.core.palette_mode(), None, "a zoom closes it");
    fixture.core.set_zoom_target(window, None);

    fixture.core.toggle_palette(window, PaletteMode::Actions);
    assert_eq!(fixture.core.palette_mode(), Some(PaletteMode::Actions));
    fixture.core.toggle_dashboard(window);
    if fixture.core.dashboard(window).is_some() {
        let _ = fixture.core.commit();
        assert_eq!(fixture.core.palette_mode(), None, "the dashboard closes it");
    }
}

#[test]
fn closing_the_palette_gives_the_session_the_keyboard_back() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    let session = fixture.selected().unwrap();
    fixture.core.toggle_palette(window, PaletteMode::Actions);
    fixture.core.close_palette();
    assert!(fixture.core.pane_focus_requested(session));
}

// MARK: - the feeds

#[test]
fn the_action_palette_lists_commands_windows_and_moves() {
    let mut fixture = Fixture::new(0);
    let fixture = &mut fixture;
    let (workspace, _) = fixture.add_workspace("ops", 0);
    let closed = fixture.core.library_mut().new_window(Some("archive"));
    fixture.core.close_window(closed);
    let entries = fixture.core.palette_entries(PaletteMode::Actions);
    let titles = titles(&entries);
    assert_eq!(titles[0], "New Session", "the static rows first, in catalog order");
    assert!(!titles.contains(&"Reopen Closed Item"), "an invisible row is left out");
    let new_window = entry(&entries, "New Window");
    assert_eq!(new_window.action, PaletteAction::Builtin(BuiltinAction::NewWindow));
    assert!(
        matches!(new_window.shortcut, Some(PaletteShortcut::Chord(_))),
        "its live chord"
    );
    assert!(titles.contains(&"Delete Window"), "two windows: one can go");
    assert_eq!(
        entry(&entries, "Open Window: archive").action,
        PaletteAction::OpenWindow(closed)
    );
    let session = fixture.selected().unwrap();
    assert_eq!(
        entry(&entries, "Move Session to ops").action,
        PaletteAction::MoveSession { session, workspace }
    );
    assert!(
        !titles
            .iter()
            .any(|title| title.starts_with("Move Session to") && *title != "Move Session to ops"),
        "the session's own workspace is skipped"
    );
}

#[test]
fn the_custom_commands_end_the_action_palette_and_fill_their_own() {
    let mut host = Host::new(0);
    host.write(
        "keymap.conf",
        "command \"Build\" ctrl+shift+b make\ncommand \"Deploy\" ./deploy\n",
    );
    host.fixture.core.reload_keymap();
    let fixture = &mut host.fixture;
    let entries = fixture.core.palette_entries(PaletteMode::Actions);
    let titles = titles(&entries);
    let build = entry(&entries, "Build");
    assert_eq!(build.badge.as_deref(), Some("custom"));
    assert_eq!(build.shortcut, Some(PaletteShortcut::Text("ctrl+shift+b".to_owned())));
    assert_eq!(entry(&entries, "Deploy").shortcut, None, "palette-only");
    assert_eq!(
        titles.last(),
        Some(&"Deploy"),
        "the custom commands come last, in file order"
    );

    let custom = fixture.core.palette_entries(PaletteMode::CustomCommands);
    assert_eq!(titles_of(&custom), ["Build", "Deploy"]);
    assert!(
        custom.iter().all(|entry| entry.badge.is_none()),
        "unbadged: all are custom"
    );
}

fn titles_of(entries: &[PaletteEntry]) -> Vec<String> {
    entries.iter().map(|entry| entry.title.clone()).collect()
}

#[test]
fn the_session_palette_lists_the_navigable_sessions_with_their_workspace() {
    let mut fixture = Fixture::new(1);
    let (_, ops) = fixture.add_workspace("ops", 1);
    let entries = fixture.core.palette_entries(PaletteMode::Sessions);
    assert_eq!(entries.len(), 3);
    let row = entries.iter().find(|entry| entry.id == wire(ops[0])).unwrap();
    assert_eq!(row.subtitle.as_deref(), Some("ops · /tmp"));
    assert_eq!(row.action, PaletteAction::SelectSession(ops[0]));

    let flagged = fixture.sessions[0];
    let store = fixture.store_mut().unwrap();
    store.set_flag(true, flagged);
    store.set_sidebar_mode(SidebarMode::Flagged);
    let entries = fixture.core.palette_entries(PaletteMode::Sessions);
    assert_eq!(
        ids(&entries),
        [wire(flagged).as_str()],
        "the flagged set in flagged mode"
    );
}

#[test]
fn the_attention_palette_spans_the_open_windows_in_attention_order() {
    let mut fixture = Fixture::new(2);
    let [quiet, waiting, busy] = [fixture.sessions[0], fixture.sessions[1], fixture.sessions[2]];
    let store = fixture.store_mut().unwrap();
    store.set_agent_indicator(
        AgentIndicator {
            status: AgentStatus::Active,
            ..AgentIndicator::default()
        },
        busy,
    );
    store.set_agent_indicator(blocked(), waiting);
    let entries = fixture.core.palette_entries(PaletteMode::Attention);
    assert_eq!(ids(&entries), [wire(waiting), wire(busy)], "blocked before active");
    assert!(!ids(&entries).contains(&wire(quiet).as_str()), "idle is left out");
    assert_eq!(
        entries[0].indicator.as_ref().map(|i| i.status),
        Some(AgentStatus::Blocked)
    );
    let workspace = fixture.store().workspaces()[0].name.clone();
    assert_eq!(
        entries[0].subtitle.as_deref(),
        Some(format!("{workspace} · /tmp").as_str())
    );
    assert_eq!(
        filter_entries(&entries, "", PaletteMode::Attention),
        [0, 1],
        "a blank query keeps the attention order"
    );

    let other = fixture.core.library_mut().new_window(Some("second"));
    let store = fixture.core.library_mut().store_mut(other).unwrap();
    let theirs = store.selected_session_id().unwrap();
    store.set_agent_indicator(blocked(), theirs);
    let entries = fixture.core.palette_entries(PaletteMode::Attention);
    assert_eq!(entries.len(), 3);
    let row = entries.iter().find(|entry| entry.id == wire(theirs)).unwrap();
    assert!(
        row.subtitle
            .as_deref()
            .is_some_and(|subtitle| subtitle.starts_with("second · ")),
        "the window is named once several are open: {:?}",
        row.subtitle
    );
    assert_eq!(
        row.action,
        PaletteAction::Attention {
            window: other,
            session: theirs
        }
    );
}

// MARK: - running a row

#[test]
fn an_inert_row_neither_runs_nor_closes_the_palette() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    fixture.core.toggle_palette(window, PaletteMode::Actions);
    let swap = PaletteAction::Command(PaletteCommand::SwapPanes);
    assert!(!fixture.core.palette_entry_enabled(window, &swap), "no split to swap");
    assert_eq!(fixture.core.run_palette_entry(window, &swap), None);
    assert_eq!(fixture.core.palette_mode(), Some(PaletteMode::Actions));
}

#[test]
fn a_row_closes_the_palette_and_runs_as_its_menu_item() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    let before = fixture.order(fixture.workspace).len();
    fixture.core.toggle_palette(window, PaletteMode::Actions);
    let effect = fixture
        .core
        .run_palette_entry(window, &PaletteAction::Command(PaletteCommand::NewSession));
    assert_eq!(effect, Some(ActionEffect::Done));
    assert_eq!(fixture.core.palette_mode(), None);
    assert_eq!(fixture.order(fixture.workspace).len(), before + 1);

    fixture.core.toggle_palette(window, PaletteMode::Actions);
    let session = fixture.selected().unwrap();
    let effect = fixture
        .core
        .run_palette_entry(window, &PaletteAction::Command(PaletteCommand::RenameSession));
    assert_eq!(
        effect,
        Some(ActionEffect::Frontend(
            crate::app_actions::FrontendAction::RenameSession { window, session }
        )),
        "a dialog is the frontend's, after the palette closed"
    );
    assert_eq!(fixture.core.palette_mode(), None);
}

#[test]
fn the_launcher_rows_open_the_next_palette() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    for (command, mode) in [
        (PaletteCommand::ShowAttention, PaletteMode::Attention),
        (PaletteCommand::SelectTheme, PaletteMode::Themes),
    ] {
        fixture.core.toggle_palette(window, PaletteMode::Actions);
        fixture.core.run_palette_entry(window, &PaletteAction::Command(command));
        assert_eq!(fixture.core.palette_mode(), Some(mode), "{command:?}");
        fixture.core.close_palette();
    }
}

#[test]
fn close_session_from_the_palette_never_closes_the_window() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    let session = fixture.selected().unwrap();
    fixture.store_mut().unwrap().select_session(None);
    fixture.core.toggle_palette(window, PaletteMode::Actions);
    let effect = fixture
        .core
        .run_palette_entry(window, &PaletteAction::Command(PaletteCommand::CloseSession));
    assert_eq!(effect, Some(ActionEffect::Done));
    assert!(fixture.store().session(session).is_some());
    assert!(fixture.core.library().is_open(window));
}

#[test]
fn picking_a_session_selects_it_and_moving_one_moves_it() {
    let mut fixture = Fixture::new(1);
    let window = fixture.window;
    let first = fixture.sessions[0];
    fixture.core.toggle_palette(window, PaletteMode::Sessions);
    fixture
        .core
        .run_palette_entry(window, &PaletteAction::SelectSession(first));
    assert_eq!(fixture.selected(), Some(first));
    assert!(fixture.core.pane_focus_requested(first));

    let (ops, _) = fixture.add_workspace("ops", 0);
    fixture.core.toggle_palette(window, PaletteMode::Actions);
    fixture.core.run_palette_entry(
        window,
        &PaletteAction::MoveSession {
            session: first,
            workspace: ops,
        },
    );
    assert_eq!(fixture.order(ops), [first]);
}

#[test]
fn an_attention_row_raises_its_window_and_selects_the_session() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    let other = fixture.core.library_mut().new_window(None);
    let store = fixture.core.library_mut().store_mut(other).unwrap();
    let workspace = store.workspaces()[0].id();
    let target = store
        .add_session(workspace, crate::model::NewSession::at_cwd("/srv"))
        .unwrap();
    let first = store.sessions().next().unwrap().id();
    store.select_session(Some(first));
    store.set_agent_indicator(blocked(), target);
    fixture.core.report_frontmost(window);
    let _ = fixture.core.take_window_requests();
    fixture.core.toggle_palette(window, PaletteMode::Attention);
    let action = PaletteAction::Attention {
        window: other,
        session: target,
    };
    assert!(fixture.core.palette_entry_enabled(window, &action));
    fixture.core.run_palette_entry(window, &action);
    assert_eq!(fixture.core.library().active_window_id(), Some(other));
    assert_eq!(
        fixture.core.take_window_requests(),
        [crate::WindowRequest::Raise(other)]
    );
    let store = fixture.core.library().store(other).unwrap();
    assert_eq!(store.selected_session_id(), Some(target));
}

#[test]
#[cfg_attr(
    windows,
    ignore = "a custom command runs in its session's directory, which the fixture puts at /tmp, a path Windows has not"
)]
fn a_custom_command_row_runs_for_the_active_session() {
    // the added session sits in /tmp, a directory that exists
    let mut host = Host::new(1);
    host.write("keymap.conf", "command \"Deploy\" ./deploy {AGT_SESSION_NAME}\n");
    host.fixture.core.reload_keymap();
    let window = host.fixture.window;
    host.fixture.core.toggle_palette(window, PaletteMode::CustomCommands);
    let entries = host.fixture.core.palette_entries(PaletteMode::CustomCommands);
    host.fixture.core.run_palette_entry(window, &entries[0].action);
    let launched = host.launcher.launches();
    assert_eq!(launched.len(), 1, "{launched:?}");
    assert_eq!(host.fixture.core.palette_mode(), None);
}

// MARK: - the theme picker

const THEMES: [&str; 3] = ["Dracula", "Nord", "Builtin Light"];

/// A core over a persisted bootstrap library whose themes come from a temp directory.
fn themed(settings: AppSettings) -> (Core, Uuid, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut persistence = LibraryPersistence::new(dir.path());
    let library = persistence.bootstrap("/home/u");
    let window = library.active_window_id().unwrap();
    let themes = dir.path().join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    for name in THEMES {
        std::fs::write(themes.join(name), "").unwrap();
    }
    let mut core = Core::new(
        CoreConfig {
            app: agterm_proto::AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings,
            persistence: Some(persistence),
        },
    );
    core.set_theme_paths(Some(themes), Some(dir.path().join("ghostty-settings.conf")));
    (core, window, dir)
}

fn saved(dir: &tempfile::TempDir) -> AppSettings {
    SettingsStore::new(dir.path()).load()
}

#[test]
fn the_theme_palette_lists_the_themes_and_badges_the_current_one() {
    let (mut core, window, _dir) = themed(AppSettings {
        theme: Some("Nord".to_owned()),
        ..AppSettings::default()
    });
    core.open_palette(window, PaletteMode::Themes);
    let entries = core.palette_entries(PaletteMode::Themes);
    assert_eq!(
        titles(&entries),
        ["default ghostty", "Builtin Light", "Dracula", "Nord"]
    );
    assert_eq!(entry(&entries, "Nord").badge.as_deref(), Some("current"));
    assert_eq!(core.palette_current_theme_id(), "theme:Nord");

    core.preview_palette_theme(Some("Dracula"));
    let entries = core.palette_entries(PaletteMode::Themes);
    assert_eq!(
        entry(&entries, "Nord").badge.as_deref(),
        Some("current"),
        "the badge stays on the theme the picker found"
    );

    let (mut core, window, _dir) = themed(AppSettings {
        theme: Some("Builtin Light".to_owned()),
        dark_theme: Some("Nord".to_owned()),
        follow_system_appearance: Some(true),
        ..AppSettings::default()
    });
    core.open_palette(window, PaletteMode::Themes);
    let entries = core.palette_entries(PaletteMode::Themes);
    assert!(
        !titles(&entries).contains(&"default ghostty"),
        "no built-in row while following"
    );
}

#[test]
fn a_preview_applies_unsaved_and_a_run_commits_it() {
    let (mut core, window, dir) = themed(AppSettings {
        theme: Some("Nord".to_owned()),
        ..AppSettings::default()
    });
    core.open_palette(window, PaletteMode::Themes);
    let _ = core.take_config_reload();
    core.preview_palette_theme(Some("Dracula"));
    assert_eq!(core.settings().theme.as_deref(), Some("Dracula"), "applied live");
    assert!(core.take_config_reload(), "the terminals reload it");
    // a settings write mid-preview keeps the committed theme on disk
    core.update_settings(|settings| settings.sidebar_font_size = Some(14.0));
    assert_eq!(saved(&dir).theme.as_deref(), Some("Nord"));
    assert_eq!(saved(&dir).sidebar_font_size, Some(14.0));

    core.run_palette_entry(window, &PaletteAction::Theme(Some("Dracula".to_owned())));
    assert_eq!(core.palette_mode(), None);
    assert_eq!(core.settings().theme.as_deref(), Some("Dracula"));
    assert_eq!(saved(&dir).theme.as_deref(), Some("Dracula"), "committed");
}

#[test]
fn leaving_the_theme_palette_reverts_the_preview() {
    let (mut core, window, dir) = themed(AppSettings {
        theme: Some("Nord".to_owned()),
        ..AppSettings::default()
    });
    core.open_palette(window, PaletteMode::Themes);
    core.preview_palette_theme(Some("Dracula"));
    core.close_palette();
    assert_eq!(core.settings().theme.as_deref(), Some("Nord"));
    assert_ne!(saved(&dir).theme.as_deref(), Some("Dracula"), "never saved");

    core.open_palette(window, PaletteMode::Themes);
    core.preview_palette_theme(None);
    assert_eq!(core.settings().theme, None);
    core.toggle_palette(window, PaletteMode::Actions);
    assert_eq!(
        core.settings().theme.as_deref(),
        Some("Nord"),
        "a mode switch reverts too"
    );
    core.preview_palette_theme(Some("Dracula"));
    assert_eq!(
        core.settings().theme.as_deref(),
        Some("Nord"),
        "no preview outside the theme palette"
    );
}

#[test]
fn a_preview_while_following_in_the_dark_writes_the_dark_slot_only() {
    let (mut core, window, dir) = themed(AppSettings {
        theme: Some("Builtin Light".to_owned()),
        dark_theme: Some("Nord".to_owned()),
        follow_system_appearance: Some(true),
        ..AppSettings::default()
    });
    core.report_applied_appearance(true);
    core.open_palette(window, PaletteMode::Themes);
    assert_eq!(core.palette_current_theme_id(), "theme:Nord");
    core.preview_palette_theme(Some("Dracula"));
    assert_eq!(core.settings().dark_theme.as_deref(), Some("Dracula"));
    assert_eq!(core.settings().theme.as_deref(), Some("Builtin Light"));
    core.run_palette_entry(window, &PaletteAction::Theme(Some("Dracula".to_owned())));
    let saved = saved(&dir);
    assert_eq!(saved.dark_theme.as_deref(), Some("Dracula"));
    assert_eq!(saved.theme.as_deref(), Some("Builtin Light"));
}

#[test]
fn a_restore_mode_write_mid_preview_saves_the_committed_theme() {
    let (mut core, window, dir) = themed(AppSettings {
        theme: Some("Nord".to_owned()),
        ..AppSettings::default()
    });
    core.open_palette(window, PaletteMode::Themes);
    core.preview_palette_theme(Some("Dracula"));
    assert!(core.save_restore_mode(crate::persist::RestoreMode::Rerun));
    let saved = saved(&dir);
    assert_eq!(saved.restore_mode, Some(crate::persist::RestoreMode::Rerun));
    assert_eq!(
        saved.theme.as_deref(),
        Some("Nord"),
        "the preview never reaches the disk"
    );
}

#[test]
fn an_explicit_theme_set_mid_preview_sticks_past_the_palettes_escape() {
    let (mut core, window, dir) = themed(AppSettings {
        theme: Some("Nord".to_owned()),
        ..AppSettings::default()
    });
    core.open_palette(window, PaletteMode::Themes);
    core.preview_palette_theme(Some("Dracula"));
    let response = super::send(
        &mut core,
        Command::Theme(agterm_proto::command::ThemeCmd::Set),
        None,
        args(|a| a.name = Some("Builtin Light".to_owned())),
    );
    assert!(response.ok, "{response:?}");
    assert_eq!(saved(&dir).theme.as_deref(), Some("Builtin Light"), "saved as asked");
    core.preview_palette_theme(Some("Dracula"));
    core.close_palette();
    assert_eq!(
        core.settings().theme.as_deref(),
        Some("Builtin Light"),
        "the Escape reverts to the explicit theme, not the one the palette found"
    );
    assert_eq!(saved(&dir).theme.as_deref(), Some("Builtin Light"));
}

fn theme_pick(theme: &str, dark: bool) -> crate::settings_model::SettingsChange {
    crate::settings_model::SettingsChange::Appearance(
        crate::settings_model::AppearanceChange::ThemeForCurrentAppearance {
            theme: Some(theme.to_owned()),
            dark,
        },
    )
}

#[test]
fn a_settings_theme_pick_mid_preview_is_saved_and_kept_past_the_palettes_escape() {
    let (mut core, window, dir) = themed(AppSettings {
        theme: Some("Nord".to_owned()),
        ..AppSettings::default()
    });
    core.open_palette(window, PaletteMode::Themes);
    core.preview_palette_theme(Some("Dracula"));
    let _ = core.take_config_reload();
    assert!(core.change_settings(theme_pick("Builtin Light", false)));
    assert_eq!(core.settings().theme.as_deref(), Some("Builtin Light"), "live");
    assert_eq!(saved(&dir).theme.as_deref(), Some("Builtin Light"), "saved as asked");
    assert!(core.take_config_reload(), "the terminals take it");
    assert_eq!(core.palette_mode(), Some(PaletteMode::Themes), "the palette stays open");
    // the palette keeps browsing from the new theme, and its Escape goes back to it
    core.preview_palette_theme(Some("Dracula"));
    assert_eq!(core.settings().theme.as_deref(), Some("Dracula"));
    assert_eq!(
        saved(&dir).theme.as_deref(),
        Some("Builtin Light"),
        "the preview stays unsaved"
    );
    core.close_palette();
    assert_eq!(core.settings().theme.as_deref(), Some("Builtin Light"));
    assert_eq!(saved(&dir).theme.as_deref(), Some("Builtin Light"));
}

#[test]
fn a_settings_pick_of_the_theme_the_palette_found_ends_the_preview_on_screen() {
    let (mut core, window, dir) = themed(AppSettings {
        theme: Some("Nord".to_owned()),
        ..AppSettings::default()
    });
    core.open_palette(window, PaletteMode::Themes);
    core.preview_palette_theme(Some("Dracula"));
    let _ = core.take_config_reload();
    assert!(
        core.change_settings(theme_pick("Nord", false)),
        "the theme on screen changed"
    );
    assert_eq!(core.settings().theme.as_deref(), Some("Nord"));
    assert!(core.take_config_reload(), "the previewed theme leaves the terminals");
    core.close_palette();
    assert_eq!(core.settings().theme.as_deref(), Some("Nord"));
    assert_ne!(
        saved(&dir).theme.as_deref(),
        Some("Dracula"),
        "the preview never reaches the disk"
    );
}

#[test]
fn the_follow_toggle_and_the_alternate_picker_mid_preview_land_over_the_found_slots() {
    use crate::settings_model::{AppearanceChange, SettingsChange};

    let (mut core, window, dir) = themed(AppSettings {
        theme: Some("Nord".to_owned()),
        ..AppSettings::default()
    });
    core.open_palette(window, PaletteMode::Themes);
    core.preview_palette_theme(Some("Dracula"));
    let follow = SettingsChange::Appearance(AppearanceChange::FollowSystemAppearance { on: true, dark: false });
    assert!(core.change_settings(follow));
    let settings = saved(&dir);
    assert_eq!(
        (settings.theme.as_deref(), settings.dark_theme.as_deref()),
        (Some("Nord"), Some("Nord")),
        "seeded from the found theme, never the preview"
    );
    core.preview_palette_theme(Some("Dracula"));
    let alternate = SettingsChange::Appearance(AppearanceChange::AlternateTheme {
        theme: Some("Builtin Light".to_owned()),
        dark: false,
    });
    assert!(core.change_settings(alternate));
    core.close_palette();
    let live = core.settings();
    assert_eq!(
        (live.theme.as_deref(), live.dark_theme.as_deref()),
        (Some("Nord"), Some("Builtin Light"))
    );
    let settings = saved(&dir);
    assert_eq!(
        (settings.theme.as_deref(), settings.dark_theme.as_deref()),
        (Some("Nord"), Some("Builtin Light"))
    );
}

#[test]
fn another_settings_edit_mid_preview_keeps_the_preview_and_saves_the_found_theme() {
    use crate::settings_model::{AppearanceChange, SettingsChange};

    let (mut core, window, dir) = themed(AppSettings {
        theme: Some("Nord".to_owned()),
        ..AppSettings::default()
    });
    core.open_palette(window, PaletteMode::Themes);
    core.preview_palette_theme(Some("Dracula"));
    assert!(core.change_settings(SettingsChange::Appearance(AppearanceChange::SidebarFontSize(15.0))));
    assert_eq!(
        core.settings().theme.as_deref(),
        Some("Dracula"),
        "the preview stays on screen"
    );
    assert_eq!(saved(&dir).theme.as_deref(), Some("Nord"));
    assert_eq!(saved(&dir).sidebar_font_size, Some(15.0));
    core.close_palette();
    assert_eq!(core.settings().theme.as_deref(), Some("Nord"));
}

#[test]
fn a_settings_theme_pick_without_a_preview_is_an_ordinary_edit() {
    let (mut core, _window, dir) = themed(AppSettings {
        theme: Some("Nord".to_owned()),
        ..AppSettings::default()
    });
    assert!(core.change_settings(theme_pick("Dracula", false)));
    assert!(!core.change_settings(theme_pick("Dracula", false)), "unchanged");
    assert_eq!(saved(&dir).theme.as_deref(), Some("Dracula"));
}

// MARK: - the Ctrl-Tab switcher

#[test]
fn the_switcher_walks_the_navigable_sessions_by_recency_capped() {
    let mut fixture = Fixture::new(3);
    let window = fixture.window;
    let sessions = fixture.sessions.clone();
    for id in [sessions[1], sessions[3], sessions[0], sessions[2]] {
        fixture.store_mut().unwrap().select_session(Some(id));
    }
    assert_eq!(
        fixture.core.switcher_candidates(window, 10),
        [sessions[2], sessions[0], sessions[3], sessions[1]],
        "most recent first"
    );
    assert_eq!(fixture.core.switcher_candidates(window, 2), [sessions[2], sessions[0]]);

    let store = fixture.store_mut().unwrap();
    store.set_flags(true, &[sessions[3], sessions[1]]);
    store.set_sidebar_mode(SidebarMode::Flagged);
    assert_eq!(
        fixture.core.switcher_candidates(window, 10),
        [sessions[3], sessions[1]],
        "the flagged set in flagged mode"
    );

    fixture.core.switch_to_session(window, sessions[1]);
    assert_eq!(fixture.selected(), Some(sessions[1]));
    open_pick(&mut fixture);
    fixture.core.switch_to_session(window, sessions[3]);
    assert_eq!(fixture.selected(), Some(sessions[1]), "behind a modal it stands down");
}
