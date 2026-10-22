//! The window chrome's colors, host-free: the title bar, the window body and the sidebar read as one surface with the
//! terminal (upstream's `WindowAppearance.sync`, `WindowContentView.sidebarTintWash` and `chromeText`, and the
//! sidebar rows' selection pill, `SidebarRowViews.swift`).
//!
//! The window takes the theme's `background`, the chrome text its `foreground`. The sidebar alone takes a wash of black
//! (darker) or white (lighter) over that background, at the magnitude [`sidebar_shift_amount`] gives the Settings
//! slider; the title strip never does. A selected sidebar row draws the theme's `selection-background`, its text the
//! `selection-foreground`, else the black or white that contrasts with the selection background
//! ([`contrasting_text`]). libghostty's config getter cannot read the optional `selection-*` keys, so
//! [`resolve_selection_colors`] reads them from the config sources and the active theme's file, as upstream's
//! `GhosttyApp.resolveSelectionColors` does.

use std::fmt;

/// An opaque sRGB color, 8 bits a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Self = Self::new(0, 0, 0);
    pub const WHITE: Self = Self::new(255, 255, 255);

    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// A `#rrggbb` or `#rgb` hex color, the `#` optional (upstream's `GhosttyApp.parseHexColor`); `None` for anything
    /// else, such as ghostty's `cell-foreground`.
    #[must_use]
    pub fn from_hex(value: &str) -> Option<Self> {
        let digits = value.strip_prefix('#').unwrap_or(value);
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let wide: String = match digits.len() {
            3 => digits.chars().flat_map(|digit| [digit, digit]).collect(),
            6 => digits.to_owned(),
            _ => return None,
        };
        let channel = |at: usize| u8::from_str_radix(&wide[at..at + 2], 16).ok();
        Some(Self::new(channel(0)?, channel(2)?, channel(4)?))
    }

    /// The channels as 0...1 fractions.
    fn unit(self) -> [f64; 3] {
        [self.r, self.g, self.b].map(|channel| f64::from(channel) / 255.0)
    }

    /// This color under a wash of `white` (lighter) or black (darker) at `opacity` (0...1): what compositing
    /// `Color(white:).opacity(opacity)` over it draws, rounded to 8 bits.
    #[must_use]
    pub fn washed(self, white: bool, opacity: f64) -> Self {
        let opacity = opacity.clamp(0.0, 1.0);
        let added = if white { 255.0 * opacity } else { 0.0 };
        let wash = |channel: u8| to_channel(f64::from(channel) * (1.0 - opacity) + added);
        Self::new(wash(self.r), wash(self.g), wash(self.b))
    }
}

impl fmt::Display for Rgb {
    /// `#rrggbb`, lowercase.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// A channel value in 0...255, rounded and clamped.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0...255 before the cast"
)]
fn to_channel(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// The window color while libghostty reports no background (upstream's `resolvedTerminalColor` fallback,
/// sRGB 0.157/0.173/0.204).
pub const FALLBACK_BACKGROUND: Rgb = Rgb::new(0x28, 0x2c, 0x34);

/// The signed sidebar wash for a Settings slider strength (upstream's `AppSettings.sidebarShiftAmount`): clamped to
/// 0...10 and measured from the neutral 5, so the endpoints are -0.30 (full lighten) and +0.30 (full darken). Positive
/// darkens with a black wash, negative lightens with a white one; the magnitude is the wash's opacity.
#[must_use]
pub fn sidebar_shift_amount(strength: i64) -> f64 {
    // clamped first, so the conversion is exact
    let steps = i32::try_from(strength.clamp(0, 10) - 5).unwrap_or_default();
    f64::from(steps) * 0.06
}

/// Black or white, whichever contrasts better with `color` by WCAG relative luminance (upstream's
/// `GhosttyApp.contrastingText`): gamma-linearized, so a saturated mid-tone such as a system green reads light.
#[must_use]
pub fn contrasting_text(color: Rgb) -> Rgb {
    let linear = |value: f64| {
        if value <= 0.039_28 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b] = color.unit();
    let luminance = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
    // 0.179 is the crossover where contrast against black equals contrast against white
    if luminance > 0.179 { Rgb::BLACK } else { Rgb::WHITE }
}

/// The theme's selection colors as its config sets them; either may be unset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SelectionColors {
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
}

/// The `key = value` pairs of a ghostty-style config text in order, blank and `#` comment lines skipped (upstream's
/// `GhosttyApp.keyValues`).
#[must_use]
pub fn config_key_values(text: &str) -> Vec<(&str, &str)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
        .collect()
}

/// The selection colors the config `sources` (their texts, in load order, last wins) set, the active theme's file
/// filling any one they leave unset (upstream's `GhosttyApp.resolveSelectionColors`). `theme_text` reads a bundled
/// theme's file by name; a `light:…,dark:…` theme picks its side by `is_dark`. Like upstream, the sources'
/// `config-file` includes are not followed.
pub fn resolve_selection_colors<'a>(
    sources: impl IntoIterator<Item = &'a str>,
    theme_text: impl FnOnce(&str) -> Option<String>,
    is_dark: bool,
) -> SelectionColors {
    let mut theme = None;
    let mut colors = SelectionColors::default();
    for (key, value) in sources.into_iter().flat_map(config_key_values) {
        match key {
            "theme" => theme = Some(value.to_owned()),
            "selection-background" => colors.background = Rgb::from_hex(value),
            "selection-foreground" => colors.foreground = Rgb::from_hex(value),
            _ => {}
        }
    }
    let unset = colors.background.is_none() || colors.foreground.is_none();
    let theme = theme
        .filter(|_| unset)
        .map(|value| crate::theme::resolved_for_appearance(&value, is_dark))
        .filter(|name| !name.is_empty());
    if let Some(text) = theme.and_then(|name| theme_text(&name)) {
        for (key, value) in config_key_values(&text) {
            match key {
                "selection-background" if colors.background.is_none() => colors.background = Rgb::from_hex(value),
                "selection-foreground" if colors.foreground.is_none() => colors.foreground = Rgb::from_hex(value),
                _ => {}
            }
        }
    }
    colors
}

/// Everything the chrome is painted with, resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromeColors {
    /// The window body and the title bar: the terminal background.
    pub background: Rgb,
    /// The chrome text: the terminal foreground.
    pub foreground: Rgb,
    /// The sidebar: the background under the tint wash, equal to `background` at the neutral strength.
    pub sidebar: Rgb,
    /// The selected row's pill, `None` for upstream's soft white wash.
    pub selection_background: Option<Rgb>,
    /// The selected row's text.
    pub selection_foreground: Rgb,
}

impl ChromeColors {
    /// Opacity of the selection pill while the window is not the key one (upstream's non-emphasized row).
    pub const INACTIVE_SELECTION_ALPHA: f64 = 0.55;
    /// The soft white wash a selected row draws when the theme sets no selection background, in the key window and
    /// elsewhere (upstream's `SidebarRowView.keyAlpha` and `inactiveAlpha`).
    pub const WASH_ALPHA: f64 = 0.13;
    pub const INACTIVE_WASH_ALPHA: f64 = 0.07;
    /// The opacity of the chrome text the title-bar hairline and the sidebar divider draw with (upstream's
    /// `chromeText.opacity(0.1)`).
    pub const SEPARATOR_ALPHA: f64 = 0.1;

    /// The chrome for the terminal's `background` and `foreground` (as libghostty reports them, `None` when it reports
    /// none), the theme's `selection` colors and the Settings sidebar tint `sidebar_shift` (0...10, 5 neutral).
    ///
    /// Without a background the window takes upstream's fallback. Without a foreground, where upstream falls back to
    /// the system label color, the text takes the black or white that contrasts with the background. The selected
    /// row's text is the selection foreground, else the contrast of the selection background, else white (upstream's
    /// `refreshSelectionColors` and `SidebarCellView.setColors`).
    #[must_use]
    pub fn new(
        background: Option<Rgb>,
        foreground: Option<Rgb>,
        selection: SelectionColors,
        sidebar_shift: i64,
    ) -> Self {
        let background = background.unwrap_or(FALLBACK_BACKGROUND);
        let shift = sidebar_shift_amount(sidebar_shift);
        Self {
            background,
            foreground: foreground.unwrap_or_else(|| contrasting_text(background)),
            sidebar: background.washed(shift < 0.0, shift.abs()),
            selection_background: selection.background,
            selection_foreground: selection
                .foreground
                .or_else(|| selection.background.map(contrasting_text))
                .unwrap_or(Rgb::WHITE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shift_is_signed_and_clamps() {
        // upstream's AppSettingsTests.sidebarShiftAmountIsSignedAndClamps
        assert!(sidebar_shift_amount(5).abs() < f64::EPSILON);
        assert!((sidebar_shift_amount(0) - -0.30).abs() < 1e-9, "full lighten");
        assert!((sidebar_shift_amount(10) - 0.30).abs() < 1e-9, "full darken");
        assert!(sidebar_shift_amount(7) > 0.0, "above center darkens");
        assert!(sidebar_shift_amount(3) < 0.0, "below center lightens");
        assert!((sidebar_shift_amount(-4) - sidebar_shift_amount(0)).abs() < f64::EPSILON);
        assert!((sidebar_shift_amount(99) - sidebar_shift_amount(10)).abs() < f64::EPSILON);
        assert!((sidebar_shift_amount(i64::MIN) - sidebar_shift_amount(0)).abs() < f64::EPSILON);
    }

    #[test]
    fn hex_colors_parse_in_both_lengths() {
        assert_eq!(Rgb::from_hex("#3a5f8c"), Some(Rgb::new(0x3a, 0x5f, 0x8c)));
        assert_eq!(Rgb::from_hex("3A5F8C"), Some(Rgb::new(0x3a, 0x5f, 0x8c)));
        assert_eq!(Rgb::from_hex("#fa0"), Some(Rgb::new(0xff, 0xaa, 0x00)));
        for bad in ["", "#", "#12345", "#1234567", "cell-foreground", "#ggg", "#+12345"] {
            assert_eq!(Rgb::from_hex(bad), None, "{bad}");
        }
        assert_eq!(Rgb::new(0x3a, 0x5f, 0x8c).to_string(), "#3a5f8c");
    }

    #[test]
    fn a_wash_blends_toward_black_or_white() {
        let color = Rgb::new(21, 121, 241);
        assert_eq!(color.washed(false, 0.0), color);
        assert_eq!(color.washed(false, 0.30), Rgb::new(15, 85, 169));
        assert_eq!(color.washed(true, 0.30), Rgb::new(91, 161, 245));
        assert_eq!(color.washed(true, 2.0), Rgb::WHITE, "the opacity clamps");
    }

    #[test]
    fn contrasting_text_uses_relative_luminance() {
        assert_eq!(contrasting_text(Rgb::BLACK), Rgb::WHITE);
        assert_eq!(contrasting_text(Rgb::WHITE), Rgb::BLACK);
        // systemGreen reads light by WCAG luminance though a plain luma average calls it dark
        assert_eq!(contrasting_text(Rgb::new(0x34, 0xc7, 0x59)), Rgb::BLACK);
        assert_eq!(contrasting_text(Rgb::new(0x1e, 0x1e, 0x2e)), Rgb::WHITE);
        // either side of the 0.179 crossover, mid grey (0x76 ≈ 0.181, 0x75 ≈ 0.178)
        assert_eq!(contrasting_text(Rgb::new(0x76, 0x76, 0x76)), Rgb::BLACK);
        assert_eq!(contrasting_text(Rgb::new(0x75, 0x75, 0x75)), Rgb::WHITE);
    }

    #[test]
    fn key_values_skip_comments_and_blank_lines() {
        let text = "# comment\n\n theme = Dracula \nselection-background=#112233\nnot a pair\nkey = a = b\n";
        assert_eq!(
            config_key_values(text),
            [
                ("theme", "Dracula"),
                ("selection-background", "#112233"),
                ("key", "a = b")
            ]
        );
    }

    #[test]
    fn explicit_selection_lines_win_over_the_theme_file() {
        let themes = |name: &str| {
            (name == "Nord").then(|| "selection-background = #445566\nselection-foreground = #ddeeff\n".to_owned())
        };
        let colors = resolve_selection_colors(["theme = Nord", "selection-background = #010203"], themes, false);
        assert_eq!(colors.background, Some(Rgb::new(1, 2, 3)));
        assert_eq!(
            colors.foreground,
            Some(Rgb::new(0xdd, 0xee, 0xff)),
            "the theme fills the unset one"
        );
        // the later source wins
        let later = resolve_selection_colors(
            ["selection-foreground = #000000", "selection-foreground = #fff"],
            |_| None,
            false,
        );
        assert_eq!(later.foreground, Some(Rgb::WHITE));
        assert_eq!(later.background, None);
    }

    #[test]
    fn a_dual_theme_reads_the_side_on_screen() {
        let themes = |name: &str| {
            Some(format!(
                "selection-background = {}\n",
                if name == "Dark" { "#000001" } else { "#fffffe" }
            ))
        };
        let source = ["theme = light:Light,dark:Dark"];
        assert_eq!(
            resolve_selection_colors(source, themes, true).background,
            Some(Rgb::new(0, 0, 1))
        );
        assert_eq!(
            resolve_selection_colors(source, themes, false).background,
            Some(Rgb::new(255, 255, 254))
        );
        // both set explicitly: the theme file is never read
        let both = [
            "theme = Dark",
            "selection-background = #111111",
            "selection-foreground = #222222",
        ];
        let colors = resolve_selection_colors(both, |_| -> Option<String> { unreachable!() }, true);
        assert_eq!(colors.foreground, Some(Rgb::new(0x22, 0x22, 0x22)));
    }

    #[test]
    fn the_chrome_falls_back_like_upstream() {
        let none = ChromeColors::new(None, None, SelectionColors::default(), 5);
        assert_eq!(none.background, FALLBACK_BACKGROUND);
        assert_eq!(none.foreground, Rgb::WHITE);
        assert_eq!(none.sidebar, FALLBACK_BACKGROUND, "neutral: no wash");
        assert_eq!(none.selection_background, None);
        assert_eq!(none.selection_foreground, Rgb::WHITE, "white over the soft wash");
        let bg = Rgb::new(21, 121, 241);
        let only_background = SelectionColors {
            background: Some(Rgb::new(0xee, 0xee, 0xee)),
            foreground: None,
        };
        let chrome = ChromeColors::new(Some(bg), Some(Rgb::new(1, 2, 3)), only_background, 10);
        assert_eq!(chrome.foreground, Rgb::new(1, 2, 3));
        assert_eq!(
            chrome.selection_foreground,
            Rgb::BLACK,
            "the contrast of the selection background"
        );
        assert_eq!(chrome.sidebar, Rgb::new(15, 85, 169), "strength 10: a 0.30 black wash");
        assert_eq!(
            ChromeColors::new(Some(bg), None, SelectionColors::default(), 0).sidebar,
            Rgb::new(91, 161, 245)
        );
    }
}
