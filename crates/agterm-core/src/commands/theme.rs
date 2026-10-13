//! `theme.set` and `theme.list` (upstream's `ControlServer+AppCommands.setTheme`/`listThemes`): the theme slots are
//! app-global settings, so neither takes a window. A change persists `settings.json`, rewrites the ghostty settings
//! config and, when its text changed, asks the frontend for a config reload that clears every session's font zoom
//! (upstream's `SettingsModel.persistAndApply` → `apply` → `reloadConfigClearingSessionZoom`). Interactive previews
//! (the palette's live theme picker) are not reachable over the socket.
//!
//! The result carries the slots by sync state (`theme-slots-by-sync`): `theme` for the single theme while not
//! following, `light` and `dark` while following, `sync` always.

use std::path::PathBuf;

use agterm_proto::command::ThemeCmd;
use agterm_proto::{ControlResult, Request};

use super::{Outcome, args_of};
use crate::Core;
use crate::error::ControlError;
use crate::persist::AppSettings;
use crate::theme::{self, ThemeCatalog};

/// Where the theme commands find the bundled themes and write the settings' ghostty config.
#[derive(Debug, Default)]
pub(crate) struct ThemeRuntime {
    /// The bundled themes' directory; `None` lists no theme.
    themes_dir: Option<PathBuf>,
    /// `ghostty-settings.conf`; `None` keeps the generated text in memory only (tests).
    settings_conf: Option<PathBuf>,
    /// The text last generated, compared against when there is no file.
    written: Option<String>,
    /// A settings change rewrote the config: the frontend reloads it into every terminal, zoom cleared.
    reload_requested: bool,
}

impl Core {
    /// Tells the core where the bundled themes are and where the settings' ghostty config goes
    /// (`<state dir>/ghostty-settings.conf`).
    pub fn set_theme_paths(&mut self, themes_dir: Option<PathBuf>, settings_conf: Option<PathBuf>) {
        self.themes.themes_dir = themes_dir;
        self.themes.settings_conf = settings_conf;
    }

    /// The bundled themes, read from their directory now (upstream reads the bundle on every call).
    #[must_use]
    pub fn theme_catalog(&self) -> ThemeCatalog {
        self.themes
            .themes_dir
            .as_deref()
            .map(ThemeCatalog::in_directory)
            .unwrap_or_default()
    }

    /// Writes the settings' ghostty config when its text changed (upstream's `writeGhosttyConfig`), whether it did.
    /// The launch calls it before libghostty loads its config, so the seeded default theme applies from the first
    /// terminal on; a failed write is logged and counts as unchanged.
    pub fn write_settings_config(&mut self) -> bool {
        let text = theme::settings_config_text(&self.settings().ghostty_config_lines());
        let changed = match self.themes.settings_conf.as_deref() {
            Some(path) => theme::write_settings_config(path, &text).unwrap_or_else(|err| {
                tracing::warn!(%err, path = %path.display(), "could not write the ghostty settings config");
                false
            }),
            None => self.themes.written.as_deref() != Some(text.as_str()),
        };
        self.themes.written = Some(text);
        changed
    }

    /// Whether a settings change asked for the config to be reloaded into every terminal since the last take; the
    /// session font sizes were already cleared then, as the reload drops the zoom.
    pub fn take_config_reload(&mut self) -> bool {
        std::mem::take(&mut self.themes.reload_requested)
    }

    /// Applies changed settings to the running app (upstream's `SettingsModel.apply`): a changed config clears every
    /// session's font size first, then asks for the reload. Commit afterwards.
    pub(crate) fn apply_settings(&mut self) {
        if !self.write_settings_config() {
            return;
        }
        self.reset_session_font_sizes();
        self.themes.reload_requested = true;
    }
}

/// `theme.set` and `theme.list`.
pub(crate) fn dispatch(core: &mut Core, cmd: ThemeCmd, request: &Request) -> Outcome {
    match cmd {
        ThemeCmd::Set => set(core, request),
        ThemeCmd::List => Ok(ControlResult {
            themes: Some(core.theme_catalog().names().to_vec()),
            ..slots(core.settings())
        }),
    }
}

/// The slots as the result reports them.
fn slots(settings: &AppSettings) -> ControlResult {
    ControlResult {
        theme: settings.current_theme().map(str::to_owned),
        sync: Some(settings.follows_system_appearance()),
        light: settings.current_light_theme().map(str::to_owned),
        dark: settings.current_dark_theme().map(str::to_owned),
        ..ControlResult::default()
    }
}

/// `theme.set [NAME] [--light L] [--dark D|none]`: NAME (or `--light`) sets the light/single slot keeping a dark one,
/// `--dark` sets the dark slot and turns following on, `--dark none` (any case) clears it and turns following off, and
/// no name at all returns to ghostty's built-in colors. Every named theme must be bundled, matched exactly.
fn set(core: &mut Core, request: &Request) -> Outcome {
    let args = args_of(request);
    let name = ThemeCatalog::resolved_name(args.name.as_deref());
    let light = ThemeCatalog::resolved_name(args.light.as_deref());
    let dark = ThemeCatalog::resolved_name(args.dark.as_deref());
    if name.is_some() && light.is_some() {
        return Err(ControlError::rejected(
            "theme.set takes either a name or --light, not both",
        ));
    }
    let light_slot = name.or(light);
    let clear_dark = dark.as_deref().is_some_and(|dark| dark.eq_ignore_ascii_case("none"));
    let catalog = core.theme_catalog();
    let named = [light_slot.as_deref(), dark.as_deref().filter(|_| !clear_dark)];
    if let Some(unknown) = named.into_iter().flatten().find(|theme| !catalog.contains(theme)) {
        return Err(ControlError::rejected(format!("unknown theme: {unknown}")));
    }
    core.update_settings(|settings| apply_slots(settings, light_slot, dark, clear_dark));
    core.apply_settings();
    Ok(slots(core.settings()))
}

/// The slot change `theme.set` makes, in upstream's branch order.
fn apply_slots(settings: &mut AppSettings, light: Option<String>, dark: Option<String>, clear_dark: bool) {
    match dark {
        _ if clear_dark => {
            settings.set_dark_theme(None);
            if light.is_some() {
                settings.set_light_theme(light);
            }
        }
        Some(dark) => match light {
            Some(light) => settings.set_system_themes(light, dark),
            None => settings.set_dark_theme(Some(dark)),
        },
        // no name: back to ghostty's built-in
        None => settings.set_light_theme(light),
    }
}
