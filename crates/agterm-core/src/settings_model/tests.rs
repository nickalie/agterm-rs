//! The Settings model: each control's validation and default-to-nil mapping, the theme slot rules, the chrome
//! element lists, the effects a change has on the running app, and the round trip through `settings.json`.

use agterm_proto::AppIdentity;

use super::*;
use crate::model::WindowLibrary;
use crate::paths::{ConfigLocator, MapEnv};
use crate::persist::{LibraryPersistence, SettingsStore};
use crate::{CoreConfig, CoreState};

fn applied(changes: impl IntoIterator<Item = SettingsChange>) -> AppSettings {
    let mut settings = AppSettings::default();
    for change in changes {
        change.apply_to(&mut settings);
    }
    settings
}

fn general(change: GeneralChange) -> SettingsChange {
    SettingsChange::General(change)
}

fn appearance(change: AppearanceChange) -> SettingsChange {
    SettingsChange::Appearance(change)
}

fn status(change: AgentStatusChange) -> SettingsChange {
    SettingsChange::AgentStatus(change)
}

/// A core whose settings live in `dir` and whose config directory is `<dir>/config`.
fn persisted_core(dir: &std::path::Path) -> Core {
    let mut library = WindowLibrary::seeded("/home/u");
    library.new_window(None);
    let mut core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings: SettingsStore::new(dir).load(),
            persistence: Some(LibraryPersistence::new(dir)),
        },
    );
    core.set_config_locator(ConfigLocator::capture(
        &MapEnv::new().with("AGTERM_STATE_DIR", dir.as_os_str()),
    ));
    core
}

/// `settings` as a write stores it: the restore mode migrated.
fn migrated(settings: &AppSettings) -> AppSettings {
    let mut settings = settings.clone();
    settings.migrate_restore_mode();
    settings
}

#[test]
fn a_controls_default_value_is_stored_as_nil() {
    let defaults = applied([
        general(GeneralChange::ScrollSpeed(3.0)),
        general(GeneralChange::RightClickPaste(true)),
        general(GeneralChange::LinkOpenMode(LinkOpenMode::Browser)),
        general(GeneralChange::WorkspaceRowClickExpands(true)),
        general(GeneralChange::NewSessionDirectory(NewSessionDirectory::Home)),
        general(GeneralChange::NewSessionPlacement(NewSessionPlacementSetting::End)),
        general(GeneralChange::ConfirmCloseSession(false)),
        general(GeneralChange::CloseGraceUndo(true)),
        general(GeneralChange::FlaggedViewLayout(FlaggedViewLayout::Flat)),
        general(GeneralChange::InheritGlobalGhosttyConfig(false)),
        appearance(AppearanceChange::ToolbarMode(ToolbarMode::Compact)),
        appearance(AppearanceChange::BackgroundOpacity(1.0)),
        appearance(AppearanceChange::BackgroundBlur(0)),
        appearance(AppearanceChange::SidebarBackgroundShift(5)),
        appearance(AppearanceChange::SidebarFontSize(13.0)),
        appearance(AppearanceChange::InterfaceFontSize(13.0)),
        appearance(AppearanceChange::InactivePaneMute(5)),
        SettingsChange::Notifications(NotificationsChange::Banners(true)),
        SettingsChange::Notifications(NotificationsChange::Badges(true)),
        SettingsChange::Notifications(NotificationsChange::DockBounce(DockBounce::Off)),
        SettingsChange::Notifications(NotificationsChange::Sound(None)),
        SettingsChange::Notifications(NotificationsChange::AttentionButton(false)),
        status(AgentStatusChange::Shape {
            status: AgentStatus::Blocked,
            shape: StatusShape::Circle,
        }),
        status(AgentStatusChange::StatusReset(StatusReset::FirstKey)),
        status(AgentStatusChange::AutoFollow(AutoFollowAttention::Off)),
        status(AgentStatusChange::AutoFollowAwayFromRunning(true)),
    ]);
    assert_eq!(defaults, AppSettings::default());
}

#[test]
fn a_non_default_value_is_stored_raw() {
    let settings = applied([
        general(GeneralChange::RightClickPaste(false)),
        general(GeneralChange::LinkOpenMode(LinkOpenMode::Overlay)),
        general(GeneralChange::NewSessionDirectory(NewSessionDirectory::CurrentSession)),
        general(GeneralChange::NewSessionPlacement(
            NewSessionPlacementSetting::AfterCurrent,
        )),
        general(GeneralChange::ConfirmCloseSession(true)),
        general(GeneralChange::CloseGraceUndo(false)),
        general(GeneralChange::FlaggedViewLayout(FlaggedViewLayout::Tree)),
        appearance(AppearanceChange::ToolbarMode(ToolbarMode::Hidden)),
        SettingsChange::Notifications(NotificationsChange::DockBounce(DockBounce::UntilFocused)),
        status(AgentStatusChange::AutoFollow(AutoFollowAttention::S30)),
        status(AgentStatusChange::AutoFollowAwayFromRunning(false)),
    ]);
    assert_eq!(settings.right_click_paste, Some(false));
    assert_eq!(settings.link_open_mode.as_deref(), Some("overlay"));
    assert_eq!(settings.new_session_directory.as_deref(), Some("currentSession"));
    assert_eq!(settings.new_session_placement.as_deref(), Some("afterCurrent"));
    assert_eq!(settings.confirm_close_session, Some(true));
    assert_eq!(settings.close_grace_undo_enabled, Some(false));
    assert_eq!(settings.flagged_view_layout.as_deref(), Some("tree"));
    assert_eq!(settings.toolbar_mode.as_deref(), Some("hidden"));
    assert_eq!(settings.dock_bounce.as_deref(), Some("untilFocused"));
    assert_eq!(settings.auto_follow_attention.as_deref(), Some("s30"));
    assert_eq!(settings.auto_follow_stay_on_active, Some(true));
}

#[test]
fn slider_and_stepper_values_are_rounded_into_their_range() {
    let settings = applied([
        general(GeneralChange::ScrollSpeed(5.4)),
        appearance(AppearanceChange::FontSize(99.0)),
        appearance(AppearanceChange::SidebarFontSize(2.0)),
        appearance(AppearanceChange::InterfaceFontSize(15.6)),
        appearance(AppearanceChange::SidebarBackgroundShift(-3)),
        appearance(AppearanceChange::InactivePaneMute(42)),
        appearance(AppearanceChange::BackgroundOpacity(-0.5)),
        appearance(AppearanceChange::BackgroundBlur(250)),
    ]);
    assert_eq!(settings.mouse_scroll_multiplier, Some(5.0));
    assert_eq!(settings.font_size, Some(32.0));
    assert_eq!(settings.sidebar_font_size, Some(9.0));
    assert_eq!(settings.interface_font_size, Some(16.0));
    assert_eq!(settings.sidebar_background_shift, Some(0));
    assert_eq!(settings.inactive_pane_mute_strength, Some(10));
    assert_eq!(settings.background_opacity, Some(0.0));
    assert_eq!(settings.background_blur, Some(100));
}

#[test]
fn a_value_that_is_not_a_number_changes_nothing() {
    let mut settings = applied([
        general(GeneralChange::ScrollSpeed(7.0)),
        appearance(AppearanceChange::FontSize(14.0)),
    ]);
    let before = settings.clone();
    for change in [
        general(GeneralChange::ScrollSpeed(f64::NAN)),
        appearance(AppearanceChange::FontSize(f64::INFINITY)),
        appearance(AppearanceChange::BackgroundOpacity(f64::NAN)),
    ] {
        change.apply_to(&mut settings);
    }
    assert_eq!(settings, before);
}

/// The font size stepper stores its value even at 13, as upstream's `setFontSize` does: an explicit size beats one
/// a `ghostty.conf` sets.
#[test]
fn the_font_size_and_family_are_stored_explicitly() {
    let settings = applied([
        appearance(AppearanceChange::FontSize(13.0)),
        appearance(AppearanceChange::FontFamily(Some(" JetBrains Mono ".into()))),
    ]);
    assert_eq!(settings.font_size, Some(13.0));
    assert_eq!(settings.font_family.as_deref(), Some("JetBrains Mono"));
    let cleared = applied([appearance(AppearanceChange::FontFamily(Some(String::new())))]);
    assert_eq!(cleared.font_family, None);
}

#[test]
fn cursor_pickers_keep_default_as_a_third_state() {
    let settings = applied([
        appearance(AppearanceChange::CursorStyle(Some(CursorStyle::Bar))),
        appearance(AppearanceChange::CursorBlink(Some(false))),
    ]);
    assert_eq!(settings.cursor_style.as_deref(), Some("bar"));
    assert_eq!(settings.cursor_blink, Some(false));
    let defaults = applied([
        appearance(AppearanceChange::CursorStyle(None)),
        appearance(AppearanceChange::CursorBlink(None)),
    ]);
    assert_eq!(defaults, AppSettings::default());
}

#[test]
fn writing_a_toolbar_mode_clears_the_legacy_toggle() {
    let mut settings = AppSettings {
        compact_toolbar: Some(false),
        ..AppSettings::default()
    };
    appearance(AppearanceChange::ToolbarMode(ToolbarMode::Normal)).apply_to(&mut settings);
    assert_eq!(settings.toolbar_mode.as_deref(), Some("normal"));
    assert_eq!(settings.compact_toolbar, None);
}

#[test]
fn the_theme_picker_writes_the_slot_on_screen() {
    let follow = |theme: &str, dark: &str| AppSettings {
        theme: Some(theme.into()),
        dark_theme: Some(dark.into()),
        follow_system_appearance: Some(true),
        ..AppSettings::default()
    };
    let mut settings = follow("Light", "Dark");
    settings.set_theme_for_current_appearance(Some("Nord".into()), true);
    assert_eq!(settings, follow("Light", "Nord"));
    settings.set_theme_for_current_appearance(Some("Day".into()), false);
    assert_eq!(settings, follow("Day", "Nord"));
    // not following, the dark appearance writes the single slot
    let mut single = AppSettings::default();
    single.set_theme_for_current_appearance(Some("Nord".into()), true);
    assert_eq!(single.theme.as_deref(), Some("Nord"));
    assert_eq!(single.dark_theme, None);
}

#[test]
fn the_alternate_picker_writes_the_other_slot_and_clearing_dark_stops_following() {
    let mut settings = AppSettings {
        theme: Some("Light".into()),
        dark_theme: Some("Dark".into()),
        follow_system_appearance: Some(true),
        ..AppSettings::default()
    };
    assert_eq!(settings.alternate_theme(true), Some("Light"));
    assert_eq!(settings.alternate_theme(false), Some("Dark"));
    settings.set_alternate_theme(Some("Paper".into()), true);
    assert_eq!(settings.theme.as_deref(), Some("Paper"));
    settings.set_alternate_theme(Some("Night".into()), false);
    assert_eq!(settings.dark_theme.as_deref(), Some("Night"));
    settings.set_alternate_theme(None, false);
    assert_eq!(settings.dark_theme, None);
    assert_eq!(settings.follow_system_appearance, None);
    assert_eq!(settings.theme.as_deref(), Some("Paper"));
}

#[test]
fn following_seeds_the_pair_and_unfollowing_keeps_the_theme_on_screen() {
    let mut settings = AppSettings {
        theme: Some("Nord".into()),
        ..AppSettings::default()
    };
    settings.set_follow_system_appearance(true, false);
    assert_eq!(settings.theme.as_deref(), Some("Nord"));
    assert_eq!(settings.dark_theme.as_deref(), Some("Nord"));
    assert!(settings.follows_system_appearance());
    settings.dark_theme = Some("Night".into());
    settings.set_follow_system_appearance(false, true);
    assert_eq!(settings.theme.as_deref(), Some("Night"), "the dark side was on screen");
    assert_eq!(
        (settings.dark_theme.clone(), settings.follow_system_appearance),
        (None, None)
    );

    // ghostty's built-in cannot be a side of a pair
    let mut builtin = AppSettings::default();
    builtin.set_follow_system_appearance(true, true);
    assert_eq!(builtin.theme.as_deref(), Some(DEFAULT_LIGHT_THEME));
    assert_eq!(builtin.dark_theme.as_deref(), Some(DEFAULT_LIGHT_THEME));
}

#[test]
fn chrome_toggles_edit_the_raw_lists_and_keep_unknown_names() {
    let mut settings = AppSettings {
        hidden_interface_elements: Some(vec!["fromTheFuture".into()]),
        ..AppSettings::default()
    };
    settings.set_interface_element_visible(InterfaceElement::Split, false);
    settings.set_interface_element_visible(InterfaceElement::Dashboard, false);
    assert_eq!(
        settings.hidden_interface_elements,
        Some(vec!["dashboard".into(), "fromTheFuture".into(), "split".into()])
    );
    settings.set_interface_element_visible(InterfaceElement::Split, true);
    settings.set_interface_element_visible(InterfaceElement::Dashboard, true);
    assert_eq!(settings.hidden_interface_elements, Some(vec!["fromTheFuture".into()]));

    // a hidden-by-default element is opted in through the shown list, which empties back to nil
    settings.set_interface_element_visible(InterfaceElement::CustomCommands, true);
    assert_eq!(settings.shown_interface_elements, Some(vec!["customCommands".into()]));
    assert!(!settings.is_interface_element_hidden(InterfaceElement::CustomCommands));
    settings.set_interface_element_visible(InterfaceElement::CustomCommands, false);
    assert_eq!(settings.shown_interface_elements, None);
    assert!(settings.is_interface_element_hidden(InterfaceElement::CustomCommands));
}

#[test]
fn chrome_elements_split_between_title_bar_and_sidebar() {
    let sidebar: Vec<_> = InterfaceElement::ALL
        .iter()
        .filter(|element| !element.in_title_bar())
        .map(|element| element.display_name())
        .collect();
    assert_eq!(
        sidebar,
        [
            "New workspace",
            "New session",
            "Flagged view",
            "Workspace filter",
            "Workspace add-session"
        ]
    );
    assert_eq!(
        InterfaceElement::ALL
            .iter()
            .filter(|element| element.in_title_bar())
            .count(),
        12
    );
}

#[test]
fn the_quick_terminal_size_is_one_of_the_offered_choices() {
    let mut settings = applied([SettingsChange::Interface(InterfaceChange::QuickTerminalSizePercent(
        Some(70),
    ))]);
    assert_eq!(settings.quick_terminal_size_percent, Some(70));
    SettingsChange::Interface(InterfaceChange::QuickTerminalSizePercent(Some(65))).apply_to(&mut settings);
    assert_eq!(settings.quick_terminal_size_percent, None);
}

#[test]
fn glyph_colors_and_shapes_validate_and_reset_leaves_auto_follow() {
    let mut settings = applied([
        status(AgentStatusChange::Color {
            status: AgentStatus::Active,
            hex: Some("#102030".into()),
        }),
        status(AgentStatusChange::Color {
            status: AgentStatus::Blocked,
            hex: Some("not a color".into()),
        }),
        status(AgentStatusChange::Color {
            status: AgentStatus::Idle,
            hex: Some("#ffffff".into()),
        }),
        status(AgentStatusChange::Shape {
            status: AgentStatus::Completed,
            shape: StatusShape::Star,
        }),
        status(AgentStatusChange::BlockedSound(Some("Ping".into()))),
        status(AgentStatusChange::StatusReset(StatusReset::Enter)),
        status(AgentStatusChange::AutoFollow(AutoFollowAttention::S5)),
    ]);
    assert_eq!(settings.status_color_hex(AgentStatus::Active), Some("#102030"));
    assert_eq!(settings.blocked_status_color_hex, None);
    assert_eq!(
        settings.effective_status_shape(AgentStatus::Completed),
        Some(StatusShape::Star)
    );
    assert_eq!(
        settings.effective_status_shape(AgentStatus::Active),
        Some(StatusShape::Circle)
    );
    assert_eq!(settings.effective_status_shape(AgentStatus::Idle), None);
    assert_eq!(settings.blocked_status_sound_name.as_deref(), Some("Ping"));
    assert_eq!(settings.status_reset.as_deref(), Some("enter"));

    status(AgentStatusChange::Reset).apply_to(&mut settings);
    assert_eq!(
        settings,
        AppSettings {
            auto_follow_attention: Some("s5".into()),
            ..AppSettings::default()
        }
    );
}

#[test]
fn the_sidebar_tint_reads_as_a_direction_from_neutral() {
    assert_eq!(sidebar_shift_label(5), "None");
    assert_eq!(sidebar_shift_label(3), "Lighter 2");
    assert_eq!(sidebar_shift_label(10), "Darker 5");
}

#[test]
fn effective_values_resolve_unset_and_out_of_range() {
    let mut settings = AppSettings::default();
    let sizes = |settings: &AppSettings| {
        (
            settings.effective_scroll_speed(),
            settings.effective_sidebar_font_size(),
            settings.effective_interface_font_size(),
        )
    };
    assert_eq!(sizes(&settings), (3.0, 13.0, 13.0));
    assert_eq!(settings.effective_inactive_pane_mute(), 5);
    assert_eq!(settings.effective_sidebar_shift(), 5);
    assert_eq!(settings.effective_new_session_directory(), NewSessionDirectory::Home);
    settings.sidebar_font_size = Some(40.0);
    settings.interface_font_size = Some(f64::NAN);
    settings.inactive_pane_mute_strength = Some(-4);
    settings.new_session_directory = Some("custom".into());
    assert_eq!(sizes(&settings), (3.0, 20.0, 13.0));
    assert_eq!(settings.effective_inactive_pane_mute(), 0);
    assert_eq!(settings.effective_new_session_directory(), NewSessionDirectory::Custom);
}

/// Upstream's `muteOpacityScalesAndClamps`.
#[test]
fn mute_opacity_scales_and_clamps() {
    assert!(mute_opacity(0).abs() < f64::EPSILON);
    assert!((mute_opacity(5) - 0.4).abs() < 1e-9);
    assert!((mute_opacity(10) - 0.8).abs() < 1e-9);
    assert!(mute_opacity(-3).abs() < f64::EPSILON);
    assert!((mute_opacity(99) - 0.8).abs() < 1e-9);
    let mut settings = AppSettings::default();
    assert!((settings.inactive_pane_mute_opacity() - 0.4).abs() < 1e-9);
    settings.inactive_pane_mute_strength = Some(0);
    assert!(settings.inactive_pane_mute_opacity().abs() < f64::EPSILON);
}

#[test]
fn changes_round_trip_through_settings_json_minimally() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = persisted_core(dir.path());
    for change in [
        appearance(AppearanceChange::FontFamily(Some("Iosevka".into()))),
        general(GeneralChange::ScrollSpeed(6.0)),
        general(GeneralChange::NewSessionCustomDirectory("/work".into())),
        SettingsChange::Interface(InterfaceChange::ElementVisible {
            element: InterfaceElement::Split,
            visible: false,
        }),
        SettingsChange::WelcomeShown,
    ] {
        assert!(core.change_settings(change));
    }
    let loaded = SettingsStore::new(dir.path()).load();
    assert_eq!(loaded, migrated(core.settings()));
    assert_eq!(loaded.font_family.as_deref(), Some("Iosevka"));
    assert_eq!(loaded.welcome_shown, Some(true));

    // back to the defaults: the file keeps only what is not one
    for change in [
        general(GeneralChange::ScrollSpeed(3.0)),
        SettingsChange::Interface(InterfaceChange::ElementVisible {
            element: InterfaceElement::Split,
            visible: true,
        }),
    ] {
        assert!(core.change_settings(change));
    }
    let text = std::fs::read_to_string(SettingsStore::new(dir.path()).path()).unwrap();
    assert!(!text.contains("mouseScrollMultiplier"), "{text}");
    assert!(!text.contains("hiddenInterfaceElements"), "{text}");
    assert!(text.contains("\"newSessionCustomDirectory\" : \"\\/work\""), "{text}");
    assert_eq!(SettingsStore::new(dir.path()).load(), migrated(core.settings()));
}

#[test]
fn an_unchanged_value_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = persisted_core(dir.path());
    let path = SettingsStore::new(dir.path()).path();
    assert!(!core.change_settings(general(GeneralChange::RightClickPaste(true))));
    assert!(!path.exists());
}

#[test]
fn the_restore_mode_is_saved_for_the_next_launch() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = persisted_core(dir.path());
    assert!(core.change_settings(general(GeneralChange::RestoreMode(RestoreMode::Rerun))));
    assert!(!core.change_settings(general(GeneralChange::RestoreMode(RestoreMode::Rerun))));
    assert_eq!(
        SettingsStore::new(dir.path()).load().restore_mode,
        Some(RestoreMode::Rerun)
    );
}

#[test]
fn a_terminal_setting_reloads_the_config_and_a_session_setting_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = persisted_core(dir.path());
    core.write_settings_config();
    assert!(core.change_settings(general(GeneralChange::NewSessionPlacement(
        NewSessionPlacementSetting::AfterCurrent
    ))));
    assert!(!core.take_config_reload());
    assert!(core.change_settings(appearance(AppearanceChange::CursorStyle(Some(CursorStyle::Underline)))));
    assert!(core.take_config_reload());
}

#[test]
fn the_global_config_toggle_reloads_even_when_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = persisted_core(dir.path());
    assert!(!core.change_settings(general(GeneralChange::InheritGlobalGhosttyConfig(false))));
    assert!(core.take_config_reload());
    assert!(core.change_settings(general(GeneralChange::InheritGlobalGhosttyConfig(true))));
    assert_eq!(core.settings().inherit_global_ghostty_config, Some(true));
    assert!(core.take_config_reload());
}

#[test]
fn a_config_directory_change_reloads_the_keymap_from_there() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = persisted_core(dir.path());
    let custom = dir.path().join("elsewhere");
    std::fs::create_dir_all(&custom).unwrap();
    std::fs::write(custom.join("keymap.conf"), "not a keybind line\n").unwrap();
    let custom_path = custom.to_string_lossy().into_owned();
    assert!(core.change_settings(SettingsChange::ConfigDirectory(Some(custom_path.clone()))));
    assert_eq!(core.config_dir(), custom);
    assert_eq!(core.keymap_path(), custom.join("keymap.conf"));
    assert!(
        !core.keymap_diagnostics().is_empty(),
        "the new directory's keymap was read"
    );
    assert!(core.take_config_reload());

    assert!(core.change_settings(SettingsChange::ConfigDirectory(None)));
    assert_eq!(core.config_dir(), dir.path().join("config"));
    assert!(core.keymap_diagnostics().is_empty());
    // a blank directory is the default too
    assert!(!core.change_settings(SettingsChange::ConfigDirectory(Some("  ".into()))));
    assert_eq!(core.settings().config_directory, None);
}

#[test]
fn turning_auto_hide_on_hides_the_inactive_sidebars_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = persisted_core(dir.path());
    let open = core.library().open_ids();
    assert_eq!(open.len(), 2);
    let frontmost = core.library().active_window_id().unwrap();
    assert!(core.change_settings(SettingsChange::Interface(
        InterfaceChange::AutoHideSidebarInactiveWindows(true)
    )));
    for window in open {
        let visible = core.library().store(window).unwrap().sidebar().visible;
        assert_eq!(visible, window == frontmost);
    }
}

#[test]
fn a_chosen_directory_is_stored_exactly_and_a_blank_one_is_the_default() {
    let settings = applied([
        general(GeneralChange::NewSessionCustomDirectory("/tmp/work ".into())),
        SettingsChange::ConfigDirectory(Some(" /tmp/config ".into())),
    ]);
    assert_eq!(settings.new_session_custom_directory.as_deref(), Some("/tmp/work "));
    assert_eq!(settings.config_directory.as_deref(), Some(" /tmp/config "));
    let blank = applied([
        general(GeneralChange::NewSessionCustomDirectory("  ".into())),
        SettingsChange::ConfigDirectory(Some(String::new())),
    ]);
    assert_eq!(blank, AppSettings::default());
}
