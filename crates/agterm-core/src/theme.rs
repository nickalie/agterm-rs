//! Host-free theme facts: the catalog of bundled themes (upstream's `ThemeCatalog.swift`), the `light:…,dark:…`
//! parser (`ThemeName.swift`), the theme slots of the settings with their setters (`AppSettings.activeTheme`,
//! `SettingsModel.setLightTheme`/`setDarkTheme`/`setSystemThemes`) and the ghostty config lines the settings emit
//! (`AppSettings.ghosttyConfigLines`), written to `ghostty-settings.conf` (`SettingsModel.writeGhosttyConfig`).
//!
//! The theme slots (`theme-picker.md`): `theme` is the single theme, or the light slot while following the system
//! appearance; `dark_theme` is the dark slot; `follow_system_appearance` turns following on. Following emits ghostty's
//! own dual `theme = light:NAME,dark:NAME` line and libghostty resolves the side from the color scheme it is told.

use std::cmp::Ordering;
use std::io;
use std::path::Path;

use crate::persist::AppSettings;

/// The catalog entry for ghostty's built-in colors (no `theme` line).
pub const DEFAULT_TITLE: &str = "default ghostty";
/// The stable id of [`DEFAULT_TITLE`]'s entry.
pub const DEFAULT_ID: &str = "theme:__default__";
/// The light side seeded when a dark theme is chosen with no theme set: ghostty's built-in cannot be a dual side, so a
/// bundled theme stands in (upstream's `SettingsModel.defaultLightTheme`).
pub const DEFAULT_LIGHT_THEME: &str = "Builtin Light";

/// The bundled theme names, in the order the picker and `theme.list` show them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemeCatalog {
    names: Vec<String>,
}

/// One picker row: the built-in default (`name` `None`) or a bundled theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeEntry {
    pub id: String,
    pub name: Option<String>,
    pub title: String,
}

impl ThemeEntry {
    /// Whether this row is ghostty's built-in colors.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.name.is_none()
    }
}

impl ThemeCatalog {
    /// A catalog of `names`, sorted case-insensitively.
    #[must_use]
    pub fn new(names: Vec<String>) -> Self {
        let mut names = names;
        names.sort_by(|a, b| compare_names(a, b));
        Self { names }
    }

    /// The catalog of the theme files in `directory` (one file per theme); empty when it is absent or empty. Hidden
    /// entries are skipped: the staged directory carries the build's resource manifest as a dotfile, which is no theme.
    #[must_use]
    pub fn in_directory(directory: &Path) -> Self {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Self::default();
        };
        let names = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.'))
            .collect();
        Self::new(names)
    }

    /// The theme names, sorted.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// The picker rows: the built-in default first, then every theme.
    #[must_use]
    pub fn entries(&self) -> Vec<ThemeEntry> {
        std::iter::once(ThemeEntry {
            id: DEFAULT_ID.to_owned(),
            name: None,
            title: DEFAULT_TITLE.to_owned(),
        })
        .chain(self.names.iter().map(|name| ThemeEntry {
            id: Self::id(Some(name)),
            name: Some(name.clone()),
            title: name.clone(),
        }))
        .collect()
    }

    /// The row id of theme `name`, the default's for `None`.
    #[must_use]
    pub fn id(name: Option<&str>) -> String {
        name.map_or_else(|| DEFAULT_ID.to_owned(), |name| format!("theme:{name}"))
    }

    /// A theme argument as the catalog matches it: `None`, empty or blank selects ghostty's built-in default,
    /// anything else is trimmed of spaces.
    #[must_use]
    pub fn resolved_name(name: Option<&str>) -> Option<String> {
        name.map(trim_spaces).filter(|name| !name.is_empty()).map(str::to_owned)
    }

    /// Whether `name` is a bundled theme, matched exactly (case included).
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.names.iter().any(|known| known == name)
    }
}

/// `s` without leading and trailing spaces and tabs (Foundation's `.whitespaces`).
fn trim_spaces(s: &str) -> &str {
    s.trim_matches([' ', '\t'])
}

/// Case-insensitive order, ties broken by the exact spelling so the sort is total.
fn compare_names(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b))
}

/// The theme name `value` (a `theme` config value) resolves to under the appearance (upstream's `ThemeName.resolved`):
/// a well-formed `light:…,dark:…` (both sides named, prefixes in any case and order) picks its side; anything else is
/// returned trimmed.
#[must_use]
pub fn resolved_for_appearance(value: &str, is_dark: bool) -> String {
    let trimmed = trim_spaces(value);
    let mut light = None;
    let mut dark = None;
    for part in trimmed.split(',').map(trim_spaces) {
        if let Some(name) = variant("light:", part) {
            light = Some(name);
        } else if let Some(name) = variant("dark:", part) {
            dark = Some(name);
        }
    }
    match (light, dark) {
        (Some(light), Some(dark)) => if is_dark { dark } else { light }.to_owned(),
        _ => trimmed.to_owned(),
    }
}

/// The trimmed name after a case-insensitive `prefix`; `None` when absent or empty.
fn variant<'a>(prefix: &str, part: &'a str) -> Option<&'a str> {
    let head = part.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let name = trim_spaces(&part[prefix.len()..]);
    (!name.is_empty()).then_some(name)
}

impl AppSettings {
    /// Whether the theme follows the system appearance (`theme.list`'s `sync`).
    #[must_use]
    pub fn follows_system_appearance(&self) -> bool {
        self.follow_system_appearance == Some(true)
    }

    /// The single theme `theme.set`/`theme.list` report: `None` for ghostty's built-in and while following, when the
    /// slots ride `light`/`dark` instead.
    #[must_use]
    pub fn current_theme(&self) -> Option<&str> {
        (!self.follows_system_appearance())
            .then_some(self.theme.as_deref())
            .flatten()
    }

    /// The light slot, reported only while following.
    #[must_use]
    pub fn current_light_theme(&self) -> Option<&str> {
        self.follows_system_appearance()
            .then_some(self.theme.as_deref())
            .flatten()
    }

    /// The dark slot, reported only while following.
    #[must_use]
    pub fn current_dark_theme(&self) -> Option<&str> {
        self.follows_system_appearance()
            .then_some(self.dark_theme.as_deref())
            .flatten()
    }

    /// The theme on screen under the appearance (the picker's badge, upstream's `activeTheme(isDark:)`).
    #[must_use]
    pub fn active_theme(&self, is_dark: bool) -> Option<&str> {
        if !self.follows_system_appearance() {
            return self.theme.as_deref();
        }
        if is_dark {
            self.dark_theme.as_deref().or(self.theme.as_deref())
        } else {
            self.theme.as_deref()
        }
    }

    /// Sets the light/single slot (`theme.set NAME`): a name keeps the dark slot and following; `None` (ghostty's
    /// built-in) clears everything, as a dual line cannot have a built-in side.
    pub fn set_light_theme(&mut self, name: Option<String>) {
        if name.is_none() {
            self.dark_theme = None;
            self.follow_system_appearance = None;
        }
        self.theme = name;
    }

    /// Sets the dark slot (`theme.set --dark`): a name turns following on, the light side seeded with
    /// [`DEFAULT_LIGHT_THEME`] when no theme is set; `None` (`--dark none`) turns following off.
    pub fn set_dark_theme(&mut self, name: Option<String>) {
        if let Some(name) = name {
            if self.theme.as_deref().is_none_or(str::is_empty) {
                self.theme = Some(DEFAULT_LIGHT_THEME.to_owned());
            }
            self.dark_theme = Some(name);
            self.follow_system_appearance = Some(true);
        } else {
            self.dark_theme = None;
            self.follow_system_appearance = None;
        }
    }

    /// Sets both slots at once, following on (`theme.set --light --dark`).
    pub fn set_system_themes(&mut self, light: String, dark: String) {
        self.theme = Some(light);
        self.dark_theme = Some(dark);
        self.follow_system_appearance = Some(true);
    }

    /// The `key = value` lines the settings emit into `ghostty-settings.conf` (upstream's `ghosttyConfigLines`), loaded
    /// after the user's `ghostty.conf` so they win. Values are written raw: ghostty takes the rest of the line, so
    /// names with spaces are not quoted.
    ///
    /// This build's GTK window has no translucency of its own, so the lines never carry upstream's translucency pins
    /// (see [`Self::ghostty_config_lines_for`]).
    #[must_use]
    pub fn ghostty_config_lines(&self) -> Vec<String> {
        self.ghostty_config_lines_for(false)
    }

    /// The config lines for a host that composites window translucency itself (`translucency`) or not. Upstream's
    /// `AppKit` window draws the translucent tint, so a translucent opacity pins the renderer fully transparent; a
    /// host without window translucency leaves the pins out, or its terminals would show the window background
    /// through.
    #[must_use]
    pub fn ghostty_config_lines_for(&self, translucency: bool) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(family) = self.font_family.as_deref().filter(|family| !family.is_empty()) {
            lines.push(format!("font-family = {family}"));
        }
        if let Some(size) = self.font_size {
            lines.push(format!("font-size = {}", format_number(size)));
        }
        let light = self.theme.as_deref().filter(|name| !name.is_empty());
        let dark = self.dark_theme.as_deref().filter(|name| !name.is_empty());
        match (light, dark) {
            (Some(light), Some(dark)) if self.follows_system_appearance() => {
                lines.push(format!("theme = light:{light},dark:{dark}"));
            }
            (light, dark) => {
                if let Some(single) = light.or(dark) {
                    lines.push(format!("theme = {single}"));
                }
            }
        }
        if let Some(style) = self.effective_cursor_style() {
            lines.push(format!("cursor-style = {}", style.raw()));
        }
        if let Some(blink) = self.cursor_blink {
            lines.push(format!("cursor-style-blink = {blink}"));
        }
        if translucency && self.background_opacity.is_some_and(|opacity| opacity < 1.0) {
            lines.push("background-opacity = 0".to_owned());
            lines.push("background-blur = 0".to_owned());
        }
        lines.push(format!(
            "mouse-scroll-multiplier = {}",
            format_number(self.mouse_scroll_multiplier.unwrap_or(3.0))
        ));
        let right_click = if self.right_click_paste.unwrap_or(true) {
            "paste"
        } else {
            "ignore"
        };
        lines.push(format!("right-click-action = {right_click}"));
        lines
    }
}

/// A whole number without a trailing `.0` (`14`), any other as Swift prints it (`13.5`).
fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// The text of `ghostty-settings.conf` for `lines`: one per line, newline-terminated.
#[must_use]
pub fn settings_config_text(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Writes `text` to `path` unless it already holds exactly that (upstream's `writeGhosttyConfig`); whether it wrote.
/// The directory is created first.
///
/// # Errors
///
/// The directory could not be created or the file written.
pub fn write_settings_config(path: &Path, text: &str) -> io::Result<bool> {
    if std::fs::read_to_string(path).is_ok_and(|existing| existing == text) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::persist::json::write_atomic(path, text.as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn names_in_a_directory_sort_case_insensitively_without_hidden_entries() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        for name in ["Nord", "alabaster", "Zenburn", ".agterm-resource-manifest"] {
            std::fs::write(dir.path().join(name), "")?;
        }
        assert_eq!(
            ThemeCatalog::in_directory(dir.path()).names(),
            names(&["alabaster", "Nord", "Zenburn"])
        );
        assert!(ThemeCatalog::in_directory(Path::new("/no/such/dir")).names().is_empty());
        Ok(())
    }

    #[test]
    fn entries_put_the_ghostty_default_before_named_themes() {
        let catalog = ThemeCatalog::new(names(&["Nord", "agterm", "Adwaita Dark"]));
        assert_eq!(catalog.names(), names(&["Adwaita Dark", "agterm", "Nord"]));
        let entries = catalog.entries();
        assert_eq!(
            entries[0],
            ThemeEntry {
                id: "theme:__default__".to_owned(),
                name: None,
                title: "default ghostty".to_owned()
            }
        );
        assert!(entries[0].is_default());
        assert_eq!(
            entries[2],
            ThemeEntry {
                id: "theme:agterm".to_owned(),
                name: Some("agterm".to_owned()),
                title: "agterm".to_owned()
            }
        );
        assert_eq!(entries.len(), 4);
    }

    #[test]
    fn ids_name_the_default_and_the_themes() {
        assert_eq!(ThemeCatalog::id(None), "theme:__default__");
        assert_eq!(ThemeCatalog::id(Some("agterm")), "theme:agterm");
    }

    #[test]
    fn a_blank_name_is_the_default_and_a_name_is_trimmed() {
        assert_eq!(ThemeCatalog::resolved_name(None), None);
        assert_eq!(ThemeCatalog::resolved_name(Some("")), None);
        assert_eq!(ThemeCatalog::resolved_name(Some("   ")), None);
        assert_eq!(ThemeCatalog::resolved_name(Some("  Nord  ")).as_deref(), Some("Nord"));
    }

    #[test]
    fn names_match_exactly() {
        let catalog = ThemeCatalog::new(names(&["agterm", "Nord"]));
        assert!(catalog.contains("agterm"));
        assert!(catalog.contains("Nord"));
        assert!(!catalog.contains("nord"));
    }

    #[test]
    fn a_plain_name_resolves_to_itself_trimmed() {
        assert_eq!(resolved_for_appearance("Gruvbox Dark Hard", true), "Gruvbox Dark Hard");
        assert_eq!(resolved_for_appearance("Gruvbox Dark Hard", false), "Gruvbox Dark Hard");
        assert_eq!(resolved_for_appearance("  Alabaster  ", false), "Alabaster");
    }

    #[test]
    fn the_dual_form_picks_the_side_in_any_order_and_case() {
        for value in [
            "light:Alabaster,dark:Gruvbox Dark Hard",
            "dark:Gruvbox Dark Hard,light:Alabaster",
            "LIGHT:Alabaster,DARK:Gruvbox Dark Hard",
            "light: Alabaster , dark: Gruvbox Dark Hard ",
        ] {
            assert_eq!(resolved_for_appearance(value, false), "Alabaster", "{value}");
            assert_eq!(resolved_for_appearance(value, true), "Gruvbox Dark Hard", "{value}");
        }
    }

    #[test]
    fn a_one_sided_or_empty_variant_is_not_a_dual_form() {
        assert_eq!(resolved_for_appearance("light:Alabaster", true), "light:Alabaster");
        assert_eq!(resolved_for_appearance("dark:Nord", false), "dark:Nord");
        assert_eq!(resolved_for_appearance("light:,dark:Nord", false), "light:,dark:Nord");
        assert_eq!(
            resolved_for_appearance("light:Alabaster,dark:", true),
            "light:Alabaster,dark:"
        );
    }

    fn settings(theme: Option<&str>, dark: Option<&str>, follow: Option<bool>) -> AppSettings {
        AppSettings {
            theme: theme.map(str::to_owned),
            dark_theme: dark.map(str::to_owned),
            follow_system_appearance: follow,
            ..AppSettings::default()
        }
    }

    #[test]
    fn the_default_settings_emit_only_the_always_on_lines() {
        assert_eq!(
            AppSettings::default().ghostty_config_lines(),
            ["mouse-scroll-multiplier = 3", "right-click-action = paste"]
        );
    }

    #[test]
    fn fonts_and_a_single_theme_emit_raw_values() {
        let lines = AppSettings {
            font_family: Some("SF Mono".to_owned()),
            font_size: Some(14.0),
            theme: Some("3024 Night".to_owned()),
            ..AppSettings::default()
        }
        .ghostty_config_lines();
        assert!(lines.contains(&"font-family = SF Mono".to_owned()));
        assert!(lines.contains(&"font-size = 14".to_owned()));
        assert!(lines.contains(&"theme = 3024 Night".to_owned()));
        assert_eq!(
            AppSettings {
                font_size: Some(13.5),
                ..AppSettings::default()
            }
            .ghostty_config_lines(),
            [
                "font-size = 13.5",
                "mouse-scroll-multiplier = 3",
                "right-click-action = paste"
            ]
        );
    }

    #[test]
    fn following_emits_the_dual_line_and_otherwise_the_single_theme() {
        let following = settings(Some("Builtin Light"), Some("Nord"), Some(true));
        assert!(
            following
                .ghostty_config_lines()
                .contains(&"theme = light:Builtin Light,dark:Nord".to_owned())
        );
        for single in [
            settings(Some("Alabaster"), Some("Nord"), Some(false)),
            settings(Some("Alabaster"), None, Some(true)),
        ] {
            let lines = single.ghostty_config_lines();
            assert!(lines.contains(&"theme = Alabaster".to_owned()), "{lines:?}");
            assert!(!lines.iter().any(|line| line.starts_with("theme = light:")));
        }
    }

    #[test]
    fn translucency_pins_the_renderer_only_where_the_host_composites_it() {
        let translucent = AppSettings {
            background_opacity: Some(0.63),
            ..AppSettings::default()
        };
        let lines = translucent.ghostty_config_lines_for(true);
        assert!(lines.contains(&"background-opacity = 0".to_owned()));
        assert!(lines.contains(&"background-blur = 0".to_owned()));
        let this_host = translucent.ghostty_config_lines();
        assert_eq!(this_host, translucent.ghostty_config_lines_for(false));
        assert!(
            !this_host.iter().any(|line| line.starts_with("background-")),
            "{this_host:?}"
        );
        for opaque in [Some(1.0), None] {
            let settings = AppSettings {
                background_opacity: opaque,
                background_blur: Some(40),
                ..AppSettings::default()
            };
            assert!(
                !settings
                    .ghostty_config_lines_for(true)
                    .contains(&"background-blur = 0".to_owned())
            );
        }
    }

    #[test]
    fn scroll_and_right_click_are_always_emitted() {
        let lines = AppSettings {
            mouse_scroll_multiplier: Some(1.5),
            right_click_paste: Some(false),
            cursor_blink: Some(true),
            ..AppSettings::default()
        }
        .ghostty_config_lines();
        assert_eq!(
            lines,
            [
                "cursor-style-blink = true",
                "mouse-scroll-multiplier = 1.5",
                "right-click-action = ignore"
            ]
        );
    }

    #[test]
    fn the_active_theme_tracks_the_appearance_while_following() {
        let synced = settings(Some("Builtin Light"), Some("Nord"), Some(true));
        assert_eq!(synced.active_theme(true), Some("Nord"));
        assert_eq!(synced.active_theme(false), Some("Builtin Light"));
        let single = settings(Some("agterm"), None, None);
        assert_eq!(single.active_theme(true), Some("agterm"));
        assert_eq!(AppSettings::default().active_theme(true), None);
    }

    #[test]
    fn the_reported_slots_split_by_sync() {
        let single = settings(Some("Dracula"), Some("Nord"), None);
        assert_eq!(
            (
                single.current_theme(),
                single.current_light_theme(),
                single.current_dark_theme()
            ),
            (Some("Dracula"), None, None)
        );
        let synced = settings(Some("Dracula"), Some("Nord"), Some(true));
        assert_eq!(
            (
                synced.current_theme(),
                synced.current_light_theme(),
                synced.current_dark_theme()
            ),
            (None, Some("Dracula"), Some("Nord"))
        );
    }

    #[test]
    fn the_slot_setters_follow_upstreams_rules() {
        let mut slots = settings(Some("Dracula"), Some("Nord"), Some(true));
        slots.set_light_theme(Some("Alabaster".to_owned()));
        assert_eq!(slots, settings(Some("Alabaster"), Some("Nord"), Some(true)));
        slots.set_light_theme(None);
        assert_eq!(slots, AppSettings::default());
        slots.set_dark_theme(Some("Nord".to_owned()));
        assert_eq!(slots, settings(Some(DEFAULT_LIGHT_THEME), Some("Nord"), Some(true)));
        slots.set_dark_theme(None);
        assert_eq!(slots, settings(Some(DEFAULT_LIGHT_THEME), None, None));
        slots.set_system_themes("Dracula".to_owned(), "Nord".to_owned());
        assert_eq!(slots, settings(Some("Dracula"), Some("Nord"), Some(true)));
    }

    #[test]
    fn the_config_file_is_rewritten_only_when_its_text_changes() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("nested/ghostty-settings.conf");
        let text = settings_config_text(&AppSettings::default().ghostty_config_lines());
        assert_eq!(text, "mouse-scroll-multiplier = 3\nright-click-action = paste\n");
        assert!(write_settings_config(&path, &text)?);
        assert!(!write_settings_config(&path, &text)?);
        assert_eq!(std::fs::read_to_string(&path)?, text);
        assert!(write_settings_config(&path, "theme = Nord\n")?);
        Ok(())
    }
}
