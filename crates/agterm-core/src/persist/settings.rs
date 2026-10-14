//! The app-wide settings in `<state dir>/settings.json` (upstream's `AppSettings.swift`, `SettingsStore.swift`,
//! `RestoreMode.swift`).
//!
//! Every field is optional, nil meaning "the default": a file written before a field existed still decodes, so
//! there is no version field. Enum-valued fields are stored as raw strings and read back through an `effective_*`
//! accessor that resolves an unknown (future-written) value to the default instead of failing the decode; only
//! [`RestoreMode`] is typed, with its own tolerant decode. A present field of the wrong JSON type fails the whole
//! decode, as Swift's synthesized `Decodable` does, and [`SettingsStore::load`] then falls back to the seeded
//! default. Unknown keys are ignored on read and dropped on the next write, as upstream does.
//!
//! The accessors whose consumers come later (the ghostty config lines, status shapes, the HTML
//! zoom, the quick-terminal size grid) join with those consumers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::json;
use crate::model::StatusReset;

/// The settings file's name in the state directory.
pub const SETTINGS_FILE_NAME: &str = "settings.json";

/// How session processes come back after an app restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum RestoreMode {
    /// Fresh shells.
    #[default]
    None,
    /// Re-run the captured commands.
    Rerun,
    /// Reattach the live-session daemons.
    Live,
}

impl RestoreMode {
    /// The stored and wire spelling.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Rerun => "rerun",
            Self::Live => "live",
        }
    }

    /// Parses the stored spelling; `None` for anything else.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "none" => Some(Self::None),
            "rerun" => Some(Self::Rerun),
            "live" => Some(Self::Live),
            _ => None,
        }
    }

    /// The label the settings show.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::None => "Fresh shells",
            Self::Rerun => "Re-run commands",
            Self::Live => "Live sessions",
        }
    }
}

impl Serialize for RestoreMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_name())
    }
}

/// A string that names no mode (a newer build's) decodes as [`RestoreMode::None`]; a non-string fails.
impl<'de> Deserialize<'de> for RestoreMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::from_wire(&raw).unwrap_or_default())
    }
}

/// Declares a raw-stored settings enum: its cases with their stored names, and a parser.
macro_rules! raw_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$case_meta:meta])* $case:ident => $raw:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$case_meta])* $case),+
        }

        impl $name {
            /// Every case, in declaration order.
            pub const ALL: &[Self] = &[$(Self::$case),+];

            /// The stored spelling.
            #[must_use]
            pub fn raw(self) -> &'static str {
                match self {
                    $(Self::$case => $raw),+
                }
            }

            /// Parses the stored spelling; `None` for an unknown one.
            #[must_use]
            pub fn from_raw(raw: &str) -> Option<Self> {
                match raw {
                    $($raw => Some(Self::$case),)+
                    _ => None,
                }
            }
        }
    };
}

raw_enum! {
    /// The window's titlebar row: the session name over a second line, one short row, or none.
    ToolbarMode { Normal => "normal", Compact => "compact", Hidden => "hidden" }
}

raw_enum! {
    /// How the sidebar's flagged view arranges its sessions.
    FlaggedViewLayout { Flat => "flat", Plain => "plain", Tree => "tree" }
}

raw_enum! {
    /// Where a clicked web link from terminal output opens.
    LinkOpenMode { Browser => "browser", Overlay => "overlay" }
}

raw_enum! {
    /// How a delivered notification asks for the user's attention at the app icon.
    DockBounce { Off => "off", Once => "once", UntilFocused => "untilFocused" }
}

raw_enum! {
    /// Where a new session opens.
    NewSessionDirectory { Home => "home", CurrentSession => "currentSession", Custom => "custom" }
}

raw_enum! {
    /// Where a new session lands in its workspace.
    NewSessionPlacementSetting { End => "end", AfterCurrent => "afterCurrent" }
}

raw_enum! {
    /// The terminal cursor shape, by ghostty's own `cursor-style` names.
    CursorStyle { Block => "block", Bar => "bar", Underline => "underline" }
}

raw_enum! {
    /// The idle timeout after which a window's selection follows to the oldest blocked session.
    AutoFollowAttention {
        Off => "off", S5 => "s5", S10 => "s10", S30 => "s30", S60 => "s60", M5 => "m5",
    }
}

impl AutoFollowAttention {
    /// Unknown or unset is `off`.
    #[must_use]
    pub fn tolerant(raw: Option<&str>) -> Self {
        raw.and_then(Self::from_raw).unwrap_or(Self::Off)
    }

    /// The idle grace in seconds, `None` when off.
    #[must_use]
    pub fn timeout_secs(self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::S5 => Some(5),
            Self::S10 => Some(10),
            Self::S30 => Some(30),
            Self::S60 => Some(60),
            Self::M5 => Some(300),
        }
    }
}

raw_enum! {
    /// A toggleable title-bar or sidebar chrome element, persisted by raw name.
    InterfaceElement {
        SidebarToggle => "sidebarToggle",
        WorkspaceName => "workspaceName",
        SessionName => "sessionName",
        WindowName => "windowName",
        RemoteHost => "remoteHost",
        SessionContext => "sessionContext",
        RecentSessions => "recentSessions",
        Scratch => "scratch",
        Split => "split",
        Dashboard => "dashboard",
        QuickTerminal => "quickTerminal",
        CustomCommands => "customCommands",
        NewWorkspace => "newWorkspace",
        NewSession => "newSession",
        FlaggedView => "flaggedView",
        FocusFilter => "focusFilter",
        WorkspaceAddSession => "workspaceAddSession",
    }
}

impl InterfaceElement {
    /// Whether the element starts hidden, so showing it is the opt-in.
    #[must_use]
    pub fn hidden_by_default(self) -> bool {
        matches!(self, Self::CustomCommands | Self::WorkspaceName)
    }
}

/// The user's settings, every field optional (upstream's `AppSettings`; field docs there).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
    /// The ghostty theme (the light slot while following the system appearance); `None` is ghostty's built-in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dark_theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_system_appearance: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_blink: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_blur: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notifications_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notification_badge_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toolbar_mode: Option<String>,
    /// Legacy two-state toggle, read only while `toolbar_mode` is unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_toolbar: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_status_color_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_status_color_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_status_color_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_status_shape: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_status_shape: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_status_shape: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_scroll_multiplier: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inactive_pane_mute_strength: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_background_shift: Option<i64>,
    /// `None` only before [`AppSettings::migrate_restore_mode`] ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_mode: Option<RestoreMode>,
    /// Legacy boolean, cleared by the migration before any write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_running_command: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherit_global_ghostty_config: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention_button_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dock_bounce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notification_sound_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_status_sound_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_reset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_click_paste: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_row_click_expands: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_session_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_session_custom_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_session_placement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm_close_session: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_grace_undo_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_follow_attention: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_follow_stay_on_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_font_size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface_font_size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quick_terminal_size_percent: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_interface_elements: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shown_interface_elements: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_hide_sidebar_inactive_windows: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flagged_view_layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_open_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html_overlay_zoom: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub welcome_shown: Option<bool>,
}

impl AppSettings {
    /// The bundled theme a fresh install starts on; distinct from an unset theme, which is ghostty's built-in.
    pub const DEFAULT_THEME: &str = "agterm";

    /// The configured restore mode, a legacy boolean included.
    #[must_use]
    pub fn effective_restore_mode(&self) -> RestoreMode {
        self.restore_mode
            .unwrap_or(if self.restore_running_command == Some(true) {
                RestoreMode::Rerun
            } else {
                RestoreMode::None
            })
    }

    /// Converts the legacy boolean in memory, so a save writes only `restoreMode`.
    pub fn migrate_restore_mode(&mut self) {
        self.restore_mode = Some(self.effective_restore_mode());
        self.restore_running_command = None;
    }

    /// The hidden chrome elements: a default-shown one when listed hidden, a hidden-by-default one unless listed
    /// shown. Unknown names are dropped.
    #[must_use]
    pub fn resolved_hidden_interface_elements(&self) -> Vec<InterfaceElement> {
        let listed = |list: &Option<Vec<String>>, element: InterfaceElement| {
            list.as_deref()
                .is_some_and(|names| names.iter().any(|name| name == element.raw()))
        };
        InterfaceElement::ALL
            .iter()
            .copied()
            .filter(|element| {
                if element.hidden_by_default() {
                    !listed(&self.shown_interface_elements, *element)
                } else {
                    listed(&self.hidden_interface_elements, *element)
                }
            })
            .collect()
    }

    /// Whether a chrome element is hidden.
    #[must_use]
    pub fn is_interface_element_hidden(&self, element: InterfaceElement) -> bool {
        self.resolved_hidden_interface_elements().contains(&element)
    }

    /// The titlebar row: a known `toolbarMode`, else the legacy `compactToolbar` (only `false` is the normal bar).
    #[must_use]
    pub fn effective_toolbar_mode(&self) -> ToolbarMode {
        self.toolbar_mode
            .as_deref()
            .and_then(ToolbarMode::from_raw)
            .unwrap_or(if self.compact_toolbar == Some(false) {
                ToolbarMode::Normal
            } else {
                ToolbarMode::Compact
            })
    }

    #[must_use]
    pub fn effective_flagged_view_layout(&self) -> FlaggedViewLayout {
        self.flagged_view_layout
            .as_deref()
            .and_then(FlaggedViewLayout::from_raw)
            .unwrap_or(FlaggedViewLayout::Flat)
    }

    #[must_use]
    pub fn effective_link_open_mode(&self) -> LinkOpenMode {
        self.link_open_mode
            .as_deref()
            .and_then(LinkOpenMode::from_raw)
            .unwrap_or(LinkOpenMode::Browser)
    }

    #[must_use]
    pub fn effective_dock_bounce(&self) -> DockBounce {
        self.dock_bounce
            .as_deref()
            .and_then(DockBounce::from_raw)
            .unwrap_or(DockBounce::Off)
    }

    #[must_use]
    pub fn effective_new_session_placement(&self) -> NewSessionPlacementSetting {
        self.new_session_placement
            .as_deref()
            .and_then(NewSessionPlacementSetting::from_raw)
            .unwrap_or(NewSessionPlacementSetting::End)
    }

    /// The picked cursor shape; `None` when unset or not one this build offers.
    #[must_use]
    pub fn effective_cursor_style(&self) -> Option<CursorStyle> {
        self.cursor_style.as_deref().and_then(CursorStyle::from_raw)
    }

    #[must_use]
    pub fn effective_auto_follow_attention(&self) -> AutoFollowAttention {
        AutoFollowAttention::tolerant(self.auto_follow_attention.as_deref())
    }

    /// Which keystroke clears a blocked or completed status glyph; `firstKey` when unset or unknown (upstream's
    /// `effectiveStatusReset`).
    #[must_use]
    pub fn effective_status_reset(&self) -> StatusReset {
        self.status_reset
            .as_deref()
            .and_then(StatusReset::from_raw)
            .unwrap_or_default()
    }

    /// The directory a new session opens in: home by default, the active session's cwd or the custom directory
    /// when picked and not blank.
    #[must_use]
    pub fn resolve_new_session_cwd(&self, current_session_cwd: Option<&str>, home: &str) -> String {
        let mode = self
            .new_session_directory
            .as_deref()
            .and_then(NewSessionDirectory::from_raw)
            .unwrap_or(NewSessionDirectory::Home);
        let chosen = match mode {
            NewSessionDirectory::Home => None,
            NewSessionDirectory::CurrentSession => current_session_cwd,
            NewSessionDirectory::Custom => self.new_session_custom_directory.as_deref(),
        };
        chosen.filter(|dir| !dir.is_empty()).unwrap_or(home).to_owned()
    }
}

/// Reads and writes [`AppSettings`] in the state directory (upstream's `SettingsStore`).
#[derive(Debug, Clone)]
pub struct SettingsStore {
    directory: PathBuf,
}

impl SettingsStore {
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// The settings file's path.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.directory.join(SETTINGS_FILE_NAME)
    }

    /// The settings a fresh install starts from: the defaults with the bundled theme.
    #[must_use]
    pub fn seeded_default() -> AppSettings {
        AppSettings {
            theme: Some(AppSettings::DEFAULT_THEME.to_owned()),
            ..AppSettings::default()
        }
    }

    /// Loads the settings, migrated; a missing, unreadable or undecodable file reads as
    /// [`SettingsStore::seeded_default`]. An existing file without `theme` keeps ghostty's built-in theme, so an
    /// existing user is never re-themed.
    #[must_use]
    pub fn load(&self) -> AppSettings {
        let Some(mut settings) = Self::read(&self.path()) else {
            return Self::seeded_default();
        };
        settings.migrate_restore_mode();
        settings
    }

    fn read(path: &Path) -> Option<AppSettings> {
        let data = std::fs::read(path).ok()?;
        serde_json::from_slice(&data).ok()
    }

    /// Writes the settings, migrated, atomically; creates the directory if needed.
    ///
    /// # Errors
    ///
    /// The directory could not be created or the file not written.
    pub fn save(&self, settings: &AppSettings) -> std::io::Result<()> {
        let mut settings = settings.clone();
        settings.migrate_restore_mode();
        json::save_json(&self.path(), &settings)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn store() -> (tempfile::TempDir, SettingsStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(dir.path());
        (dir, store)
    }

    fn seed(store: &SettingsStore, text: &str) {
        fs::write(store.path(), text).unwrap();
    }

    #[test]
    fn save_load_round_trip() {
        let (_dir, store) = store();
        let settings = AppSettings {
            font_family: Some("Menlo".to_owned()),
            font_size: Some(15.0),
            theme: Some("Adwaita Dark".to_owned()),
            ..AppSettings::default()
        };
        store.save(&settings).unwrap();
        let mut expected = settings;
        expected.migrate_restore_mode();
        assert_eq!(store.load(), expected);
    }

    #[test]
    fn a_missing_or_corrupt_file_seeds_the_default_theme() {
        let (_dir, store) = store();
        assert_eq!(store.load(), SettingsStore::seeded_default());
        assert_eq!(store.load().theme.as_deref(), Some("agterm"));
        seed(&store, "{ not valid json ]");
        assert_eq!(store.load(), SettingsStore::seeded_default());
    }

    #[test]
    fn a_wrong_type_fails_the_whole_decode_like_swift() {
        let (_dir, store) = store();
        seed(&store, r#"{"fontSize": "big", "theme": "x"}"#);
        assert_eq!(store.load(), SettingsStore::seeded_default());
    }

    #[test]
    fn an_existing_file_without_theme_stays_on_ghosttys_default() {
        let (_dir, store) = store();
        seed(&store, r#"{"fontSize":14}"#);
        assert_eq!(store.load().theme, None);
        assert_eq!(store.load().font_size, Some(14.0));
    }

    #[test]
    fn save_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = SettingsStore::new(dir.path().join("does/not/exist/yet"));
        let settings = AppSettings {
            theme: Some("Alabaster".to_owned()),
            ..AppSettings::default()
        };
        nested.save(&settings).unwrap();
        assert_eq!(nested.load().theme.as_deref(), Some("Alabaster"));
    }

    #[test]
    fn load_migrates_the_restore_mode() {
        let cases = [
            (r#"{"restoreRunningCommand":true}"#, RestoreMode::Rerun),
            (r#"{"restoreRunningCommand":false}"#, RestoreMode::None),
            ("{}", RestoreMode::None),
            (r#"{"restoreMode":"future"}"#, RestoreMode::None),
            (r#"{"restoreMode":"live"}"#, RestoreMode::Live),
        ];
        let (_dir, store) = store();
        for (text, expected) in cases {
            seed(&store, text);
            let settings = store.load();
            assert_eq!(settings.restore_mode, Some(expected), "{text}");
            assert_eq!(settings.restore_running_command, None, "{text}");
        }
    }

    #[test]
    fn save_writes_only_restore_mode() {
        let (_dir, store) = store();
        store
            .save(&AppSettings {
                restore_running_command: Some(true),
                ..AppSettings::default()
            })
            .unwrap();
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(text.contains(r#""restoreMode" : "rerun""#), "{text}");
        assert!(!text.contains("restoreRunningCommand"), "{text}");
    }

    /// The conformance harness seeds `settings.json` with Python's `json.dumps` before launch.
    #[test]
    fn harness_seeded_settings_decode() {
        let (_dir, store) = store();
        seed(&store, r#"{"restoreMode": "live"}"#);
        assert_eq!(store.load().effective_restore_mode(), RestoreMode::Live);
        seed(&store, r#"{"autoFollowAttention": "s30"}"#);
        let settings = store.load();
        assert_eq!(settings.effective_auto_follow_attention().timeout_secs(), Some(30));
        assert_eq!(settings.theme, None, "a seeded file is an existing user's");
        seed(&store, r#"{"attentionButtonEnabled": true}"#);
        assert_eq!(store.load().attention_button_enabled, Some(true));
        seed(&store, r#"{"shownInterfaceElements": ["customCommands"]}"#);
        let settings = store.load();
        assert!(!settings.is_interface_element_hidden(InterfaceElement::CustomCommands));
        assert!(settings.is_interface_element_hidden(InterfaceElement::WorkspaceName));
    }

    #[test]
    fn unknown_keys_are_ignored_and_unset_fields_omitted() {
        let (_dir, store) = store();
        seed(&store, r#"{"fromTheFuture": {"x": 1}, "theme": "x"}"#);
        let settings = store.load();
        assert_eq!(settings.theme.as_deref(), Some("x"));
        store.save(&settings).unwrap();
        assert_eq!(
            fs::read_to_string(store.path()).unwrap(),
            "{\n  \"restoreMode\" : \"none\",\n  \"theme\" : \"x\"\n}"
        );
    }

    #[test]
    fn effective_accessors_resolve_unknown_values_to_defaults() {
        let mut settings = AppSettings::default();
        assert_eq!(settings.effective_toolbar_mode(), ToolbarMode::Compact);
        settings.compact_toolbar = Some(false);
        assert_eq!(settings.effective_toolbar_mode(), ToolbarMode::Normal);
        settings.toolbar_mode = Some("hidden".to_owned());
        assert_eq!(settings.effective_toolbar_mode(), ToolbarMode::Hidden);
        settings.toolbar_mode = Some("future".to_owned());
        assert_eq!(settings.effective_toolbar_mode(), ToolbarMode::Normal);
        settings.flagged_view_layout = Some("tree".to_owned());
        settings.link_open_mode = Some("bogus".to_owned());
        settings.dock_bounce = Some("untilFocused".to_owned());
        settings.cursor_style = Some("block_hollow".to_owned());
        settings.new_session_placement = Some("afterCurrent".to_owned());
        assert_eq!(settings.effective_flagged_view_layout(), FlaggedViewLayout::Tree);
        assert_eq!(settings.effective_link_open_mode(), LinkOpenMode::Browser);
        assert_eq!(settings.effective_dock_bounce(), DockBounce::UntilFocused);
        assert_eq!(settings.effective_cursor_style(), None);
        assert_eq!(
            settings.effective_new_session_placement(),
            NewSessionPlacementSetting::AfterCurrent
        );
        assert_eq!(AutoFollowAttention::tolerant(Some("s7")), AutoFollowAttention::Off);
        assert_eq!(AutoFollowAttention::M5.timeout_secs(), Some(300));
    }

    /// Upstream's `linkOpenModeDefaultsToBrowserResolvesTolerantlyAndIsNotAGhosttyKey`.
    #[test]
    fn the_link_open_mode_defaults_to_browser_resolves_tolerantly_and_is_not_a_ghostty_key() {
        assert_eq!(AppSettings::default().effective_link_open_mode(), LinkOpenMode::Browser);
        let with_mode = |raw: &str| AppSettings {
            link_open_mode: Some(raw.to_owned()),
            ..AppSettings::default()
        };
        assert_eq!(
            with_mode("overlay").ghostty_config_lines(),
            AppSettings::default().ghostty_config_lines()
        );
        let (_dir, store) = store();
        for mode in LinkOpenMode::ALL {
            store.save(&with_mode(mode.raw())).unwrap();
            let decoded = store.load();
            assert_eq!(decoded.link_open_mode.as_deref(), Some(mode.raw()));
            assert_eq!(decoded.effective_link_open_mode(), *mode);
        }
        assert_eq!(with_mode("tab").effective_link_open_mode(), LinkOpenMode::Browser);
        seed(&store, r#"{ "fontSize": 16 }"#);
        let legacy = store.load();
        assert_eq!(legacy.link_open_mode, None);
        assert_eq!(legacy.effective_link_open_mode(), LinkOpenMode::Browser);
    }

    #[test]
    fn hidden_interface_elements_resolve_both_lists() {
        let mut settings = AppSettings::default();
        assert_eq!(
            settings.resolved_hidden_interface_elements(),
            [InterfaceElement::WorkspaceName, InterfaceElement::CustomCommands]
        );
        settings.hidden_interface_elements = Some(vec!["split".to_owned(), "nope".to_owned()]);
        settings.shown_interface_elements = Some(vec!["workspaceName".to_owned()]);
        assert_eq!(
            settings.resolved_hidden_interface_elements(),
            [InterfaceElement::Split, InterfaceElement::CustomCommands]
        );
    }

    #[test]
    fn new_session_cwd_falls_back_to_home() {
        let mut settings = AppSettings::default();
        assert_eq!(settings.resolve_new_session_cwd(Some("/cur"), "/home"), "/home");
        settings.new_session_directory = Some("currentSession".to_owned());
        assert_eq!(settings.resolve_new_session_cwd(Some("/cur"), "/home"), "/cur");
        assert_eq!(settings.resolve_new_session_cwd(Some(""), "/home"), "/home");
        settings.new_session_directory = Some("custom".to_owned());
        assert_eq!(settings.resolve_new_session_cwd(Some("/cur"), "/home"), "/home");
        settings.new_session_custom_directory = Some("/fixed".to_owned());
        assert_eq!(settings.resolve_new_session_cwd(None, "/home"), "/fixed");
    }

    #[test]
    fn restore_mode_names() {
        for mode in [RestoreMode::None, RestoreMode::Rerun, RestoreMode::Live] {
            assert_eq!(RestoreMode::from_wire(mode.wire_name()), Some(mode));
        }
        assert_eq!(RestoreMode::Live.display_name(), "Live sessions");
    }
}
