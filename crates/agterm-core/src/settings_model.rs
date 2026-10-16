//! The Settings dialog's model (upstream's `SettingsModel.swift` setters and the bindings of `SettingsView.swift`),
//! host-free: one [`SettingsChange`] per control edit, grouped by the dialog's pages, and [`Core::change_settings`],
//! which writes it to `settings.json` and applies it the way upstream's setter does.
//!
//! Each change is validated on the way in, as the control's range bounds it (a slider or stepper value is rounded and
//! clamped, a non-finite one ignored), and a control's DEFAULT value is stored as nil, so `settings.json` stays
//! minimal; the one exception is the font size, stored explicitly even at its default so it beats `ghostty.conf`
//! (upstream's `setFontSize`). A chosen directory is stored exactly as picked. What a change does to the running app follows upstream's setter: most rewrite the settings' ghostty config
//! and reload the terminals only when its text changed ([`Effect::Live`]); the global-config toggle reloads the config
//! unconditionally, a config-directory change reloads the keymap, the hooks and the config, and a restore-mode change
//! applies to the next launch only and rolls back when it could not be written.

use std::collections::BTreeSet;
use std::ops::RangeInclusive;

use crate::Core;
use crate::model::{AgentStatus, StatusReset, StatusShape};
use crate::persist::RestoreMode;
use crate::persist::settings::{
    AppSettings, AutoFollowAttention, CursorStyle, DockBounce, FlaggedViewLayout, InterfaceElement, LinkOpenMode,
    NewSessionDirectory, NewSessionPlacementSetting, ToolbarMode,
};
use crate::quick_metrics::SIZE_PERCENT_CHOICES;
use crate::theme::DEFAULT_LIGHT_THEME;

/// The terminal font size stepper's bounds (upstream's `8 ... 32`) and the size an unset one reads as.
pub const FONT_SIZE_RANGE: RangeInclusive<f64> = 8.0..=32.0;
pub const DEFAULT_FONT_SIZE: f64 = 13.0;
/// The scroll speed slider's bounds and its default, stored as nil.
pub const SCROLL_SPEED_RANGE: RangeInclusive<f64> = 1.0..=10.0;
pub const DEFAULT_SCROLL_SPEED: f64 = 3.0;
/// The sidebar and the palette/switcher text sizes (upstream's `sidebarFontSizeRange`, `interfaceFontSizeRange`).
pub const SIDEBAR_FONT_SIZE_RANGE: RangeInclusive<f64> = 9.0..=20.0;
pub const DEFAULT_SIDEBAR_FONT_SIZE: f64 = 13.0;
pub const INTERFACE_FONT_SIZE_RANGE: RangeInclusive<f64> = 9.0..=20.0;
pub const DEFAULT_INTERFACE_FONT_SIZE: f64 = 13.0;
/// The inactive-pane mute and the sidebar tint sliders: `0...10`, neutral 5.
pub const STRENGTH_RANGE: RangeInclusive<i64> = 0..=10;
pub const DEFAULT_INACTIVE_PANE_MUTE: i64 = 5;
pub const DEFAULT_SIDEBAR_SHIFT: i64 = 5;
/// The background blur slider's bounds.
pub const BLUR_RANGE: RangeInclusive<i64> = 0..=100;

/// What a change does to the running app once written (upstream's setter bodies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Rewrites the settings' ghostty config; the terminals reload only when its text changed (`persistAndApply`).
    /// The chrome reads the settings at the next render.
    Live,
    /// Reloads the ghostty config whether or not anything changed: the global-config toggle changes WHICH files load.
    ReloadConfig,
    /// Reloads `keymap.conf`, `hooks.conf` and the ghostty config from the (new) config directory.
    ReloadConfigFiles,
    /// The inactive windows' sidebars hide at once when auto-hide was turned on; nothing reaches the ghostty config.
    HideInactiveSidebars,
}

/// One edit in the Settings dialog, by page.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsChange {
    General(GeneralChange),
    Appearance(AppearanceChange),
    Interface(InterfaceChange),
    Notifications(NotificationsChange),
    AgentStatus(AgentStatusChange),
    /// Key Mapping: the config directory, `None` for the default.
    ConfigDirectory(Option<String>),
    /// The first-run welcome was shown.
    WelcomeShown,
}

/// The General page: Mouse and Sessions.
#[derive(Debug, Clone, PartialEq)]
pub enum GeneralChange {
    ScrollSpeed(f64),
    RightClickPaste(bool),
    LinkOpenMode(LinkOpenMode),
    WorkspaceRowClickExpands(bool),
    NewSessionDirectory(NewSessionDirectory),
    NewSessionCustomDirectory(String),
    NewSessionPlacement(NewSessionPlacementSetting),
    /// The next launch's restore mode; see [`Core::save_restore_mode`].
    RestoreMode(RestoreMode),
    ConfirmCloseSession(bool),
    CloseGraceUndo(bool),
    FlaggedViewLayout(FlaggedViewLayout),
    InheritGlobalGhosttyConfig(bool),
}

/// The Appearance page: Terminal and Window. The theme edits carry the appearance the terminals render now (`dark`),
/// which picks the slot they write.
#[derive(Debug, Clone, PartialEq)]
pub enum AppearanceChange {
    FontFamily(Option<String>),
    FontSize(f64),
    CursorStyle(Option<CursorStyle>),
    CursorBlink(Option<bool>),
    ThemeForCurrentAppearance { theme: Option<String>, dark: bool },
    AlternateTheme { theme: Option<String>, dark: bool },
    FollowSystemAppearance { on: bool, dark: bool },
    ToolbarMode(ToolbarMode),
    BackgroundOpacity(f64),
    BackgroundBlur(i64),
    SidebarBackgroundShift(i64),
    SidebarFontSize(f64),
    InterfaceFontSize(f64),
    InactivePaneMute(i64),
}

/// The Interface page: chrome elements, multiple windows, the quick terminal's size.
#[derive(Debug, Clone, PartialEq)]
pub enum InterfaceChange {
    ElementVisible {
        element: InterfaceElement,
        visible: bool,
    },
    AutoHideSidebarInactiveWindows(bool),
    /// A percentage of [`SIZE_PERCENT_CHOICES`], `None` for the built-in size.
    QuickTerminalSizePercent(Option<i64>),
}

/// The Notifications page.
#[derive(Debug, Clone, PartialEq)]
pub enum NotificationsChange {
    Banners(bool),
    Badges(bool),
    DockBounce(DockBounce),
    /// A sound name, `None` for silence.
    Sound(Option<String>),
    AttentionButton(bool),
}

/// The Agent Status page. The glyph edits name `active`, `blocked` or `completed`; `idle` has no glyph and changes
/// nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentStatusChange {
    /// A `#rrggbb` color, `None` for the status's default.
    Color {
        status: AgentStatus,
        hex: Option<String>,
    },
    Shape {
        status: AgentStatus,
        shape: StatusShape,
    },
    BlockedSound(Option<String>),
    StatusReset(StatusReset),
    AutoFollow(AutoFollowAttention),
    /// The forward-reading toggle over `autoFollowStayOnActive`: on (the default) leaves a running session.
    AutoFollowAwayFromRunning(bool),
    /// Reset to defaults: colors, shapes, the blocked sound and the status reset; not auto-follow.
    Reset,
}

/// A default-on toggle: on is nil, off is stored.
fn default_on(on: bool) -> Option<bool> {
    (!on).then_some(false)
}

/// A default-off toggle: off is nil, on is stored.
fn default_off(on: bool) -> Option<bool> {
    on.then_some(true)
}

/// `value` rounded into `range`; `None` when it is not a number.
fn stepped(value: f64, range: &RangeInclusive<f64>) -> Option<f64> {
    value
        .is_finite()
        .then(|| value.round().clamp(*range.start(), *range.end()))
}

/// `value` stored unless it is `default`.
fn unless_default<T: PartialEq + Copy>(value: T, default: T) -> Option<T> {
    (value != default).then_some(value)
}

/// A non-empty, trimmed name; `None` for an empty one.
fn named(name: Option<String>) -> Option<String> {
    name.map(|name| name.trim().to_owned()).filter(|name| !name.is_empty())
}

/// A directory exactly as chosen (whitespace can be part of a path); `None` for a blank one.
fn directory(path: Option<String>) -> Option<String> {
    path.filter(|path| !path.trim().is_empty())
}

impl SettingsChange {
    /// Applies the change to `settings`, validated, and says what it does to the running app. A restore-mode change is
    /// applied here too, but [`Core::change_settings`] routes it through [`Core::save_restore_mode`].
    pub fn apply_to(self, settings: &mut AppSettings) -> Effect {
        match self {
            Self::General(change) => change.apply_to(settings),
            Self::Appearance(change) => {
                change.apply_to(settings);
                Effect::Live
            }
            Self::Interface(change) => change.apply_to(settings),
            Self::Notifications(change) => {
                change.apply_to(settings);
                Effect::Live
            }
            Self::AgentStatus(change) => {
                change.apply_to(settings);
                Effect::Live
            }
            Self::ConfigDirectory(path) => {
                settings.config_directory = directory(path);
                Effect::ReloadConfigFiles
            }
            Self::WelcomeShown => {
                settings.welcome_shown = Some(true);
                Effect::Live
            }
        }
    }
}

impl GeneralChange {
    fn apply_to(self, settings: &mut AppSettings) -> Effect {
        match self {
            Self::ScrollSpeed(speed) => {
                if let Some(speed) = stepped(speed, &SCROLL_SPEED_RANGE) {
                    settings.mouse_scroll_multiplier = unless_default(speed, DEFAULT_SCROLL_SPEED);
                }
            }
            Self::RightClickPaste(on) => settings.right_click_paste = default_on(on),
            Self::LinkOpenMode(mode) => {
                settings.link_open_mode = unless_default(mode, LinkOpenMode::Browser).map(|mode| mode.raw().to_owned());
            }
            Self::WorkspaceRowClickExpands(on) => settings.workspace_row_click_expands = default_on(on),
            Self::NewSessionDirectory(mode) => {
                settings.new_session_directory =
                    unless_default(mode, NewSessionDirectory::Home).map(|mode| mode.raw().to_owned());
            }
            Self::NewSessionCustomDirectory(path) => {
                settings.new_session_custom_directory = directory(Some(path));
            }
            Self::NewSessionPlacement(placement) => {
                settings.new_session_placement = unless_default(placement, NewSessionPlacementSetting::End)
                    .map(|placement| placement.raw().to_owned());
            }
            Self::RestoreMode(mode) => {
                settings.restore_mode = Some(mode);
                settings.restore_running_command = None;
            }
            Self::ConfirmCloseSession(on) => settings.confirm_close_session = default_off(on),
            Self::CloseGraceUndo(on) => settings.close_grace_undo_enabled = default_on(on),
            Self::FlaggedViewLayout(layout) => {
                settings.flagged_view_layout =
                    unless_default(layout, FlaggedViewLayout::Flat).map(|layout| layout.raw().to_owned());
            }
            Self::InheritGlobalGhosttyConfig(on) => {
                settings.inherit_global_ghostty_config = default_off(on);
                return Effect::ReloadConfig;
            }
        }
        Effect::Live
    }
}

impl AppearanceChange {
    fn apply_to(self, settings: &mut AppSettings) {
        match self {
            Self::FontFamily(family) => settings.font_family = named(family),
            Self::FontSize(size) => {
                if let Some(size) = stepped(size, &FONT_SIZE_RANGE) {
                    settings.font_size = Some(size);
                }
            }
            Self::CursorStyle(style) => settings.cursor_style = style.map(|style| style.raw().to_owned()),
            Self::CursorBlink(blink) => settings.cursor_blink = blink,
            Self::ThemeForCurrentAppearance { theme, dark } => {
                settings.set_theme_for_current_appearance(named(theme), dark);
            }
            Self::AlternateTheme { theme, dark } => settings.set_alternate_theme(named(theme), dark),
            Self::FollowSystemAppearance { on, dark } => settings.set_follow_system_appearance(on, dark),
            Self::ToolbarMode(mode) => {
                settings.toolbar_mode = unless_default(mode, ToolbarMode::Compact).map(|mode| mode.raw().to_owned());
                settings.compact_toolbar = None;
            }
            Self::BackgroundOpacity(opacity) => {
                if opacity.is_finite() {
                    settings.background_opacity = (opacity < 1.0).then(|| opacity.max(0.0));
                }
            }
            Self::BackgroundBlur(blur) => {
                let blur = blur.clamp(*BLUR_RANGE.start(), *BLUR_RANGE.end());
                settings.background_blur = (blur > 0).then_some(blur);
            }
            Self::SidebarBackgroundShift(shift) => {
                settings.sidebar_background_shift = unless_default(clamp_strength(shift), DEFAULT_SIDEBAR_SHIFT);
            }
            Self::SidebarFontSize(size) => {
                if let Some(size) = stepped(size, &SIDEBAR_FONT_SIZE_RANGE) {
                    settings.sidebar_font_size = unless_default(size, DEFAULT_SIDEBAR_FONT_SIZE);
                }
            }
            Self::InterfaceFontSize(size) => {
                if let Some(size) = stepped(size, &INTERFACE_FONT_SIZE_RANGE) {
                    settings.interface_font_size = unless_default(size, DEFAULT_INTERFACE_FONT_SIZE);
                }
            }
            Self::InactivePaneMute(strength) => {
                settings.inactive_pane_mute_strength =
                    unless_default(clamp_strength(strength), DEFAULT_INACTIVE_PANE_MUTE);
            }
        }
    }
}

fn clamp_strength(strength: i64) -> i64 {
    strength.clamp(*STRENGTH_RANGE.start(), *STRENGTH_RANGE.end())
}

/// The opacity of the mute wash at `strength` (upstream's `AppSettings.muteOpacity`): 0.08 a step, clamped, so 0.8 at
/// the strongest and nothing at 0.
#[must_use]
pub fn mute_opacity(strength: i64) -> f64 {
    // within 0...10 by the clamp, so the conversion is exact
    #[expect(clippy::cast_precision_loss, reason = "a strength within 0...10")]
    let steps = clamp_strength(strength) as f64;
    steps * 0.08
}

impl InterfaceChange {
    fn apply_to(self, settings: &mut AppSettings) -> Effect {
        match self {
            Self::ElementVisible { element, visible } => settings.set_interface_element_visible(element, visible),
            Self::AutoHideSidebarInactiveWindows(on) => {
                settings.auto_hide_sidebar_inactive_windows = default_off(on);
                return Effect::HideInactiveSidebars;
            }
            Self::QuickTerminalSizePercent(percent) => {
                settings.quick_terminal_size_percent = percent.filter(|percent| SIZE_PERCENT_CHOICES.contains(percent));
            }
        }
        Effect::Live
    }
}

impl NotificationsChange {
    fn apply_to(self, settings: &mut AppSettings) {
        match self {
            Self::Banners(on) => settings.notifications_enabled = default_on(on),
            Self::Badges(on) => settings.notification_badge_enabled = default_on(on),
            Self::DockBounce(bounce) => {
                settings.dock_bounce = unless_default(bounce, DockBounce::Off).map(|bounce| bounce.raw().to_owned());
            }
            Self::Sound(name) => settings.notification_sound_name = named(name),
            Self::AttentionButton(on) => settings.attention_button_enabled = default_off(on),
        }
    }
}

impl AgentStatusChange {
    fn apply_to(self, settings: &mut AppSettings) {
        match self {
            Self::Color { status, hex } => {
                let hex = hex
                    .map(|hex| hex.trim().to_owned())
                    .filter(|hex| crate::model::is_valid_color_hex(hex));
                if let Some(slot) = status_color_slot(settings, status) {
                    *slot = hex;
                }
            }
            Self::Shape { status, shape } => {
                if let Some(slot) = status_shape_slot(settings, status) {
                    *slot = unless_default(shape, StatusShape::Circle).map(|shape| shape.wire_name().to_owned());
                }
            }
            Self::BlockedSound(name) => settings.blocked_status_sound_name = named(name),
            Self::StatusReset(reset) => {
                settings.status_reset =
                    unless_default(reset, StatusReset::FirstKey).map(|reset| reset.raw().to_owned());
            }
            Self::AutoFollow(follow) => {
                settings.auto_follow_attention =
                    unless_default(follow, AutoFollowAttention::Off).map(|follow| follow.raw().to_owned());
            }
            Self::AutoFollowAwayFromRunning(away) => settings.auto_follow_stay_on_active = default_off(!away),
            Self::Reset => {
                for status in [AgentStatus::Active, AgentStatus::Blocked, AgentStatus::Completed] {
                    if let Some(slot) = status_color_slot(settings, status) {
                        *slot = None;
                    }
                    if let Some(slot) = status_shape_slot(settings, status) {
                        *slot = None;
                    }
                }
                settings.blocked_status_sound_name = None;
                settings.status_reset = None;
            }
        }
    }
}

fn status_color_slot(settings: &mut AppSettings, status: AgentStatus) -> Option<&mut Option<String>> {
    match status {
        AgentStatus::Active => Some(&mut settings.active_status_color_hex),
        AgentStatus::Blocked => Some(&mut settings.blocked_status_color_hex),
        AgentStatus::Completed => Some(&mut settings.completed_status_color_hex),
        AgentStatus::Idle => None,
    }
}

fn status_shape_slot(settings: &mut AppSettings, status: AgentStatus) -> Option<&mut Option<String>> {
    match status {
        AgentStatus::Active => Some(&mut settings.active_status_shape),
        AgentStatus::Blocked => Some(&mut settings.blocked_status_shape),
        AgentStatus::Completed => Some(&mut settings.completed_status_shape),
        AgentStatus::Idle => None,
    }
}

impl AppSettings {
    /// Whether the slot on screen is the dark one: following, and the terminals render dark.
    fn renders_dark_slot(&self, dark: bool) -> bool {
        self.follows_system_appearance() && dark
    }

    /// The theme picker's write (upstream's `setThemeForCurrentAppearance`): the slot on screen.
    pub fn set_theme_for_current_appearance(&mut self, theme: Option<String>, dark: bool) {
        if self.renders_dark_slot(dark) {
            self.dark_theme = theme;
        } else {
            self.theme = theme;
        }
    }

    /// The other appearance's theme picker, shown only while following (upstream's `setAlternateTheme`): the light
    /// slot while rendering dark, else the dark slot, whose clearing turns following off so it is never left on with
    /// no dark theme.
    pub fn set_alternate_theme(&mut self, theme: Option<String>, dark: bool) {
        if self.renders_dark_slot(dark) {
            self.theme = theme;
        } else if theme.is_none() {
            self.set_dark_theme(None);
        } else {
            self.dark_theme = theme;
        }
    }

    /// The theme the alternate picker shows: the light slot while rendering dark, else the dark slot.
    #[must_use]
    pub fn alternate_theme(&self, dark: bool) -> Option<&str> {
        if dark {
            self.theme.as_deref()
        } else {
            self.dark_theme.as_deref()
        }
    }

    /// The "Follow system appearance" toggle (upstream's `setFollowSystemAppearance`): on seeds the dark slot from the
    /// current theme (ghostty's built-in cannot be a side, so an unset theme becomes [`DEFAULT_LIGHT_THEME`]), a
    /// well-formed pair with no visual change; off collapses to the theme on screen, so nothing flips.
    pub fn set_follow_system_appearance(&mut self, on: bool, dark: bool) {
        if on {
            if self.theme.as_deref().is_none_or(str::is_empty) {
                self.theme = Some(DEFAULT_LIGHT_THEME.to_owned());
            }
            if self.dark_theme.is_none() {
                self.dark_theme.clone_from(&self.theme);
            }
            self.follow_system_appearance = Some(true);
        } else {
            if dark && self.dark_theme.is_some() {
                self.theme = self.dark_theme.take();
            }
            self.dark_theme = None;
            self.follow_system_appearance = None;
        }
    }

    /// Shows or hides one chrome element (upstream's `setInterfaceElementVisible`): a hidden-by-default element is
    /// listed in `shownInterfaceElements` when shown, any other in `hiddenInterfaceElements` when hidden. The raw lists
    /// are edited, so a name a newer build wrote survives; a list left empty is stored as nil, the rest sorted.
    pub fn set_interface_element_visible(&mut self, element: InterfaceElement, visible: bool) {
        let opt_in = element.hidden_by_default();
        let list = if opt_in {
            &mut self.shown_interface_elements
        } else {
            &mut self.hidden_interface_elements
        };
        let mut names: BTreeSet<String> = list.take().unwrap_or_default().into_iter().collect();
        if visible == opt_in {
            names.insert(element.raw().to_owned());
        } else {
            names.remove(element.raw());
        }
        *list = (!names.is_empty()).then(|| names.into_iter().collect());
    }

    /// The scroll speed the slider shows.
    #[must_use]
    pub fn effective_scroll_speed(&self) -> f64 {
        self.mouse_scroll_multiplier.unwrap_or(DEFAULT_SCROLL_SPEED)
    }

    /// The sidebar row-text size, clamped (upstream's `effectiveSidebarFontSize`).
    #[must_use]
    pub fn effective_sidebar_font_size(&self) -> f64 {
        clamp_size(
            self.sidebar_font_size,
            DEFAULT_SIDEBAR_FONT_SIZE,
            &SIDEBAR_FONT_SIZE_RANGE,
        )
    }

    /// The palette and switcher text size, clamped (upstream's `effectiveInterfaceFontSize`).
    #[must_use]
    pub fn effective_interface_font_size(&self) -> f64 {
        clamp_size(
            self.interface_font_size,
            DEFAULT_INTERFACE_FONT_SIZE,
            &INTERFACE_FONT_SIZE_RANGE,
        )
    }

    /// The inactive-pane mute the slider shows.
    #[must_use]
    pub fn effective_inactive_pane_mute(&self) -> i64 {
        clamp_strength(self.inactive_pane_mute_strength.unwrap_or(DEFAULT_INACTIVE_PANE_MUTE))
    }

    /// The opacity of the wash muting the inactive split pane and a floating panel's backdrop.
    #[must_use]
    pub fn inactive_pane_mute_opacity(&self) -> f64 {
        mute_opacity(self.effective_inactive_pane_mute())
    }

    /// The sidebar tint the slider shows.
    #[must_use]
    pub fn effective_sidebar_shift(&self) -> i64 {
        clamp_strength(self.sidebar_background_shift.unwrap_or(DEFAULT_SIDEBAR_SHIFT))
    }

    /// The new-session directory mode; unset or unknown is home.
    #[must_use]
    pub fn effective_new_session_directory(&self) -> NewSessionDirectory {
        self.new_session_directory
            .as_deref()
            .and_then(NewSessionDirectory::from_raw)
            .unwrap_or(NewSessionDirectory::Home)
    }

    /// The configured shape of a status's glyph, a circle when unset or unknown; `None` for `idle`.
    #[must_use]
    pub fn effective_status_shape(&self, status: AgentStatus) -> Option<StatusShape> {
        let raw = match status {
            AgentStatus::Active => &self.active_status_shape,
            AgentStatus::Blocked => &self.blocked_status_shape,
            AgentStatus::Completed => &self.completed_status_shape,
            AgentStatus::Idle => return None,
        };
        Some(
            raw.as_deref()
                .and_then(StatusShape::from_wire)
                .unwrap_or(StatusShape::Circle),
        )
    }

    /// The configured color of a status's glyph, when it is a valid hex; `None` for its default.
    #[must_use]
    pub fn status_color_hex(&self, status: AgentStatus) -> Option<&str> {
        let raw = match status {
            AgentStatus::Active => &self.active_status_color_hex,
            AgentStatus::Blocked => &self.blocked_status_color_hex,
            AgentStatus::Completed => &self.completed_status_color_hex,
            AgentStatus::Idle => return None,
        };
        raw.as_deref()
            .map(str::trim)
            .filter(|hex| crate::model::is_valid_color_hex(hex))
    }
}

fn clamp_size(size: Option<f64>, default: f64, range: &RangeInclusive<f64>) -> f64 {
    size.filter(|size| size.is_finite())
        .unwrap_or(default)
        .clamp(*range.start(), *range.end())
}

impl InterfaceElement {
    /// Where the element lives: the title bar (`true`) or the sidebar.
    #[must_use]
    pub fn in_title_bar(self) -> bool {
        !matches!(
            self,
            Self::NewWorkspace | Self::NewSession | Self::FlaggedView | Self::FocusFilter | Self::WorkspaceAddSession
        )
    }

    /// The toggle's label (upstream's `InterfaceElement.displayName`).
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::SidebarToggle => "Sidebar toggle",
            Self::WorkspaceName => "Workspace name",
            Self::SessionName => "Session name",
            Self::WindowName => "Window name",
            Self::RemoteHost => "Remote host",
            Self::SessionContext => "Session context",
            Self::RecentSessions => "Recent sessions",
            Self::Scratch => "Scratch terminal",
            Self::Split => "Split view",
            Self::Dashboard => "Dashboard",
            Self::QuickTerminal => "Quick terminal",
            Self::CustomCommands => "Custom commands",
            Self::NewWorkspace => "New workspace",
            Self::NewSession => "New session",
            Self::FlaggedView => "Flagged view",
            Self::FocusFilter => "Workspace filter",
            Self::WorkspaceAddSession => "Workspace add-session",
        }
    }
}

/// The sidebar tint slider's readout (upstream's `sidebarShiftLabel`): "None" at the neutral center, else the
/// direction and the distance from it.
#[must_use]
pub fn sidebar_shift_label(shift: i64) -> String {
    let offset = shift - DEFAULT_SIDEBAR_SHIFT;
    match offset {
        0 => "None".to_owned(),
        offset if offset < 0 => format!("Lighter {}", -offset),
        offset => format!("Darker {offset}"),
    }
}

impl Core {
    /// Writes one Settings edit to `settings.json` and applies it (upstream's `SettingsModel` setters); whether the
    /// settings changed. An unchanged value writes nothing, but the two reload toggles reload anyway, as upstream's do.
    /// Commit afterwards.
    pub fn change_settings(&mut self, change: SettingsChange) -> bool {
        if let SettingsChange::General(GeneralChange::RestoreMode(mode)) = change {
            let changed = self.settings().restore_mode != Some(mode);
            return self.save_restore_mode(mode) && changed;
        }
        if change.writes_theme_slots() && self.previewed_slots_to_persist().is_some() {
            return self.change_theme_mid_preview(change);
        }
        let mut effect = Effect::Live;
        let changed = self.update_settings(|settings| effect = change.apply_to(settings));
        match effect {
            Effect::Live => {
                if changed {
                    self.apply_settings();
                }
            }
            Effect::ReloadConfig => self.request_config_reload(),
            Effect::ReloadConfigFiles => {
                self.reload_keymap();
                self.reload_hooks();
                self.request_config_reload();
                // the frontend's watch follows the state to the new file
                self.repoint_remotes();
            }
            Effect::HideInactiveSidebars => {
                if changed && self.settings().auto_hide_sidebar_inactive_windows == Some(true) {
                    self.library_mut().apply_inactive_window_sidebar_hiding();
                }
            }
        }
        changed
    }

    /// A theme slot edit while the theme palette previews: an explicit change as `theme.set` is, so it lands over the
    /// slots the palette found, saves as asked, and the preview starts over from it, so neither the save nor the
    /// palette's later Escape undoes it (upstream's `persistAndApply` moves `committedTheme`). Whether the settings on
    /// screen changed.
    fn change_theme_mid_preview(&mut self, change: SettingsChange) -> bool {
        let before = self.settings().clone();
        self.with_theme_preview_rebased(|core| {
            core.update_settings(|settings| {
                change.apply_to(settings);
            });
            // the preview's slot went back under the edit: the terminals follow whether or not the edit changed it
            core.apply_settings();
        });
        *self.settings() != before
    }
}

impl SettingsChange {
    /// Whether the change writes the theme slots (the Appearance page's theme pickers and the follow toggle), which an
    /// open theme preview holds.
    #[must_use]
    pub fn writes_theme_slots(&self) -> bool {
        matches!(
            self,
            Self::Appearance(
                AppearanceChange::ThemeForCurrentAppearance { .. }
                    | AppearanceChange::AlternateTheme { .. }
                    | AppearanceChange::FollowSystemAppearance { .. }
            )
        )
    }
}

#[cfg(test)]
mod tests;
