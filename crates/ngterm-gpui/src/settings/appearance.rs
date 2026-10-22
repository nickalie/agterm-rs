//! Appearance: Terminal (font, size, cursor, the theme pickers) and Window (toolbar, translucency, the sidebar's tint
//! and text size, the palette's text size, the inactive-pane mute) (upstream's `AppearanceSettingsView`).

use ngterm_core::persist::settings::{CursorStyle, ToolbarMode};
use ngterm_core::settings_model::{
    AppearanceChange, DEFAULT_FONT_SIZE, FONT_SIZE_RANGE, INTERFACE_FONT_SIZE_RANGE, SIDEBAR_FONT_SIZE_RANGE,
    SettingsChange,
};

use super::form::{Control, Item, Picker, Section, SliderKey, picker, stepper, toggle};
use super::{Catalogs, Snapshot};

fn change(change: AppearanceChange) -> SettingsChange {
    SettingsChange::Appearance(change)
}

/// The page's sections.
#[must_use]
pub fn sections(snapshot: &Snapshot, catalogs: &Catalogs) -> Vec<Section> {
    vec![
        Section::new("Terminal", terminal(snapshot, catalogs)),
        Section::new("Window", window(snapshot)),
    ]
}

fn terminal(snapshot: &Snapshot, catalogs: &Catalogs) -> Vec<Item> {
    let settings = &snapshot.settings;
    let mut items = vec![
        Item::new(
            "settings-font-family",
            "Font name",
            Control::Picker(Picker::new(
                font_options(&catalogs.fonts, settings.font_family.as_deref()),
                &settings.font_family,
                |family| change(AppearanceChange::FontFamily(family)),
            )),
        ),
        stepper(
            "settings-font-size",
            "Font size",
            settings.font_size.unwrap_or(DEFAULT_FONT_SIZE),
            FONT_SIZE_RANGE,
            |size| change(AppearanceChange::FontSize(size)),
        ),
        // "Default" is a state, not a synonym for Block: it emits no `cursor-style`, so the config chain picks the shape
        picker(
            "settings-cursor-style",
            "Cursor shape",
            &[
                ("Default", None),
                ("Block", Some(CursorStyle::Block)),
                ("Bar", Some(CursorStyle::Bar)),
                ("Underline", Some(CursorStyle::Underline)),
            ],
            &settings.effective_cursor_style(),
            |style| change(AppearanceChange::CursorStyle(style)),
        ),
        // three states, not a toggle: only Default leaves DEC mode 12 able to drive the blink
        picker(
            "settings-cursor-blink",
            "Cursor blink",
            &[("Default", None), ("Blink", Some(true)), ("Steady", Some(false))],
            &settings.cursor_blink,
            |blink| change(AppearanceChange::CursorBlink(blink)),
        ),
    ];
    items.extend(themes(snapshot, catalogs));
    items
}

/// The font picker's rows: Default, the monospaced families, and a configured family the system does not list.
#[must_use]
pub fn font_options(fonts: &[String], configured: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut options = vec![("Default".to_owned(), None)];
    options.extend(fonts.iter().map(|font| (font.clone(), Some(font.clone()))));
    if let Some(configured) = configured.filter(|family| !fonts.iter().any(|font| font == family)) {
        options.push((configured.to_owned(), Some(configured.to_owned())));
    }
    options
}

/// The theme pickers (upstream's two `Picker`s and the follow toggle): the first is the theme on screen, with ghostty's
/// built-in offered only while not following; the second, while following, the other appearance's theme.
fn themes(snapshot: &Snapshot, catalogs: &Catalogs) -> Vec<Item> {
    let settings = &snapshot.settings;
    let dark = snapshot.dark;
    let following = settings.follows_system_appearance();
    let active = settings.active_theme(dark).map(str::to_owned);
    let mut items = vec![
        Item::new(
            "settings-theme",
            "Theme",
            Control::Picker(Picker::new(
                theme_options(&catalogs.themes, !following, active.as_deref()),
                &active,
                move |theme| change(AppearanceChange::ThemeForCurrentAppearance { theme, dark }),
            )),
        ),
        toggle(
            "settings-follow-appearance",
            "Follow system appearance",
            following,
            move |on| change(AppearanceChange::FollowSystemAppearance { on, dark }),
        ),
    ];
    if following {
        let other = settings.alternate_theme(dark).map(str::to_owned);
        let (title, side) = if dark {
            ("Light theme", "light")
        } else {
            ("Dark theme", "dark")
        };
        items.push(Item::new(
            "settings-theme-dark",
            title,
            Control::Picker(Picker::new(
                theme_options(&catalogs.themes, false, other.as_deref()),
                &other,
                move |theme| change(AppearanceChange::AlternateTheme { theme, dark }),
            )),
        ));
        items.push(Item::hint(
            "settings-theme-dark-hint",
            format!("Used when the desktop is in {side} mode."),
        ));
    }
    items
}

/// A theme picker's rows: ghostty's built-in when `builtin`, the bundled themes, and a set theme not among them.
#[must_use]
pub fn theme_options(themes: &[String], builtin: bool, current: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut options = Vec::new();
    if builtin {
        options.push(("default ghostty".to_owned(), None));
    }
    options.extend(themes.iter().map(|name| (name.clone(), Some(name.clone()))));
    if let Some(current) = current.filter(|name| !themes.iter().any(|theme| theme == name)) {
        options.push((current.to_owned(), Some(current.to_owned())));
    }
    options
}

fn window(snapshot: &Snapshot) -> Vec<Item> {
    let settings = &snapshot.settings;
    let translucent = settings.background_opacity.unwrap_or(1.0) < 1.0;
    vec![
        picker(
            "settings-toolbar-mode",
            "Toolbar",
            &[
                ("Normal", ToolbarMode::Normal),
                ("Compact", ToolbarMode::Compact),
                ("Hidden", ToolbarMode::Hidden),
            ],
            &settings.effective_toolbar_mode(),
            |mode| change(AppearanceChange::ToolbarMode(mode)),
        ),
        SliderKey::BackgroundOpacity.item("Background Opacity", settings),
        SliderKey::BackgroundBlur
            .item("Background Blur", settings)
            .enabled(translucent),
        Item::hint("settings-bg-blur-hint", "Blur needs opacity below 100%."),
        SliderKey::SidebarShift.item("Sidebar Tint", settings),
        stepper(
            "settings-sidebar-font-size",
            "Sidebar font size",
            settings.effective_sidebar_font_size(),
            SIDEBAR_FONT_SIZE_RANGE,
            |size| change(AppearanceChange::SidebarFontSize(size)),
        ),
        stepper(
            "settings-interface-font-size",
            "Palette and switcher font size",
            settings.effective_interface_font_size(),
            INTERFACE_FONT_SIZE_RANGE,
            |size| change(AppearanceChange::InterfaceFontSize(size)),
        ),
        SliderKey::InactivePaneMute.item("Inactive pane and backdrop mute", settings),
    ]
}
