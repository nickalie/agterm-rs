//! The window chrome in the terminal's colors (upstream's `WindowAppearance.sync`, `WindowContentView`'s
//! `titlebarHairline`, `sidebarDivider` and `sidebarTintWash`, and the sidebar rows' selection pill): the window body
//! and the header bar take the theme's background in every toolbar mode, the sidebar (its rows and footer) the same
//! background under the Settings tint's wash, never the title strip; the chrome text takes the theme's foreground, the
//! title-bar hairline (not in the hidden mode) and the sidebar divider that foreground at low opacity, and a selected
//! sidebar row the theme's selection colors. [`agterm_core::chrome`] resolves the colors.
//!
//! One display-wide stylesheet carries it, scoped to the main windows by [`WINDOW_CLASS`], so the popovers, dialogs and
//! palettes keep their own styles. [`sync`] runs on every settle after a config reload, so every appearance change
//! upstream re-applies on (a theme set or previewed, a config reload, a settings change, a desktop light/dark flip that
//! re-feeds the config) reaches it; it reloads the stylesheet only when a color changed.

use std::cell::RefCell;
use std::fmt::Write;

use agterm_core::Core;
use agterm_core::chrome::{ChromeColors, Rgb};
use gtk::gdk;

/// The class of every main window.
pub const WINDOW_CLASS: &str = "agterm-chrome";
/// The class of a main window's header bar.
pub const BAR_CLASS: &str = "agterm-chrome-bar";
/// The header bar's class while the toolbar mode is hidden, which draws no hairline (upstream #368).
pub const BAR_HIDDEN_CLASS: &str = "agterm-toolbar-hidden";
/// The class of the paned holding the sidebar beside the deck, whose handle is the sidebar divider.
pub const SPLIT_ROOT_CLASS: &str = "agterm-split-root";

/// `color` at `alpha` as a CSS color.
fn rgba(color: Rgb, alpha: f64) -> String {
    format!("rgba({}, {}, {}, {alpha})", color.r, color.g, color.b)
}

/// The stylesheet painting the chrome in `colors`.
#[must_use]
pub fn stylesheet(colors: &ChromeColors) -> String {
    let ChromeColors {
        background,
        foreground,
        sidebar,
        ..
    } = *colors;
    let separator = rgba(foreground, ChromeColors::SEPARATOR_ALPHA);
    let window = format!("window.{WINDOW_CLASS}");
    let mut css = String::new();
    let _ = writeln!(css, "{window} {{ background-color: {background}; }}");
    let _ = writeln!(
        css,
        "{window} headerbar.{BAR_CLASS} {{ background: none; color: {foreground}; box-shadow: none; }}"
    );
    let _ = writeln!(
        css,
        "{window} headerbar.{BAR_CLASS}:not(.{BAR_HIDDEN_CLASS}) {{ box-shadow: inset 0 -1px {separator}; }}"
    );
    let _ = writeln!(
        css,
        "{window} paned.{SPLIT_ROOT_CLASS} > separator {{ background: none; box-shadow: inset 1px 0 {separator}; }}"
    );
    let _ = writeln!(
        css,
        "{window} .agterm-sidebar {{ background-color: {sidebar}; color: {foreground}; }}"
    );
    css.push_str(&selected_rows(colors, &window));
    css
}

/// The selected rows' pill and text: the selection background, dimmed while the window is not the active one, else
/// upstream's soft white wash.
fn selected_rows(colors: &ChromeColors, window: &str) -> String {
    let (active, backdrop) = match colors.selection_background {
        Some(selection) => (
            selection.to_string(),
            rgba(selection, ChromeColors::INACTIVE_SELECTION_ALPHA),
        ),
        None => (
            rgba(Rgb::WHITE, ChromeColors::WASH_ALPHA),
            rgba(Rgb::WHITE, ChromeColors::INACTIVE_WASH_ALPHA),
        ),
    };
    let row = format!("{window} .agterm-sidebar list.navigation-sidebar > row:selected");
    format!(
        "{row} {{ background-color: {active}; color: {}; }}\n{row}:backdrop {{ background-color: {backdrop}; }}\n",
        colors.selection_foreground
    )
}

/// The chrome's colors now: the terminal colors of the config on screen, the theme's selection colors and the
/// settings' sidebar tint.
fn current(core: &Core) -> ChromeColors {
    let rgb = |color: agterm_ghostty::Color| Rgb::new(color.r, color.g, color.b);
    ChromeColors::new(
        crate::ghostty_app::background_color().map(rgb),
        crate::ghostty_app::foreground_color().map(rgb),
        crate::ghostty_app::selection_colors(),
        core.settings().effective_sidebar_shift(),
    )
}

thread_local! {
    /// The chrome's provider and the colors it was last loaded with.
    static APPLIED: RefCell<Option<(gtk::CssProvider, ChromeColors)>> = const { RefCell::new(None) };
}

/// Paints the chrome in the colors the terminals and the settings resolve to now; a no-op while they are unchanged, and
/// off GTK's main thread (a headless state test settles there).
pub fn sync(core: &Core) {
    if !gtk::is_initialized_main_thread() {
        return;
    }
    let colors = current(core);
    APPLIED.with(|applied| {
        let mut applied = applied.borrow_mut();
        if applied.as_ref().is_some_and(|(_, shown)| *shown == colors) {
            return;
        }
        let provider = if let Some((provider, _)) = applied.take() {
            provider
        } else {
            let Some(display) = gdk::Display::default() else {
                return;
            };
            let provider = gtk::CssProvider::new();
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            provider
        };
        provider.load_from_string(&stylesheet(&colors));
        tracing::debug!(background = %colors.background, sidebar = %colors.sidebar, "chrome colors applied");
        *applied = Some((provider, colors));
    });
}

#[cfg(test)]
mod tests {
    use agterm_core::chrome::SelectionColors;

    use super::*;

    fn colors(selection: SelectionColors, shift: i64) -> ChromeColors {
        ChromeColors::new(
            Some(Rgb::new(0x10, 0x20, 0x30)),
            Some(Rgb::new(0xe0, 0xe0, 0xe0)),
            selection,
            shift,
        )
    }

    #[test]
    fn the_window_bar_and_sidebar_take_the_terminal_background() {
        let css = stylesheet(&colors(SelectionColors::default(), 5));
        assert!(
            css.contains("window.agterm-chrome { background-color: #102030; }"),
            "{css}"
        );
        assert!(css.contains("headerbar.agterm-chrome-bar { background: none; color: #e0e0e0; box-shadow: none; }"));
        assert!(css.contains(".agterm-sidebar { background-color: #102030; color: #e0e0e0; }"));
        // the hairline and the divider: the foreground at 0.1, no hairline in the hidden mode
        assert!(css.contains(
            "headerbar.agterm-chrome-bar:not(.agterm-toolbar-hidden) { box-shadow: inset 0 -1px rgba(224, 224, 224, 0.1); }"
        ));
        assert!(css.contains("paned.agterm-split-root > separator { background: none; box-shadow: inset 1px 0 rgba(224, 224, 224, 0.1); }"));
        // every rule is scoped to the main windows
        assert!(
            css.lines().all(|line| line.starts_with("window.agterm-chrome")),
            "{css}"
        );
    }

    #[test]
    fn the_tint_washes_the_sidebar_only() {
        let css = stylesheet(&colors(SelectionColors::default(), 10));
        assert!(css.contains("window.agterm-chrome { background-color: #102030; }"));
        assert!(
            css.contains(".agterm-sidebar { background-color: #0b1622; color: #e0e0e0; }"),
            "{css}"
        );
    }

    #[test]
    fn selected_rows_take_the_selection_colors() {
        let selection = SelectionColors {
            background: Some(Rgb::new(0x44, 0x47, 0x5a)),
            foreground: None,
        };
        let css = stylesheet(&colors(selection, 5));
        assert!(
            css.contains("row:selected { background-color: #44475a; color: #ffffff; }"),
            "{css}"
        );
        assert!(css.contains("row:selected:backdrop { background-color: rgba(68, 71, 90, 0.55); }"));
        // no selection background: upstream's soft white wash, white text
        let css = stylesheet(&colors(SelectionColors::default(), 5));
        assert!(css.contains("row:selected { background-color: rgba(255, 255, 255, 0.13); color: #ffffff; }"));
        assert!(css.contains("row:selected:backdrop { background-color: rgba(255, 255, 255, 0.07); }"));
    }
}
