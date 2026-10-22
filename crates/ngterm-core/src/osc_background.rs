//! What a terminal does with a dynamic background color libghostty reports (`GHOSTTY_ACTION_COLOR_CHANGE`, kind
//! background), upstream's `OSCBackgroundPolicy.swift`. A set (OSC 11) and a reset (OSC 111) arrive through the same
//! action, a reset carrying the terminal's default background; so the color the surface's config seeded that default
//! from identifies a reset. Host-free; the frontend owns the config push ([`crate::watermark::osc_background_overlay_text`]).

use crate::model::is_valid_color_hex;

/// What the surface does with a reported color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Render this color as the surface's dynamic background.
    Apply(String),
    /// The program reset to the default: drop the OSC overlay, back to the surface's own config.
    Reset,
    /// Nothing to do: a re-emit of the applied color, a reset with no overlay live, or a malformed color.
    Ignore,
}

/// Routes a reported `incoming` color against the `baseline` the surface's default layer holds (see [`baseline`];
/// `None` before the first config resolve) and the color currently applied (`current`). Colors compare without case
/// or a leading `#`.
#[must_use]
pub fn decide(incoming: &str, baseline: Option<&str>, current: Option<&str>) -> Decision {
    if !is_valid_color_hex(incoming) {
        return Decision::Ignore;
    }
    let color = normalized(incoming);
    if baseline.is_some_and(|baseline| normalized(baseline) == color) {
        return if current.is_none() {
            Decision::Ignore
        } else {
            Decision::Reset
        };
    }
    // a shell re-asserting OSC 11 on every prompt must not rebuild the surface config each time
    if current.is_some_and(|current| normalized(current) == color) {
        return Decision::Ignore;
    }
    Decision::Apply(incoming.to_owned())
}

/// The color a reported change must equal to count as a reset: the surface's own config color (a color session
/// background), else the theme's. While an OSC overlay is installed the config in force is the base one, whose
/// default is the theme's, so the surface's color would make a reset look like a set.
#[must_use]
pub fn baseline<'a>(
    osc_overlay_active: bool,
    surface_background: Option<&'a str>,
    theme_background: Option<&'a str>,
) -> Option<&'a str> {
    if osc_overlay_active {
        theme_background
    } else {
        surface_background.or(theme_background)
    }
}

/// A hex color reduced to its six lowercase digits.
fn normalized(hex: &str) -> String {
    let trimmed = hex.trim();
    trimmed.strip_prefix('#').unwrap_or(trimmed).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: Option<&str> = Some("#1d1f21");

    #[test]
    fn a_first_or_different_color_applies_and_a_repeat_is_deduped() {
        assert_eq!(decide("#123456", THEME, None), Decision::Apply("#123456".to_owned()));
        assert_eq!(
            decide("#abcdef", THEME, Some("#123456")),
            Decision::Apply("#abcdef".to_owned())
        );
        assert_eq!(decide("#123456", THEME, Some("#123456")), Decision::Ignore);
    }

    #[test]
    fn a_reset_to_the_theme_releases_only_a_live_overlay() {
        assert_eq!(decide("#1d1f21", THEME, Some("#123456")), Decision::Reset);
        assert_eq!(decide("#1d1f21", THEME, None), Decision::Ignore);
    }

    #[test]
    fn comparison_ignores_case_and_the_leading_hash() {
        assert_eq!(decide("#1d1f21", Some("#1D1F21"), Some("#123456")), Decision::Reset);
        assert_eq!(decide("#1d1f21", Some("1D1F21"), Some("#123456")), Decision::Reset);
        assert_eq!(decide("#ABCDEF", THEME, Some("#abcdef")), Decision::Ignore);
    }

    #[test]
    fn an_unknown_theme_still_dedupes_and_applies() {
        assert_eq!(decide("#123456", None, None), Decision::Apply("#123456".to_owned()));
        assert_eq!(decide("#123456", None, Some("#123456")), Decision::Ignore);
    }

    #[test]
    fn a_malformed_color_is_ignored() {
        for hex in ["", "#12345", "#12345g", "nonsense", "＃ＦＦ００００"] {
            assert_eq!(decide(hex, THEME, None), Decision::Ignore, "{hex}");
        }
    }

    #[test]
    fn the_baseline_is_the_surface_color_unless_an_overlay_is_installed() {
        assert_eq!(baseline(false, Some("#aa0000"), THEME), Some("#aa0000"));
        assert_eq!(baseline(true, Some("#aa0000"), THEME), THEME);
        assert_eq!(baseline(false, None, THEME), THEME);
        assert_eq!(baseline(false, None, None), None);
        // a reset on a colored surface with the overlay installed routes to reset, not apply
        assert_eq!(
            decide("#1d1f21", baseline(true, Some("#aa0000"), THEME), Some("#123456")),
            Decision::Reset
        );
    }
}
