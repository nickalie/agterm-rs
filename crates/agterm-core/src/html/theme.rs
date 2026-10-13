//! The terminal theme as a page sees it (upstream's `HtmlOverlayTheme` in `HtmlOverlay.swift`), and the brightness
//! test that tells a dark theme background from a light one (upstream's `ThemeBrightness.isDark`).

use std::fmt::Write;

use crate::model::is_valid_color_hex;

/// The terminal theme as a page sees it (upstream's `HtmlOverlayTheme`). Every page gets it as custom properties
/// (`--agterm-background`, `--agterm-foreground`, `--agterm-color-0` to `15`) that apply nothing until the page uses
/// them; a themed page, one agterm shows from a file, also gets the scheme and text color as its default look. All of
/// it sits at zero specificity. A file page's background is painted behind its transparent web view instead of set in
/// CSS, because a CSS background on `html` would stop an authored `body` background filling the canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlOverlayTheme {
    pub background: String,
    pub foreground: String,
    pub dark: bool,
    /// The 16 ANSI colors by slot, an invalid entry kept empty so the others keep their index; empty when the theme
    /// did not supply exactly 16.
    pub palette: Vec<String>,
}

impl HtmlOverlayTheme {
    /// Takes `#rrggbb` colors; anything else falls back to a plain dark or light pair, since the values end up inside
    /// a stylesheet.
    #[must_use]
    pub fn new(background: &str, foreground: &str, dark: bool, palette: &[String]) -> Self {
        let valid = is_valid_color_hex(background) && is_valid_color_hex(foreground);
        let (background, foreground) = match (valid, dark) {
            (true, _) => (background, foreground),
            (false, true) => ("#1e1e1e", "#d4d4d4"),
            (false, false) => ("#ffffff", "#1e1e1e"),
        };
        let palette = if palette.len() == 16 {
            palette
                .iter()
                .map(|color| {
                    if is_valid_color_hex(color) {
                        color.clone()
                    } else {
                        String::new()
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        Self {
            background: background.to_owned(),
            foreground: foreground.to_owned(),
            dark,
            palette,
        }
    }

    /// The stylesheet a page gets: the custom properties, and for a themed page the scheme and text color too.
    #[must_use]
    pub fn stylesheet(&self, themed: bool) -> String {
        let look = if themed {
            format!(
                "color-scheme: {}; color: {}; ",
                if self.dark { "dark" } else { "light" },
                self.foreground
            )
        } else {
            String::new()
        };
        let mut slots = String::new();
        for (slot, color) in self.palette.iter().enumerate().filter(|(_, color)| !color.is_empty()) {
            let _ = write!(slots, "--agterm-color-{slot}: {color}; ");
        }
        format!(
            ":where(html) {{ {look}--agterm-background: {}; --agterm-foreground: {}; {slots}}}",
            self.background, self.foreground
        )
    }

    /// The script prepending the stylesheet as element `agterm-theme`; it runs at document start, before the page's
    /// own styles.
    #[must_use]
    pub fn script(&self, themed: bool) -> String {
        format!(
            "(() => {{\n  const style = document.createElement('style');\n  style.id = 'agterm-theme';\n  \
             style.textContent = '{}';\n  document.documentElement.prepend(style);\n}})();",
            self.stylesheet(themed)
        )
    }
}

/// Whether an sRGB background (each component 0...1) reads as dark, by Rec. 601 perceived luminance against a 0.5
/// midpoint (upstream's `ThemeBrightness.isDark`). An unknown background should default to dark at the call site.
#[must_use]
pub fn is_dark(red: f64, green: f64, blue: f64) -> bool {
    let luminance = 0.299 * red + 0.587 * green + 0.114 * blue;
    luminance < 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette(count: usize) -> Vec<String> {
        (0..count).map(|slot| format!("#{slot:02x}0000")).collect()
    }

    fn theme(palette: &[String]) -> HtmlOverlayTheme {
        HtmlOverlayTheme::new("#102030", "#e0e0e0", true, palette)
    }

    #[test]
    fn the_theme_stylesheet_yields_to_any_page_rule() {
        let theme = theme(&[]);
        assert_eq!(
            theme.stylesheet(true),
            ":where(html) { color-scheme: dark; color: #e0e0e0; --agterm-background: #102030; \
             --agterm-foreground: #e0e0e0; }"
        );
        assert_eq!(theme.background, "#102030");
        assert!(theme.script(true).contains(&theme.stylesheet(true)));
        assert!(theme.script(true).contains("agterm-theme"));
    }

    #[test]
    fn the_script_prepends_the_stylesheet() {
        let theme = HtmlOverlayTheme::new("#ffffff", "#000000", false, &[]);
        assert_eq!(
            theme.script(true),
            "(() => {\n  const style = document.createElement('style');\n  style.id = 'agterm-theme';\n  \
             style.textContent = ':where(html) { color-scheme: light; color: #000000; --agterm-background: #ffffff; \
             --agterm-foreground: #000000; }';\n  document.documentElement.prepend(style);\n})();"
        );
    }

    #[test]
    fn a_url_page_gets_the_variables_without_the_look() {
        let theme = theme(&palette(16));
        let css = theme.stylesheet(false);
        assert!(!css.contains("color-scheme"));
        assert!(!css.contains(" color:"));
        assert!(css.starts_with(
            ":where(html) { --agterm-background: #102030; --agterm-foreground: #e0e0e0; --agterm-color-0: #000000;"
        ));
        assert!(theme.script(false).contains(&css));
    }

    #[test]
    fn the_palette_keeps_its_ansi_slots() {
        let css = theme(&palette(16)).stylesheet(true);
        assert!(css.contains("--agterm-color-1: #010000; "));
        assert!(css.contains("--agterm-color-15: #0f0000; }"));
    }

    #[test]
    fn an_invalid_palette_entry_omits_only_its_own_slot() {
        let mut entries = palette(16);
        entries[3] = "red; } body { display: none".to_owned();
        let theme = theme(&entries);
        let css = theme.stylesheet(true);
        assert!(!css.contains("--agterm-color-3:"));
        assert!(!css.contains("display"));
        assert!(css.contains("--agterm-color-4: #040000;"));
        assert_eq!(theme.palette.len(), 16);
        assert_eq!(theme.palette[3], "");
    }

    #[test]
    fn a_palette_of_the_wrong_size_omits_every_slot() {
        for count in [0, 8, 17] {
            let theme = theme(&palette(count));
            assert!(!theme.stylesheet(true).contains("--agterm-color-"), "{count}");
            assert!(theme.palette.is_empty(), "{count}");
        }
    }

    #[test]
    fn a_malformed_theme_color_falls_back_to_a_plain_pair() {
        let theme = HtmlOverlayTheme::new("red; } body { display: none", "#ffffff", false, &[]);
        assert_eq!(theme.background, "#ffffff");
        assert_eq!(theme.foreground, "#1e1e1e");
        assert!(!theme.stylesheet(true).contains("display"));
        let dark = HtmlOverlayTheme::new("#000000", "white", true, &[]);
        assert_eq!(
            (dark.background.as_str(), dark.foreground.as_str()),
            ("#1e1e1e", "#d4d4d4")
        );
    }

    /// Upstream's `ThemeBrightnessTests`.
    #[test]
    fn brightness_follows_perceived_luminance() {
        assert!(is_dark(0.0, 0.0, 0.0));
        assert!(!is_dark(1.0, 1.0, 1.0));
        // gruvbox dark hard #1d2021, solarized dark #002b36 → dark; solarized light #fdf6e3 → light
        assert!(is_dark(0.114, 0.125, 0.129));
        assert!(is_dark(0.0, 0.169, 0.212));
        assert!(!is_dark(0.992, 0.965, 0.890));
        // Rec. 601 weights: green 0.587 clears the 0.5 midpoint, red 0.299 and blue 0.114 do not
        assert!(!is_dark(0.0, 1.0, 0.0));
        assert!(is_dark(0.0, 0.0, 1.0));
        assert!(is_dark(1.0, 0.0, 0.0));
        assert!(!is_dark(0.55, 0.55, 0.55));
        assert!(is_dark(0.45, 0.45, 0.45));
    }
}
