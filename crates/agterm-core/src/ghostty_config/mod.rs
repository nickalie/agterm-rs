//! Ghostty's config, parsed by agterm itself: what libghostty's config loader did for the GTK build, for a frontend
//! that drives libghostty-vt (which has no config) and draws the terminal itself.
//!
//! **Load order** (upstream's `GhosttyApp.loadConfig`, the GTK build's `build_config`): the built-in
//! [`DEFAULTS`] ([`defaults_text`]), then the user files [`config_files`] lists (the global ghostty config when the
//! settings inherit it, the agterm-scoped `ghostty.conf`, the Settings window's `ghostty-settings.conf`), then the
//! `config-file` includes they name ([`Loader::load_includes`]), then a per-surface overlay. [`load_app_config`] runs
//! that order; [`LoadedConfig::finalize`] then lays the theme under it (ghostty loads the theme file first and replays
//! every line on top, so any line beats the theme) and yields the typed [`GhosttyConfig`] with the diagnostics.
//!
//! **Diagnostics** (the count `config.reload` answers): every line is checked against the key table generated from
//! ghostty's `Config.zig` at `GHOSTTY_REV` ([`known_keys`], `scripts/gen-ghostty-config-keys.sh`), the value by its
//! key's type ([`validate`]), supported by agterm or not: an unknown key, a malformed value of any known key, an
//! include that cannot be read or closes a cycle each count once, a theme that cannot be found once per theme
//! directory tried, as libghostty counts them. A valid value of a key agterm does not support counts nothing; the key is ignored and logged. libghostty
//! finalizes a fresh config on the light side of a dual theme, so [`LoadedConfig::diagnostic_count`] does too.
//!
//! **Residual differences** from libghostty (the count ones pinned by `agterm-ghostty`'s `config_parity` test, which
//! checks every other case above against the linked libghostty):
//! - count: keys whose type has a `parseCLI` agterm does not port ([`validate::Kind::Opaque`]: `font-style*`,
//!   `font-variation*`, `font-codepoint-map`, `clipboard-codepoint-map`, `adjust-*`, `command`, `initial-command`,
//!   `env`, `input`, `working-directory`, `quick-terminal-size`, `background-blur`, `mouse-scroll-multiplier`,
//!   `selection-word-chars`, `macos-icon-screen-color`, `command-palette-entry`, `key-remap`) take any value: a
//!   malformed one counts 0 here, 1 in libghostty (a bare key still counts when the type needs a value). `keybind` is
//!   ported ([`binds`]) and counts as libghostty counts it (a `chain=` with nothing before it to extend counts when
//!   the line is applied, [`GhosttyConfig::apply`]);
//! - message only: an include, theme or path that cannot be read for a reason other than its absence counts once as
//!   in libghostty, its I/O error named after the closest Zig error;
//! - value only: a relative path set by a theme file stays relative (libghostty makes it absolute against the first
//!   config file replayed after the theme).

pub mod binds;
mod keys;
mod model;
mod parse;
pub mod validate;

#[cfg(test)]
mod binds_tests;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

pub use keys::GHOSTTY_REV;
pub use model::{
    BackgroundImage, BackgroundImageFit, BackgroundImagePosition, Clipboard, ClipboardAccess, Colors, Cursor,
    CursorStyle, DEFAULT_TERM, Font, GhosttyConfig, Mouse, MouseShiftCapture, PaddingBalance, PaddingColor, Scrollback,
    Selection, ShellIntegrationFeatures, WindowPadding,
};
pub use parse::{Diagnostic, Loader, Location, Setting, Step, lines};
pub use validate::{ConfigPath, KeySpec, Padding, TerminalColor, ThemeName};

use crate::paths::{self, Env};
use crate::persist::SettingsStore;

/// agterm's built-in terminal defaults, loaded before the user's own configs (upstream's
/// `agterm/Resources/ghostty-defaults.conf`, with the Linux Edit chords).
pub const DEFAULTS: &str = include_str!("ghostty-defaults.conf");

/// Every key ghostty's config knows at [`GHOSTTY_REV`], sorted by name.
#[must_use]
pub fn known_keys() -> &'static [KeySpec] {
    keys::KEYS
}

/// The built-in defaults; without ghostty's terminfo the terminal claims `xterm-256color` instead of its own.
#[must_use]
pub fn defaults_text(terminfo_resolved: bool) -> String {
    if terminfo_resolved {
        DEFAULTS.to_owned()
    } else {
        format!("{DEFAULTS}\nterm = xterm-256color\n")
    }
}

/// The user's config files the settings select, in load order (upstream's `resolveConfigInputs`, read from
/// `settings.json` on every build): the global ghostty config when `inheritGlobalGhosttyConfig` is on, the
/// agterm-scoped `ghostty.conf` of the settings' config directory, then the Settings window's `ghostty-settings.conf`.
#[must_use]
pub fn config_files(env: &dyn Env) -> Vec<PathBuf> {
    let settings = SettingsStore::new(paths::state_dir(env)).load();
    let config_dir = paths::config_dir(env, settings.config_directory.as_deref().map(Path::new));
    let global =
        paths::global_ghostty_config_path(env).filter(|_| settings.inherit_global_ghostty_config == Some(true));
    global
        .into_iter()
        .chain([
            paths::ghostty_config_path(&config_dir),
            paths::ghostty_settings_config_path(env),
        ])
        .collect()
}

/// Where ghostty looks a theme up by name, in order: the user's `<XDG config home>/ghostty/themes`, then the bundled
/// `themes` directory of the resolved resources.
#[must_use]
pub fn theme_dirs(env: &dyn Env, bundled: Option<&Path>) -> Vec<PathBuf> {
    let user = paths::global_ghostty_config_path(env)
        .as_deref()
        .and_then(Path::parent)
        .map(|ghostty| ghostty.join("themes"));
    user.into_iter().chain(bundled.map(Path::to_path_buf)).collect()
}

/// The app's config in today's load order: defaults, the user files, their includes, then `overlay` (a per-surface
/// config that wins over all of them).
#[must_use]
pub fn load_app_config(env: &dyn Env, terminfo_resolved: bool, overlay: Option<&str>) -> LoadedConfig {
    let mut loader = Loader::new(paths::home_dir(env));
    loader.load_text("ghostty-defaults.conf", &defaults_text(terminfo_resolved));
    for file in config_files(env) {
        loader.load_file(&file);
    }
    loader.load_includes();
    if let Some(overlay) = overlay.filter(|overlay| !overlay.is_empty()) {
        loader.load_text("overlay", overlay);
    }
    loader.finish()
}

/// What loading the config files left, before the theme: every line in replay order and the loading diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    steps: Vec<Step>,
    home: Option<PathBuf>,
}

/// A finalized config: the typed values and every diagnostic, in libghostty's order.
#[derive(Debug, Clone, PartialEq)]
pub struct FinalConfig {
    pub config: GhosttyConfig,
    pub diagnostics: Vec<Diagnostic>,
    /// The theme file that was loaded, if any.
    pub theme_path: Option<PathBuf>,
}

impl LoadedConfig {
    /// The lines and loading diagnostics, in replay order.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// The config on the `dark` or light side of a dual theme, the theme found in `theme_dirs` by name (or by its
    /// absolute path): the theme's lines first, then every loaded line over them (`Config.finalize`, `loadTheme`).
    #[must_use]
    pub fn finalize(&self, dark: bool, theme_dirs: &[PathBuf]) -> FinalConfig {
        let mut diagnostics = Vec::new();
        let mut theme_settings = Vec::new();
        let mut missing_theme = Vec::new();
        let mut theme_path = None;
        if let Some(theme) = self.theme() {
            let name = expand_home(theme.side(dark), self.home.as_deref());
            match parse::open_theme(&name, theme_dirs) {
                Ok((path, text)) => {
                    theme_settings = parse::theme_settings(&path, &text);
                    theme_path = Some(path);
                }
                Err(theme_diagnostics) => missing_theme = theme_diagnostics,
            }
        }
        let mut config = GhosttyConfig::default();
        for setting in &theme_settings {
            diagnostics.extend(setting.diagnostic());
            diagnostics.extend(config.apply(setting));
        }
        for step in &self.steps {
            match step {
                Step::Setting(setting) => {
                    diagnostics.extend(setting.diagnostic());
                    diagnostics.extend(config.apply(setting));
                }
                Step::Diagnostic(diagnostic) => diagnostics.push(diagnostic.clone()),
            }
        }
        diagnostics.extend(missing_theme);
        config.finalize();
        FinalConfig {
            config,
            diagnostics,
            theme_path,
        }
    }

    /// The diagnostic count libghostty reports for this config: finalized on the light side, as a fresh config is.
    #[must_use]
    pub fn diagnostic_count(&self, theme_dirs: &[PathBuf]) -> usize {
        self.finalize(false, theme_dirs).diagnostics.len()
    }

    /// The theme the lines leave set.
    fn theme(&self) -> Option<ThemeName> {
        let mut config = GhosttyConfig::default();
        for step in &self.steps {
            if let Step::Setting(setting) = step
                && setting.key == "theme"
            {
                config.apply(setting);
            }
        }
        config.theme
    }
}

/// `Theme.finalize`: a leading `~/` expanded against the home.
fn expand_home(name: &str, home: Option<&Path>) -> String {
    match (name.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest).to_string_lossy().into_owned(),
        _ => name.to_owned(),
    }
}
