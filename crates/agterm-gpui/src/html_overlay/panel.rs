//! A page's panel (upstream's `HtmlOverlayView`; port of the GTK build's `html_overlay/panel.rs`): the colors it is
//! drawn in, the app-drawn identity strip the page cannot cover with the navigation buttons of `--navigation`, and
//! the failure shown over a page that did not load. A chromeless page has no strip. The element is
//! [`super::view::PageView`].

use agterm_core::chrome::FALLBACK_BACKGROUND;
use agterm_core::html::{HtmlLoadState, HtmlNavigation, HtmlOverlay, HtmlOverlayTheme, HtmlReloadTarget};
use agterm_core::model::is_valid_color_hex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px, svg,
};
use uuid::Uuid;

use super::policy::{self, StripItem};
use super::registry;
use crate::state::{AppState, Poster, post};

/// The theme foreground when the terminal's is unknown (upstream's `HtmlOverlayRegistry` falls back to white).
const FALLBACK_FOREGROUND: &str = "#ffffff";
/// The strip's height (upstream's 28 points).
pub const STRIP_HEIGHT: f32 = 28.;
/// How many characters of the identity hold their place before the page's title gives up width.
const IDENTITY_CHARS: usize = 24;

/// The terminal's colors the pages are drawn in, each `#rrggbb`: its background, its foreground and its 16 ANSI
/// colors (empty when unknown).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalColors {
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub palette: Vec<String>,
}

impl TerminalColors {
    /// The colors of the terminals' theme.
    #[must_use]
    pub fn of(theme: &agterm_term::ThemeColors) -> Self {
        let hex = |color: agterm_term::Rgb| format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b);
        Self {
            background: Some(hex(theme.background)),
            foreground: Some(hex(theme.foreground)),
            palette: theme
                .ansi()
                .map(|ansi| ansi.iter().copied().map(hex).collect())
                .unwrap_or_default(),
        }
    }
}

/// The colors a page is drawn in: the terminal's, the overlay's own background first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelColors {
    /// The terminal's background, which the strip sits on.
    pub terminal: String,
    /// The page's background: the overlay's own, else the terminal's.
    pub background: String,
    pub foreground: String,
    pub dark: bool,
    /// The theme's 16 ANSI colors as `#rrggbb`, empty when unknown.
    pub palette: Vec<String>,
}

impl PanelColors {
    /// The colors under the overlay's `background` (`#rrggbb`) in the `terminal`'s colors (each falling back to
    /// upstream's when unknown), the terminal's background when the overlay has none or an invalid one.
    #[must_use]
    pub fn resolve(terminal: &TerminalColors, background: Option<&str>) -> Self {
        let terminal_background = terminal
            .background
            .clone()
            .unwrap_or_else(|| FALLBACK_BACKGROUND.to_string());
        let foreground = terminal
            .foreground
            .clone()
            .unwrap_or_else(|| FALLBACK_FOREGROUND.to_owned());
        let background = background.filter(|color| is_valid_color_hex(color)).map_or_else(
            || terminal_background.clone(),
            |color| format!("#{}", color.trim_start_matches('#')),
        );
        let dark = rgb(&background).is_none_or(|(r, g, b)| agterm_core::html::theme::is_dark(r, g, b));
        Self {
            terminal: terminal_background,
            background,
            foreground,
            dark,
            palette: terminal.palette.clone(),
        }
    }

    /// The terminal theme as the page sees it (upstream's `HtmlOverlayRegistry.theme`), its palette giving the page
    /// `--agterm-color-0` to `15`.
    #[must_use]
    pub fn theme(&self) -> HtmlOverlayTheme {
        HtmlOverlayTheme::new(&self.background, &self.foreground, self.dark, &self.palette)
    }

    fn hsla(hex: &str, alpha: f32) -> Hsla {
        let (r, g, b) = rgb(hex).unwrap_or((0.0, 0.0, 0.0));
        #[expect(clippy::cast_possible_truncation, reason = "a channel within 0...1")]
        let color = gpui_kit::Rgba {
            r: r as f32,
            g: g as f32,
            b: b as f32,
            a: alpha,
        };
        color.into()
    }

    /// The page's background.
    #[must_use]
    pub fn background(&self) -> Hsla {
        Self::hsla(&self.background, 1.0)
    }

    /// The foreground at `alpha`.
    #[must_use]
    pub fn ink(&self, alpha: f32) -> Hsla {
        Self::hsla(&self.foreground, alpha)
    }

    /// The strip's ground: the terminal's background under a faint wash of the text color (upstream's opacities).
    #[must_use]
    pub fn strip(&self) -> Hsla {
        let (r, g, b) = rgb(&self.terminal).unwrap_or((0.0, 0.0, 0.0));
        let (fr, fg, fb) = rgb(&self.foreground).unwrap_or((1.0, 1.0, 1.0));
        let mix = |base: f64, wash: f64| base + (wash - base) * 0.06;
        #[expect(clippy::cast_possible_truncation, reason = "a channel within 0...1")]
        let color = gpui_kit::Rgba {
            r: mix(r, fr) as f32,
            g: mix(g, fg) as f32,
            b: mix(b, fb) as f32,
            a: 1.0,
        };
        color.into()
    }
}

/// The components of a `#rrggbb` color, each 0...1.
fn rgb(hex: &str) -> Option<(f64, f64, f64)> {
    let digits = hex.strip_prefix('#')?;
    let channel = |at: usize| {
        digits
            .get(at..at + 2)
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            .map(|value| f64::from(value) / 255.0)
    };
    Some((channel(0)?, channel(2)?, channel(4)?))
}

// MARK: - the strip

/// The identity strip of `overlay` in `colors`, its buttons acting on page `id`; `None` for a chromeless page.
#[must_use]
pub fn strip(overlay: &HtmlOverlay, colors: &PanelColors, poster: &Poster) -> Option<AnyElement> {
    let items = policy::strip_items(overlay)?;
    let current = overlay.current.as_ref();
    let mut bar = div()
        .id(SharedString::from(format!("html-strip-{}", overlay.id)))
        .flex_shrink_0()
        .h(px(STRIP_HEIGHT))
        .px(px(10.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .bg(colors.strip())
        .text_size(px(12.))
        .text_color(colors.ink(1.0));
    for item in items {
        bar = match item {
            StripItem::Identity => bar.child(identity(overlay, colors)),
            StripItem::Back => bar.child(button(
                overlay.id,
                item,
                current.is_some_and(|page| page.can_go_back),
                colors,
                poster,
            )),
            StripItem::Forward => bar.child(button(
                overlay.id,
                item,
                current.is_some_and(|page| page.can_go_forward),
                colors,
                poster,
            )),
            _ => bar.child(button(overlay.id, item, true, colors, poster)),
        };
    }
    Some(
        div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .child(bar)
            .child(div().h(px(1.)).bg(colors.ink(0.1)))
            .into_any_element(),
    )
}

/// The identity label and the page's title, centered between the buttons.
fn identity(overlay: &HtmlOverlay, colors: &PanelColors) -> AnyElement {
    let identity = overlay.identity();
    let title = overlay.current.as_ref().and_then(|current| current.title.clone());
    let chars = identity.chars().count().min(IDENTITY_CHARS);
    #[expect(clippy::cast_precision_loss, reason = "at most 24 characters")]
    let reserved = chars as f32 * 6.5;
    div()
        .id("html-identity")
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_row()
        .justify_center()
        .items_center()
        .gap(px(6.))
        .overflow_hidden()
        .tooltip({
            let tip = SharedString::from(policy::identity_tooltip(overlay));
            move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
        })
        .child(
            div()
                .id("htmlOverlay.identity")
                .debug_selector(|| "htmlOverlay.identity".to_owned())
                .accessibility_id("htmlOverlay.identity")
                .role(Role::Label)
                .min_w(px(reserved.min(160.)))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(SharedString::from(identity)),
        )
        .when_some(title, |row, title| {
            row.child(div().text_color(colors.ink(0.4)).child("·")).child(
                div()
                    .id("htmlOverlay.title")
                    .debug_selector(|| "htmlOverlay.title".to_owned())
                    .accessibility_id("htmlOverlay.title")
                    .role(Role::Label)
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(colors.ink(0.55))
                    .child(SharedString::from(title)),
            )
        })
        .into_any_element()
}

/// One of the strip's flat icon buttons; it never takes the keyboard from the page.
fn button(id: Uuid, item: StripItem, enabled: bool, colors: &PanelColors, poster: &Poster) -> AnyElement {
    let (icon, tooltip) = match item {
        StripItem::Back => ("icons/chevron-left.svg", "Back"),
        StripItem::Forward => ("icons/chevron-right.svg", "Forward"),
        StripItem::Reload => ("icons/rotate-cw.svg", "Reload"),
        StripItem::Browser => ("icons/compass.svg", "Open in Browser"),
        StripItem::Files => ("icons/folder-open.svg", "Show in Files"),
        StripItem::CopyLink => ("icons/link.svg", "Copy Link"),
        StripItem::Close | StripItem::Identity => ("icons/x.svg", "Close"),
    };
    let ink = colors.ink(if enabled { 1.0 } else { 0.3 });
    let poster = poster.clone();
    let element_id = accessibility_id(item);
    div()
        .id(element_id)
        .debug_selector(move || element_id.to_owned())
        .accessibility_id(element_id)
        .role(Role::Button)
        .aria_label(tooltip)
        .flex_shrink_0()
        .w(px(22.))
        .h(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .when(enabled, |button| {
            button.hover(|style| style.bg(gpui_kit::hsla(0., 0., 0.5, 0.15)))
        })
        .child(svg().path(icon).size(px(14.)).text_color(ink))
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx))
        // the page keeps the keyboard: a press here is no click into the page
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(enabled, |button| {
            button.on_click(move |_, _, cx| {
                cx.stop_propagation();
                act(id, item, &poster, cx);
            })
        })
        .into_any_element()
}

/// The strip item's element id, upstream's accessibility identifier (`HtmlOverlayView.strip`); the identity is its
/// label's.
#[must_use]
pub fn accessibility_id(item: StripItem) -> &'static str {
    match item {
        StripItem::Back => "htmlOverlay.back",
        StripItem::Forward => "htmlOverlay.forward",
        StripItem::Reload => "htmlOverlay.reload",
        StripItem::Browser => "htmlOverlay.browser",
        StripItem::Files => "htmlOverlay.finder",
        StripItem::CopyLink => "htmlOverlay.copy",
        StripItem::Close => "htmlOverlay.close",
        StripItem::Identity => "htmlOverlay.identity",
    }
}

/// What a strip button does: history and hand-offs go to the page's engine page, reload and close to the model.
pub fn act(id: Uuid, item: StripItem, poster: &Poster, cx: &mut App) {
    let navigation = match item {
        StripItem::Back => HtmlNavigation::Back,
        StripItem::Forward => HtmlNavigation::Forward,
        StripItem::Browser => HtmlNavigation::Browser,
        StripItem::Files => HtmlNavigation::Finder,
        StripItem::CopyLink => return registry::copy_link(id, cx),
        StripItem::Reload => {
            return post(poster, move |state: &mut AppState| {
                if state.core.reload_html_overlay(id, HtmlReloadTarget::Current) {
                    state.settle();
                }
            });
        }
        StripItem::Close | StripItem::Identity => {
            return post(poster, move |state: &mut AppState| {
                if state.core.close_html_overlay(id) {
                    state.settle();
                }
            });
        }
    };
    if let Err(error) = registry::navigate(id, navigation) {
        tracing::debug!(%error, "page navigation refused");
    }
}

/// The failure shown over a page that did not load, so a failed load never reads as a blank one; a reload replaces it
/// with the loading state. `None` unless the page failed.
#[must_use]
pub fn failure(overlay: &HtmlOverlay, colors: &PanelColors, error: Option<AnyElement>) -> Option<AnyElement> {
    if overlay.load_state != HtmlLoadState::Failed {
        return None;
    }
    // the error as the element made it (selectable text), else as plain text
    let error = error.or_else(|| {
        overlay
            .load_error
            .clone()
            .map(|error| SharedString::from(error).into_any_element())
    });
    Some(
        div()
            .id("htmlOverlay.error")
            .debug_selector(|| "htmlOverlay.error".to_owned())
            .accessibility_id("htmlOverlay.error")
            .role(Role::Alert)
            // the page under it takes no press: one here selects the error's text
            .occlude()
            .absolute()
            .inset_0()
            .bg(colors.background())
            .text_color(colors.ink(1.0))
            .p(px(24.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .child(
                svg()
                    .path("icons/triangle-alert.svg")
                    .size(px(24.))
                    .text_color(colors.ink(1.0)),
            )
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::BOLD)
                    .child("The page could not be loaded"),
            )
            .when_some(error, |column, error| {
                column.child(
                    div()
                        .debug_selector(|| "html-failure-error".to_owned())
                        .max_w_full()
                        .text_size(px(12.))
                        .text_center()
                        .child(error),
                )
            })
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors_read_as_components() {
        assert_eq!(rgb("#ff0000"), Some((1.0, 0.0, 0.0)));
        assert_eq!(rgb("#zz0000"), None);
        assert_eq!(rgb("ff0000"), None);
    }

    fn palette() -> Vec<String> {
        (0..16).map(|slot| format!("#0000{slot:02x}")).collect()
    }

    fn terminal(background: Option<&str>, foreground: Option<&str>, palette: Vec<String>) -> TerminalColors {
        TerminalColors {
            background: background.map(str::to_owned),
            foreground: foreground.map(str::to_owned),
            palette,
        }
    }

    #[test]
    fn the_theme_carries_the_terminal_palette() {
        let colors = PanelColors::resolve(&terminal(Some("#101010"), Some("#eeeeee"), palette()), None);
        let theme = colors.theme();
        assert_eq!(theme.palette, palette(), "a page gets --agterm-color-0 to 15");
        assert_eq!(theme.background, "#101010");
        assert_eq!(theme.foreground, "#eeeeee");
        assert!(theme.dark);
        assert!(theme.stylesheet(false).contains("--agterm-color-15: #00000f"));
    }

    #[test]
    fn an_unknown_palette_gives_the_page_none() {
        let colors = PanelColors::resolve(&TerminalColors::default(), None);
        let theme = colors.theme();
        assert!(theme.palette.is_empty());
        assert_eq!(theme.background, "#282c34", "the window chrome's fallback");
        assert_eq!(theme.foreground, FALLBACK_FOREGROUND);
    }

    #[test]
    fn the_overlay_background_replaces_the_terminal_one_but_not_under_the_strip() {
        let colors = PanelColors::resolve(&terminal(Some("#101010"), None, palette()), Some("fafafa"));
        let theme = colors.theme();
        assert_eq!(theme.background, "#fafafa");
        assert!(!theme.dark, "a light page background reads as a light scheme");
        assert_eq!(colors.terminal, "#101010", "the strip sits on the terminal's");
        let invalid = PanelColors::resolve(&terminal(Some("#101010"), None, Vec::new()), Some("nope"));
        assert_eq!(invalid.theme().background, "#101010");
    }

    #[test]
    fn the_strip_washes_the_terminal_background_with_the_text_color() {
        let colors = PanelColors::resolve(&terminal(Some("#000000"), Some("#ffffff"), Vec::new()), None);
        let wash: gpui_kit::Rgba = colors.strip().into();
        assert!((wash.r - 0.06).abs() < 0.01, "6% of white over black: {wash:?}");
        let ink: gpui_kit::Rgba = colors.ink(0.3).into();
        assert!((ink.a - 0.3).abs() < 0.001);
    }

    #[test]
    fn the_terminal_colors_come_from_the_theme_and_its_ansi_slots() {
        let theme = agterm_term::ThemeColors {
            background: agterm_term::Rgb::new(0x10, 0x20, 0x30),
            foreground: agterm_term::Rgb::new(0xe0, 0xe0, 0xe0),
            cursor: None,
            palette: [(3, agterm_term::Rgb::new(1, 2, 3))].into(),
        };
        let colors = TerminalColors::of(&theme);
        assert_eq!(colors.background.as_deref(), Some("#102030"));
        assert_eq!(colors.foreground.as_deref(), Some("#e0e0e0"));
        assert_eq!(colors.palette.len(), 16);
        assert_eq!(colors.palette[3], "#010203");
    }
}
