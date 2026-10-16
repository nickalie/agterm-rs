//! The chrome's colors from the terminals' config (the GTK build's `chrome.rs` tests over the stylesheet, here over
//! the resolved colors and what the bar and the sidebar paint), the global's updates, and the resize handles of a
//! client-decorated window.

use std::sync::Arc;

use agterm_core::model::WindowLibrary;
use agterm_core::{CoreConfig, CoreState};
use agterm_proto::AppIdentity;
use gpui_kit::TestAppContext;

use super::*;
use crate::terminal::Palette;

fn config(selection: (Option<Pick>, Option<Pick>)) -> TerminalConfig {
    let mut config = TerminalConfig::default();
    config.theme.background = agterm_term::Rgb::new(0x10, 0x20, 0x30);
    config.theme.foreground = agterm_term::Rgb::new(0xe0, 0xe0, 0xe0);
    let mut palette = Palette::clone(&config.palette);
    palette.selection_background = selection.0;
    palette.selection_foreground = selection.1;
    config.palette = Arc::new(palette);
    config
}

fn tint(shift: i64) -> AppSettings {
    AppSettings {
        sidebar_background_shift: Some(shift),
        ..AppSettings::default()
    }
}

#[test]
fn the_window_bar_and_sidebar_take_the_terminal_background() {
    let colors = colors(&config((None, None)), &tint(5));
    assert_eq!(colors.background, Rgb::new(0x10, 0x20, 0x30));
    assert_eq!(colors.foreground, Rgb::new(0xe0, 0xe0, 0xe0));
    assert_eq!(colors.sidebar, colors.background, "the neutral tint");
    // the hairline and the divider: the foreground at 0.1
    assert_eq!(separator(&colors), color(Rgb::new(0xe0, 0xe0, 0xe0), 0.1));
}

#[test]
fn the_tint_washes_the_sidebar_only() {
    let colors = colors(&config((None, None)), &tint(10));
    assert_eq!(colors.background, Rgb::new(0x10, 0x20, 0x30));
    assert_eq!(colors.sidebar, Rgb::new(0x0b, 0x16, 0x22));
}

#[test]
fn selected_rows_take_the_selection_colors() {
    let selection = Pick::Color(agterm_term::Rgb::new(0x44, 0x47, 0x5a));
    let colors = colors(&config((Some(selection), None)), &tint(5));
    assert_eq!(colors.selection_background, Some(Rgb::new(0x44, 0x47, 0x5a)));
    assert_eq!(colors.selection_foreground, Rgb::WHITE, "the contrast of the pill");
    let (pill, text) = selected_row(&colors, true);
    assert_eq!(pill, color(Rgb::new(0x44, 0x47, 0x5a), 1.0));
    assert_eq!(text, color(Rgb::WHITE, 1.0));
    assert_eq!(selected_row(&colors, false).0, color(Rgb::new(0x44, 0x47, 0x5a), 0.55));
    // no selection background: upstream's soft white wash, white text
    let colors = colors_of(&config((None, None)));
    assert_eq!(selected_row(&colors, true).0, color(Rgb::WHITE, 0.13));
    assert_eq!(selected_row(&colors, false).0, color(Rgb::WHITE, 0.07));
    // a selection that follows the cell is no fixed color
    let colors = colors_of(&config((Some(Pick::CellForeground), Some(Pick::CellBackground))));
    assert_eq!(colors.selection_background, None);
    assert_eq!(colors.selection_foreground, Rgb::WHITE);
}

fn colors_of(config: &TerminalConfig) -> ChromeColors {
    colors(config, &tint(5))
}

#[test]
fn a_client_decorated_window_resizes_from_its_free_edges() {
    let free = Decorations::Client {
        tiling: Tiling::default(),
    };
    assert_eq!(resize_zones(free, false).len(), 8, "every edge and corner");
    assert!(resize_zones(free, true).is_empty(), "maximized or full screen");
    assert!(
        resize_zones(Decorations::Server, false).is_empty(),
        "the window manager's"
    );
    let left_tiled = Decorations::Client {
        tiling: Tiling {
            left: true,
            ..Tiling::default()
        },
    };
    let zones = resize_zones(left_tiled, false);
    assert!(!zones.contains(&ResizeEdge::Left));
    assert!(!zones.contains(&ResizeEdge::TopLeft) && !zones.contains(&ResizeEdge::BottomLeft));
    assert!(zones.contains(&ResizeEdge::Right) && zones.contains(&ResizeEdge::TopRight));
    assert!(
        resize_zones(
            Decorations::Client {
                tiling: Tiling::tiled()
            },
            false
        )
        .is_empty()
    );
}

#[gpui_kit::test]
fn the_global_follows_the_config_and_the_tint(cx: &mut TestAppContext) {
    let mut core = agterm_core::Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library: WindowLibrary::seeded("/home/u"),
            ..CoreState::default()
        },
    );
    let dark = config((None, None));
    cx.update(|cx| {
        assert_eq!(current(cx), colors(&TerminalConfig::default(), &AppSettings::default()));
        assert!(sync(&core, &dark, cx), "the first sync paints");
        assert!(!sync(&core, &dark, cx), "unchanged colors leave the global alone");
        assert_eq!(current(cx).background, Rgb::new(0x10, 0x20, 0x30));
    });
    core.update_settings(|settings| settings.sidebar_background_shift = Some(10));
    cx.update(|cx| {
        assert!(sync(&core, &dark, cx), "a tint change repaints");
        assert_eq!(current(cx).sidebar, Rgb::new(0x0b, 0x16, 0x22));
    });
}

// MARK: - the library's theme

/// A light theme's config: a near-white background under dark text.
fn light_config() -> TerminalConfig {
    let mut config = TerminalConfig::default();
    config.theme.background = agterm_term::Rgb::new(0xfa, 0xfa, 0xf5);
    config.theme.foreground = agterm_term::Rgb::new(0x20, 0x20, 0x28);
    config
}

/// `foreground` at `alpha` over `background`, flattened: what a hovered row or a menu's wash shows.
fn washed(background: Rgb, foreground: Rgb, alpha: f64) -> Hsla {
    color(background, 1.0).blend(color(foreground, alpha))
}

fn core_seeded() -> agterm_core::Core {
    agterm_core::Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library: WindowLibrary::seeded("/home/u"),
            ..CoreState::default()
        },
    )
}

/// What the library's widgets paint with now: its side, its window, popup, hover, pressed, border and dim-text colors,
/// and the side of its base layer (the scrollbars).
fn kit_shows(cx: &App) -> (bool, [Hsla; 10], bool) {
    use gpui_kit::component::ActiveTheme as _;
    let theme = cx.theme();
    let base = cx.global::<gpui_kit::base::Theme>().appearance == gpui_kit::base::ThemeAppearance::Dark;
    let colors = [
        theme.background,
        theme.popover,
        theme.popover_foreground,
        theme.accent,
        theme.list_hover,
        theme.secondary_hover,
        theme.button_active,
        theme.border,
        theme.muted_foreground,
        theme.accent_foreground,
    ];
    (theme.mode.is_dark(), colors, base)
}

/// What the library should paint with over a chrome of `background` and `foreground` text.
fn kit_expected(background: Rgb, foreground: Rgb) -> [Hsla; 10] {
    [
        color(background, 1.0),
        color(background, 1.0),
        color(foreground, 1.0),
        washed(background, foreground, 0.12),
        washed(background, foreground, 0.12),
        washed(background, foreground, 0.12),
        washed(background, foreground, 0.18),
        washed(background, foreground, 0.15),
        washed(background, foreground, DIM_ALPHA),
        color(foreground, 1.0),
    ]
}

#[test]
fn the_library_side_is_the_washed_sidebar_brightness() {
    use gpui_kit::component::ThemeMode;
    assert_eq!(kit::mode(&colors_of(&config((None, None)))), ThemeMode::Dark);
    assert_eq!(kit::mode(&colors_of(&light_config())), ThemeMode::Light);
    // upstream's `terminalThemeIsDark` reads the sidebar under its tint: a full lighten over a mid grey reads light
    let mut grey = TerminalConfig::default();
    grey.theme.background = agterm_term::Rgb::new(0x70, 0x70, 0x70);
    assert_eq!(kit::mode(&colors(&grey, &tint(5))), ThemeMode::Dark);
    assert_eq!(kit::mode(&colors(&grey, &tint(0))), ThemeMode::Light);
}

/// The library's colors in `cx` are the dark or light side's chrome colors `expected`, never its own white in the
/// dark nor its black in the light.
fn check_kit(cx: &App, dark: bool, expected: [Hsla; 10], when: &str) {
    let (side, shown, base) = kit_shows(cx);
    assert_eq!(side, dark, "{when}: the library's side");
    assert_eq!(base, dark, "{when}: the scrollbars' side");
    assert_eq!(
        shown, expected,
        "{when}: window, popup, hover, pressed, border and dim text"
    );
    for fill in &shown[..2] {
        assert_eq!(fill.l < 0.5, dark, "{when}: {fill:?}");
    }
}

#[gpui_kit::test]
fn the_library_theme_follows_the_chrome_through_every_flip(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let core = core_seeded();
    let (dark, light) = (config((None, None)), light_config());
    let dark_kit = kit_expected(Rgb::new(0x10, 0x20, 0x30), Rgb::new(0xe0, 0xe0, 0xe0));
    let light_kit = kit_expected(Rgb::new(0xfa, 0xfa, 0xf5), Rgb::new(0x20, 0x20, 0x28));
    cx.update(|cx| {
        sync(&core, &dark, cx);
        check_kit(cx, true, dark_kit, "dark");
        assert!(
            !kit::sync(&current(cx), cx),
            "an unchanged chrome leaves the theme alone"
        );
        sync(&core, &light, cx);
        check_kit(cx, false, light_kit, "dark to light");
        sync(&core, &dark, cx);
        check_kit(cx, true, dark_kit, "light to dark");
        // whatever puts the library back on its light palette, the next sync puts it on the chrome again, though the
        // chrome itself did not change
        gpui_kit::init(cx);
        assert!(!sync(&core, &dark, cx), "the chrome is unchanged");
        check_kit(cx, true, dark_kit, "after a reset");
    });
}
