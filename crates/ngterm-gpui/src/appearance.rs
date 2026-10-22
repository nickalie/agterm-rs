//! Following the desktop's light or dark appearance (upstream's `SystemAppearanceObserver.swift` and
//! `SettingsModel.appearanceChanged`; ported from the GTK build's `theme.rs`, which read libadwaita's style manager):
//! the XDG settings portal's `org.freedesktop.appearance` `color-scheme` over `zbus`, which is what libadwaita follows
//! on every desktop that runs a portal backend (GNOME, KDE, the wlroots portals, darkman).
//!
//! - **Launch**: [`Portal::session`] reads the scheme before the config loads ([`Portal::is_dark`]), so the first
//!   terminal starts on the desktop's side of a dual `theme = light:…,dark:…`. No session bus, no portal, or a portal
//!   without the key reads as light, libadwaita's default for "no preference".
//! - **Following**: [`Portal::follow`] subscribes to the portal's `SettingChanged` for the key on a thread of its own
//!   and reads the key once more after subscribing, so no change between the launch read and the subscription is lost.
//!   Each value is debounced on the main loop (upstream's 0.05 s) and lands in [`AppState::appearance_changed`].
//! - **The flip**: as upstream, a new side re-feeds the config only while following the system appearance with both
//!   theme slots set ([`Appearance::flip_reloads`]); every reload, whichever path asked for it, loads the side
//!   [`Appearance::side`] gives and records it ([`AppState::config_applied`]). The terminals keep their font sizes
//!   across it (a config change never resets a live size here), and each takes the side as its color scheme
//!   (`TerminalConfig::color_scheme`).
//! - **`debug.appearance`** forces a side for good, as the GTK build's forced style-manager scheme did: the desktop
//!   no longer moves the app, and the flip runs at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ngterm_core::persist::AppSettings;
use zbus::blocking::{Connection, Proxy};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedValue, Value};

use crate::state::{AppState, Poster};

/// The portal's bus name, object path and settings interface.
pub const PORTAL: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub const SETTINGS_INTERFACE: &str = "org.freedesktop.portal.Settings";
/// The setting: its namespace and key.
pub const NAMESPACE: &str = "org.freedesktop.appearance";
pub const KEY: &str = "color-scheme";

/// How long a burst of appearance changes settles before one report (upstream's `appearanceDebounceInterval`).
pub const DEBOUNCE: Duration = Duration::from_millis(50);
/// How long a portal call may take: the launch waits for the first read, which may start the portal.
const CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// The portal's `color-scheme` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorScheme {
    /// 0, and any value the specification does not define.
    #[default]
    NoPreference,
    /// 1.
    PreferDark,
    /// 2.
    PreferLight,
}

impl ColorScheme {
    /// The scheme of the portal's value `value`.
    #[must_use]
    pub fn from_value(value: u32) -> Self {
        match value {
            1 => Self::PreferDark,
            2 => Self::PreferLight,
            _ => Self::NoPreference,
        }
    }

    /// The scheme a setting's `value` carries: a `u32`, possibly wrapped in variants (the deprecated `Read` wraps it
    /// twice); `None` for any other type.
    #[must_use]
    pub fn from_variant(value: &Value<'_>) -> Option<Self> {
        match value {
            Value::U32(raw) => Some(Self::from_value(*raw)),
            Value::Value(inner) => Self::from_variant(inner),
            _ => None,
        }
    }

    /// Whether the desktop prefers dark: no preference is light, as libadwaita reads it.
    #[must_use]
    pub fn is_dark(self) -> bool {
        self == Self::PreferDark
    }
}

/// A connection to the desktop's settings portal.
pub struct Portal {
    connection: Connection,
}

impl Portal {
    /// The portal on the session bus; `None` (logged) without one.
    #[must_use]
    pub fn session() -> Option<Self> {
        let connected = zbus::blocking::connection::Builder::session()
            .map(|builder| builder.method_timeout(CALL_TIMEOUT))
            .and_then(zbus::blocking::connection::Builder::build);
        match connected {
            Ok(connection) => Some(Self::over(connection)),
            Err(error) => {
                tracing::info!(%error, "appearance: no session bus; the desktop reads as light");
                None
            }
        }
    }

    /// The portal reached over `connection`.
    #[must_use]
    pub fn over(connection: Connection) -> Self {
        Self { connection }
    }

    fn proxy(&self) -> zbus::Result<Proxy<'static>> {
        zbus::blocking::proxy::Builder::new(&self.connection)
            .destination(PORTAL)?
            .path(PORTAL_PATH)?
            .interface(SETTINGS_INTERFACE)?
            .cache_properties(CacheProperties::No)
            .build()
    }

    /// The desktop's color scheme now: `ReadOne`, else the deprecated `Read` of portals before version 2.
    ///
    /// # Errors
    ///
    /// The portal is not there, does not have the setting, or answered with another type.
    pub fn color_scheme(&self) -> zbus::Result<ColorScheme> {
        let proxy = self.proxy()?;
        let value: OwnedValue = match proxy.call("ReadOne", &(NAMESPACE, KEY)) {
            Ok(value) => value,
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
            {
                proxy.call("Read", &(NAMESPACE, KEY))?
            }
            Err(error) => return Err(error),
        };
        ColorScheme::from_variant(&value).ok_or_else(|| zbus::Error::Failure(format!("{NAMESPACE} {KEY}: {value:?}")))
    }

    /// Whether the desktop prefers dark now; light when the portal cannot say (logged).
    #[must_use]
    pub fn is_dark(&self) -> bool {
        match self.color_scheme() {
            Ok(scheme) => {
                tracing::info!(?scheme, "appearance: the desktop's color scheme");
                scheme.is_dark()
            }
            Err(error) => {
                tracing::info!(%error, "appearance: the settings portal has no color scheme; the desktop reads as light");
                false
            }
        }
    }

    /// Reports every color scheme the portal announces from now on to `report`, on a thread of its own, starting with
    /// one read made after subscribing. The thread ends with the connection.
    ///
    /// # Errors
    ///
    /// The thread could not start.
    pub fn follow(self, report: impl Fn(ColorScheme) + Send + 'static) -> std::io::Result<std::thread::JoinHandle<()>> {
        std::thread::Builder::new()
            .name("ngterm-appearance".to_owned())
            .spawn(move || self.listen(&report))
    }

    fn listen(&self, report: &dyn Fn(ColorScheme)) {
        let changes = self
            .proxy()
            .and_then(|proxy| proxy.receive_signal_with_args("SettingChanged", &[(0, NAMESPACE), (1, KEY)]));
        let changes = match changes {
            Ok(changes) => changes,
            Err(error) => {
                tracing::warn!(%error, "appearance: cannot follow the settings portal");
                return;
            }
        };
        if let Ok(scheme) = self.color_scheme() {
            report(scheme);
        }
        for message in changes {
            let body = message.body();
            match body.deserialize::<(String, String, OwnedValue)>() {
                Ok((_, _, value)) => {
                    if let Some(scheme) = ColorScheme::from_variant(&value) {
                        report(scheme);
                    } else {
                        tracing::debug!(?value, "appearance: a color scheme of another type");
                    }
                }
                Err(error) => tracing::debug!(%error, "appearance: an unreadable setting change"),
            }
        }
        tracing::debug!("appearance: the settings portal's signals ended");
    }
}

/// Follows the portal into the app state: each scheme is debounced on the main loop by [`DEBOUNCE`] (a later one
/// supersedes it) and then lands in [`AppState::appearance_changed`].
///
/// # Errors
///
/// The following thread could not start.
pub fn follow_into(portal: Portal, poster: Poster) -> std::io::Result<()> {
    let latest = Arc::new(AtomicU64::new(0));
    portal.follow(move |scheme| debounce(&poster, &latest, scheme.is_dark()))?;
    Ok(())
}

/// Posts `dark` to land after [`DEBOUNCE`] unless a later report comes first.
fn debounce(poster: &Poster, latest: &Arc<AtomicU64>, dark: bool) {
    let Some(main_loop) = poster.upgrade() else {
        return;
    };
    let mine = latest.fetch_add(1, Ordering::AcqRel) + 1;
    let current = Arc::clone(latest);
    let task = Box::new(move |state: &mut AppState| {
        // the side rides the report, never re-read when the debounce fires
        if current.load(Ordering::Acquire) == mine {
            state.appearance_changed(dark);
        }
    });
    if main_loop.post_after(DEBOUNCE, task).is_err() {
        tracing::debug!("appearance: the report is dropped, the loop is closed");
    }
}

/// The side the terminals render and why (upstream's `lastAppliedIsDark` and the system appearance it follows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Appearance {
    /// The desktop's side as the portal last reported it.
    desktop_dark: bool,
    /// The side `debug.appearance` forced, which the desktop no longer moves.
    forced: Option<bool>,
    /// The side the last config load applied.
    last_applied_dark: bool,
    /// A flip asked for a reload the windows have not run yet.
    reload: bool,
}

impl Appearance {
    /// The app launched with the desktop on `dark` and loaded its config on that side.
    #[must_use]
    pub fn new(dark: bool) -> Self {
        Self {
            desktop_dark: dark,
            forced: None,
            last_applied_dark: dark,
            reload: false,
        }
    }

    /// The side a config load takes now: the forced one, else the desktop's.
    #[must_use]
    pub fn side(&self) -> bool {
        self.forced.unwrap_or(self.desktop_dark)
    }

    /// The side the last config load applied.
    #[must_use]
    pub fn last_applied_dark(&self) -> bool {
        self.last_applied_dark
    }

    /// Whether a flip to `dark` re-feeds the config: only while following with both slots set (a dual line exists),
    /// and only when the side moved.
    #[must_use]
    pub fn flip_reloads(&self, settings: &AppSettings, dark: bool) -> bool {
        settings.follows_system_appearance()
            && settings.theme.is_some()
            && settings.dark_theme.is_some()
            && dark != self.last_applied_dark
    }

    /// The desktop moved to `dark`; whether that asks for a reload (never while a side is forced).
    pub fn desktop_changed(&mut self, settings: &AppSettings, dark: bool) -> bool {
        self.desktop_dark = dark;
        self.forced.is_none() && self.ask_reload(settings, dark)
    }

    /// `debug.appearance` forced `dark`; whether that asks for a reload.
    pub fn force(&mut self, settings: &AppSettings, dark: bool) -> bool {
        self.forced = Some(dark);
        self.ask_reload(settings, dark)
    }

    fn ask_reload(&mut self, settings: &AppSettings, dark: bool) -> bool {
        let reloads = self.flip_reloads(settings, dark);
        self.reload |= reloads;
        reloads
    }

    /// Takes the reload a flip asked for.
    pub fn take_reload(&mut self) -> bool {
        std::mem::take(&mut self.reload)
    }

    /// A config load applied `dark`.
    pub fn applied(&mut self, dark: bool) {
        self.last_applied_dark = dark;
    }
}

/// A config reload the windows run before they render: on `dark`; `asked` when the core asked for it (a command or a
/// settings change, which takes the diagnostic count back), else an appearance flip alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigReload {
    pub dark: bool,
    pub asked: bool,
}

impl AppState {
    /// The app launched with its config loaded on `dark`, the desktop's side.
    pub fn launched_on(&mut self, dark: bool) {
        self.appearance = Appearance::new(dark);
        self.core.report_applied_appearance(dark);
    }

    /// The desktop's appearance settled on `dark` (upstream's `SettingsModel.appearanceChanged`): while following with
    /// both slots set, a new side re-feeds the config.
    pub fn appearance_changed(&mut self, dark: bool) {
        if self.is_quitting() {
            return;
        }
        if self.appearance.desktop_changed(self.core.settings(), dark) {
            tracing::info!(dark, "appearance: the desktop flipped; reloading the config");
            self.settle();
        }
    }

    /// What the windows reload before they render: the side `debug.appearance` forced is taken first (its flip runs
    /// at once, rather than waiting for a desktop report), then a command's reload or a flip's.
    pub fn take_config_reload(&mut self) -> Option<ConfigReload> {
        if let Some(dark) = self.core.take_debug_appearance() {
            self.appearance.force(self.core.settings(), dark);
        }
        let asked = self.core.take_config_reload();
        let flipped = self.appearance.take_reload();
        (asked || flipped).then(|| ConfigReload {
            dark: self.appearance.side(),
            asked,
        })
    }

    /// A config load applied `dark`: recorded, and readable through `debug.appearance`.
    pub fn config_applied(&mut self, dark: bool) {
        self.appearance.applied(dark);
        self.core.report_applied_appearance(dark);
    }

    /// The side the terminals render.
    #[must_use]
    pub fn applied_dark(&self) -> bool {
        self.appearance.last_applied_dark()
    }
}

#[cfg(test)]
mod tests;
