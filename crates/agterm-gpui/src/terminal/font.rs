//! The terminal's fonts from `font-family*` and `font-feature`, as GPUI fonts for the four text styles.
//!
//! Ghostty's embedded fonts are embedded here too (plan > Task 15; `scripts/fetch-fonts.sh` fetches the package deps
//! of the Ghostty at `LIBGHOSTTY_VT_REV`, the build script points `include_bytes!` at them) and registered with the
//! app's text system at start ([`register_embedded`]): `JetBrains Mono`, the grid's font when `font-family` is unset or
//! none of its families is installed, and `Symbols Nerd Font Mono`, every family's symbol fallback (Nerd Font icons,
//! the lock cursor). The configured families after the first installed one are fallbacks too; GPUI's text system
//! falls back further on its own.
//!
//! Ghostty embeds the variable `JetBrains Mono` faces and sets the bold weight as a variation; GPUI loads every face at
//! its default instance (`get_font(id, Weight::NORMAL)`), so this build embeds the same release's static Regular,
//! Bold, Italic and Bold Italic faces instead (the same design at the same weights). It embeds the one-cell `Mono`
//! variant of the symbols font, where Ghostty scales the wide one into the cell with its Nerd Font constraints, which
//! GPUI has not. A configured family is looked up among the installed ones, as Ghostty's discovery does, so the
//! embedded `JetBrains Mono` is registered only where none is installed: a configured `font-family = JetBrains Mono`
//! keeps the system's.

use std::borrow::Cow;
use std::sync::Arc;

use agterm_core::ghostty_config;
use gpui_kit::{Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, TextSystem, font};

#[cfg(test)]
mod tests;

/// Ghostty's default face, embedded.
pub const DEFAULT_FAMILY: &str = "JetBrains Mono";

/// Ghostty's symbol fallback, embedded; every family falls back to it.
pub const SYMBOLS_FAMILY: &str = "Symbols Nerd Font Mono";

/// The families tried, in order, when no configured family is installed.
pub const DEFAULT_FAMILIES: &[&str] = &[DEFAULT_FAMILY];

macro_rules! embedded_font {
    ($file:literal) => {
        include_bytes!(concat!(env!("AGTERM_GPUI_FONTS"), "/", $file)).as_slice()
    };
}

/// `JetBrains Mono` 2.304's static faces: regular, bold, italic, bold italic.
const JETBRAINS_MONO: [&[u8]; 4] = [
    embedded_font!("JetBrainsMono-Regular.ttf"),
    embedded_font!("JetBrainsMono-Bold.ttf"),
    embedded_font!("JetBrainsMono-Italic.ttf"),
    embedded_font!("JetBrainsMono-BoldItalic.ttf"),
];

/// Symbols Nerd Font Mono 3.4.0.
const SYMBOLS: &[u8] = embedded_font!("SymbolsNerdFontMono-Regular.ttf");

/// The embedded faces to register when `installed` lists the families the system has: the symbols font always,
/// `JetBrains Mono` unless the system has its own.
#[must_use]
pub fn embedded_faces(installed: &[String]) -> Vec<&'static [u8]> {
    let mut faces = vec![SYMBOLS];
    if !installed.iter().any(|family| family == DEFAULT_FAMILY) {
        faces.extend(JETBRAINS_MONO);
    }
    faces
}

/// Registers the embedded fonts with the app's text system; call once, before the first terminal resolves its fonts.
pub fn register_embedded(text: &TextSystem) {
    let faces = embedded_faces(&text.all_font_names());
    let count = faces.len();
    match text.add_fonts(faces.into_iter().map(Cow::Borrowed).collect()) {
        Ok(()) => tracing::debug!(target: "agterm::surface", count, "registered the embedded terminal fonts"),
        Err(error) => tracing::warn!(%error, "the embedded terminal fonts are not registered"),
    }
}

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

/// The first installed of `families` (else of [`DEFAULT_FAMILIES`]) with every other installed family and then the
/// symbols font as fallbacks.
fn family_font(families: &[String], installed: &dyn Fn(&str) -> bool, features: &FontFeatures) -> Font {
    let candidates: Vec<String> = families
        .iter()
        .cloned()
        .chain(DEFAULT_FAMILIES.iter().map(|family| (*family).to_owned()))
        .collect();
    let primary = candidates.iter().position(|family| installed(family)).unwrap_or(0);
    let family = candidates[primary].clone();
    let mut fallbacks: Vec<String> = Vec::new();
    for fallback in candidates.into_iter().chain([SYMBOLS_FAMILY.to_owned()]) {
        if fallback != family && installed(&fallback) && !fallbacks.contains(&fallback) {
            fallbacks.push(fallback);
        }
    }
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
