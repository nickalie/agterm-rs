//! The terminal's fonts from `font-family*` and `font-feature`, as GPUI fonts for the four text styles.
//!
//! Ghostty's own default (its embedded `JetBrains Mono` and `Symbols Nerd Font Mono`, used when `font-family` is unset)
//! is not embedded in this build yet (plan > Task 15): with no family configured, or none of the configured ones
//! installed, the first installed family of [`DEFAULT_FAMILIES`] is the grid's font. The other families follow as
//! fallbacks for glyphs the primary lacks; GPUI's text system falls back further on its own.

use std::sync::Arc;

use agterm_core::ghostty_config;
use gpui_kit::{Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, font};

/// The monospace families tried, in order, when no configured family is installed: Ghostty's default first.
pub const DEFAULT_FAMILIES: &[&str] = &[
    "JetBrains Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Adwaita Mono",
    "Ubuntu Mono",
    "Bitstream Vera Sans Mono",
    "monospace",
];

/// The fonts of the four text styles.
#[derive(Debug, Clone, PartialEq)]
pub struct Fonts {
    pub regular: Font,
    pub bold: Font,
    pub italic: Font,
    pub bold_italic: Font,
}

impl Fonts {
    /// The fonts for `config`, `installed` telling which families exist.
    #[must_use]
    pub fn resolve(config: &ghostty_config::Font, installed: &dyn Fn(&str) -> bool) -> Self {
        let features = features(&config.feature);
        let regular = family_font(&config.family, installed, &features);
        let styled = |families: &[String], weight: FontWeight, style: FontStyle| {
            let base = if families.iter().any(|family| installed(family)) {
                family_font(families, installed, &features)
            } else {
                // an unset or missing style family is the regular family in that style, as in Ghostty
                regular.clone()
            };
            Font { weight, style, ..base }
        };
        Self {
            bold: styled(&config.family_bold, FontWeight::BOLD, FontStyle::Normal),
            italic: styled(&config.family_italic, FontWeight::NORMAL, FontStyle::Italic),
            bold_italic: styled(&config.family_bold_italic, FontWeight::BOLD, FontStyle::Italic),
            regular,
        }
    }

    /// The font of a text style.
    #[must_use]
    pub fn style(&self, bold: bool, italic: bool) -> &Font {
        match (bold, italic) {
            (false, false) => &self.regular,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (true, true) => &self.bold_italic,
        }
    }
}

/// The first installed of `families` (else of [`DEFAULT_FAMILIES`]) with every other family as a fallback.
fn family_font(families: &[String], installed: &dyn Fn(&str) -> bool, features: &FontFeatures) -> Font {
    let candidates: Vec<String> = families
        .iter()
        .cloned()
        .chain(DEFAULT_FAMILIES.iter().map(|family| (*family).to_owned()))
        .collect();
    let primary = candidates.iter().position(|family| installed(family)).unwrap_or(0);
    let mut fallbacks = candidates.clone();
    let family = fallbacks.remove(primary);
    fallbacks.retain(|fallback| *fallback != family && installed(fallback));
    tracing::debug!(target: "agterm::surface", %family, ?fallbacks, "terminal font");
    Font {
        features: features.clone(),
        fallbacks: (!fallbacks.is_empty()).then(|| FontFallbacks::from_fonts(fallbacks)),
        ..font(family)
    }
}

/// `font-feature` lines as OpenType features: `-calt` off, `calt`/`+calt` on, `calt=N` or `calt N` a value, `on` and
/// `off` as words (Ghostty's feature syntax, as `HarfBuzz` parses it); anything else is skipped.
#[must_use]
pub fn features(lines: &[String]) -> FontFeatures {
    let parsed = lines
        .iter()
        .flat_map(|line| line.split(','))
        .filter_map(|feature| parse_feature(feature.trim()))
        .collect();
    FontFeatures(Arc::new(parsed))
}

fn parse_feature(feature: &str) -> Option<(String, u32)> {
    let (tag, value) = if let Some(tag) = feature.strip_prefix('-') {
        (tag, Some(0))
    } else if let Some(tag) = feature.strip_prefix('+') {
        (tag, Some(1))
    } else {
        (feature, None)
    };
    let (tag, value) = match value {
        Some(value) => (tag, value),
        None => match tag.split_once(['=', ' ']) {
            Some((tag, value)) => (tag.trim(), feature_value(value.trim())?),
            None => (tag, 1),
        },
    };
    (tag.len() == 4 && tag.chars().all(|ch| ch.is_ascii_graphic())).then(|| (tag.to_owned(), value))
}

fn feature_value(value: &str) -> Option<u32> {
    match value {
        "on" | "true" => Some(1),
        "off" | "false" => Some(0),
        number => number.parse().ok(),
    }
}
