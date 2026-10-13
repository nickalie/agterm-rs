//! Appearance: Terminal (font, size, cursor, the theme pickers) and Window (toolbar, translucency, the sidebar's tint
//! and text size, the palette's text size, the inactive-pane mute) (upstream's `AppearanceSettingsView`).

use std::rc::Rc;

use adw::prelude::*;
use agterm_core::persist::settings::{CursorStyle, ToolbarMode};
use agterm_core::settings_model::{
    AppearanceChange, FONT_SIZE_RANGE, INTERFACE_FONT_SIZE_RANGE, SIDEBAR_FONT_SIZE_RANGE, SettingsChange,
    sidebar_shift_label,
};

use super::rows::{self, Slider};
use super::{Catalogs, Rows, Snapshot};

fn change(change: AppearanceChange) -> SettingsChange {
    SettingsChange::Appearance(change)
}

pub fn page(rows: &mut Rows, catalogs: &Catalogs) -> adw::PreferencesPage {
    let page = rows::page("Appearance", "appearance", "applications-graphics-symbolic");
    page.add(&terminal(rows, catalogs));
    page.add(&window(rows));
    page
}

fn terminal(rows: &mut Rows, catalogs: &Catalogs) -> adw::PreferencesGroup {
    let group = rows::group("Terminal");
    let fonts = catalogs.fonts.clone();
    group.add(&rows::choice(
        rows,
        "settings-font-family",
        "Font name",
        move |snapshot| font_options(&fonts, snapshot.settings.font_family.as_deref()),
        |snapshot| snapshot.settings.font_family.clone(),
        |family| change(AppearanceChange::FontFamily(family)),
    ));
    group.add(&rows::stepper(
        rows,
        "settings-font-size",
        "Font size",
        &FONT_SIZE_RANGE,
        |snapshot| {
            snapshot
                .settings
                .font_size
                .unwrap_or(agterm_core::settings_model::DEFAULT_FONT_SIZE)
        },
        |size| change(AppearanceChange::FontSize(size)),
    ));
    // "Default" is a state, not a synonym for Block: it emits no `cursor-style`, so the config chain picks the shape
    group.add(&rows::picker(
        rows,
        "settings-cursor-style",
        "Cursor shape",
        &[
            ("Default", None),
            ("Block", Some(CursorStyle::Block)),
            ("Bar", Some(CursorStyle::Bar)),
            ("Underline", Some(CursorStyle::Underline)),
        ],
        |snapshot| snapshot.settings.effective_cursor_style(),
        |style| change(AppearanceChange::CursorStyle(style)),
    ));
    // three states, not a toggle: only Default leaves DEC mode 12 able to drive the blink
    group.add(&rows::picker(
        rows,
        "settings-cursor-blink",
        "Cursor blink",
        &[("Default", None), ("Blink", Some(true)), ("Steady", Some(false))],
        |snapshot| snapshot.settings.cursor_blink,
        |blink| change(AppearanceChange::CursorBlink(blink)),
    ));
    add_themes(rows, &group, &catalogs.themes);
    group
}

/// The font picker's rows: Default, the monospaced families, and a configured family the system does not list.
fn font_options(fonts: &[String], configured: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut options = vec![("Default".to_owned(), None)];
    options.extend(fonts.iter().map(|font| (font.clone(), Some(font.clone()))));
    if let Some(configured) = configured.filter(|family| !fonts.iter().any(|font| font == family)) {
        options.push((configured.to_owned(), Some(configured.to_owned())));
    }
    options
}

/// The theme pickers (upstream's two `Picker`s and the follow toggle): the first is the theme on screen, with ghostty's
/// built-in offered only while not following; the second, while following, the other appearance's theme.
fn add_themes(rows: &mut Rows, group: &adw::PreferencesGroup, themes: &Rc<[String]>) {
    let offered = Rc::clone(themes);
    group.add(&rows::choice(
        rows,
        "settings-theme",
        "Theme",
        move |snapshot| {
            let builtin = !snapshot.settings.follows_system_appearance();
            theme_options(&offered, builtin, snapshot_active(snapshot))
        },
        |snapshot| snapshot_active(snapshot).map(str::to_owned),
        |theme| {
            change(AppearanceChange::ThemeForCurrentAppearance {
                theme,
                dark: crate::ghostty_app::is_dark(),
            })
        },
    ));
    group.add(&rows::switch(
        rows,
        "settings-follow-appearance",
        "Follow system appearance",
        |snapshot| snapshot.settings.follows_system_appearance(),
        |on| {
            change(AppearanceChange::FollowSystemAppearance {
                on,
                dark: crate::ghostty_app::is_dark(),
            })
        },
    ));
    let offered = Rc::clone(themes);
    let alternate = rows::choice(
        rows,
        "settings-theme-dark",
        "Dark theme",
        move |snapshot| theme_options(&offered, false, snapshot.settings.alternate_theme(snapshot.dark)),
        |snapshot| snapshot.settings.alternate_theme(snapshot.dark).map(str::to_owned),
        |theme| {
            change(AppearanceChange::AlternateTheme {
                theme,
                dark: crate::ghostty_app::is_dark(),
            })
        },
    );
    let shown = alternate.clone();
    rows.refresh(move |snapshot| {
        let (title, other) = if snapshot.dark {
            ("Light theme", "light")
        } else {
            ("Dark theme", "dark")
        };
        shown.set_title(title);
        shown.set_subtitle(&format!("Used when the desktop is in {other} mode."));
    });
    rows::show_when(rows, &alternate, |snapshot| {
        snapshot.settings.follows_system_appearance()
    });
    group.add(&alternate);
}

fn snapshot_active(snapshot: &Snapshot) -> Option<&str> {
    snapshot.settings.active_theme(snapshot.dark)
}

/// A theme picker's rows: ghostty's built-in when `builtin`, the bundled themes, and a set theme not among them.
fn theme_options(themes: &[String], builtin: bool, current: Option<&str>) -> Vec<(String, Option<String>)> {
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

fn window(rows: &mut Rows) -> adw::PreferencesGroup {
    let group = rows::group("Window");
    group.add(&rows::picker(
        rows,
        "settings-toolbar-mode",
        "Toolbar",
        &[
            ("Normal", ToolbarMode::Normal),
            ("Compact", ToolbarMode::Compact),
            ("Hidden", ToolbarMode::Hidden),
        ],
        |snapshot| snapshot.settings.effective_toolbar_mode(),
        |mode| change(AppearanceChange::ToolbarMode(mode)),
    ));
    add_translucency(rows, &group);
    add_sidebar_and_mute(rows, &group);
    group
}

fn add_translucency(rows: &mut Rows, group: &adw::PreferencesGroup) {
    group.add(&rows::slider(
        rows,
        "settings-bg-opacity",
        "Background Opacity",
        Slider {
            range: 0.0..=100.0,
            get: |snapshot: &Snapshot| opacity_percent(snapshot),
            label: |snapshot: &Snapshot| format!("{}%", opacity_percent(snapshot)),
            set: |percent| change(AppearanceChange::BackgroundOpacity(percent / 100.0)),
        },
    ));
    let blur = rows::slider(
        rows,
        "settings-bg-blur",
        "Background Blur",
        Slider {
            range: 0.0..=100.0,
            get: |snapshot: &Snapshot| blur_of(snapshot),
            label: |snapshot: &Snapshot| blur_of(snapshot).to_string(),
            set: |blur| change(AppearanceChange::BackgroundBlur(whole(blur))),
        },
    );
    blur.set_subtitle("Blur needs opacity below 100%.");
    rows::enable_when(rows, &blur, |snapshot| {
        snapshot.settings.background_opacity.unwrap_or(1.0) < 1.0
    });
    group.add(&blur);
}

/// The opacity as the slider's whole percentage.
fn opacity_percent(snapshot: &Snapshot) -> f64 {
    (snapshot.settings.background_opacity.unwrap_or(1.0) * 100.0).round()
}

fn blur_of(snapshot: &Snapshot) -> f64 {
    let blur = snapshot.settings.background_blur.unwrap_or(0).clamp(0, 100);
    f64::from(u8::try_from(blur).unwrap_or(0))
}

/// A slider position as a whole number; the sliders round to whole steps, within `i64`.
fn whole(value: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a slider value, rounded and within 0...100"
    )]
    let whole = value.round() as i64;
    whole
}

fn add_sidebar_and_mute(rows: &mut Rows, group: &adw::PreferencesGroup) {
    group.add(&rows::slider(
        rows,
        "settings-sidebar-shift",
        "Sidebar Tint",
        Slider {
            range: 0.0..=10.0,
            get: |snapshot: &Snapshot| strength(snapshot.settings.effective_sidebar_shift()),
            label: |snapshot: &Snapshot| sidebar_shift_label(snapshot.settings.effective_sidebar_shift()),
            set: |shift| change(AppearanceChange::SidebarBackgroundShift(whole(shift))),
        },
    ));
    group.add(&rows::stepper(
        rows,
        "settings-sidebar-font-size",
        "Sidebar font size",
        &SIDEBAR_FONT_SIZE_RANGE,
        |snapshot| snapshot.settings.effective_sidebar_font_size(),
        |size| change(AppearanceChange::SidebarFontSize(size)),
    ));
    group.add(&rows::stepper(
        rows,
        "settings-interface-font-size",
        "Palette and switcher font size",
        &INTERFACE_FONT_SIZE_RANGE,
        |snapshot| snapshot.settings.effective_interface_font_size(),
        |size| change(AppearanceChange::InterfaceFontSize(size)),
    ));
    group.add(&rows::slider(
        rows,
        "settings-inactive-pane-mute",
        "Inactive pane and backdrop mute",
        Slider {
            range: 0.0..=10.0,
            get: |snapshot: &Snapshot| strength(snapshot.settings.effective_inactive_pane_mute()),
            label: |snapshot: &Snapshot| snapshot.settings.effective_inactive_pane_mute().to_string(),
            set: |mute| change(AppearanceChange::InactivePaneMute(whole(mute))),
        },
    ));
}

/// A `0...10` strength as the slider's position.
fn strength(value: i64) -> f64 {
    f64::from(u8::try_from(value.clamp(0, 10)).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_font_picker_keeps_a_configured_family_the_system_lacks() {
        let fonts = ["Iosevka".to_owned(), "Mono".to_owned()];
        let labels = |configured| -> Vec<String> {
            font_options(&fonts, configured)
                .into_iter()
                .map(|(label, _)| label)
                .collect()
        };
        assert_eq!(labels(None), ["Default", "Iosevka", "Mono"]);
        assert_eq!(labels(Some("Mono")), ["Default", "Iosevka", "Mono"]);
        assert_eq!(labels(Some("Gone")), ["Default", "Iosevka", "Mono", "Gone"]);
    }

    #[test]
    fn whole_rounds_slider_positions() {
        assert_eq!((whole(4.6), strength(12)), (5, 10.0));
    }
}
