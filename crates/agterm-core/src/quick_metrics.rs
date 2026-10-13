//! The quick-terminal panel's size on the screen it is summoned onto (upstream's `QuickTerminalMetrics.swift`).
//! Host-free so the arithmetic is testable: the frontend owns the monitor and the panel and converts at the call site.

use std::ops::RangeInclusive;

use crate::persist::AppSettings;

/// A size in logical pixels (upstream's `WindowGeometry.Size`, `Double`-backed: a share of a screen is fractional).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelSize {
    pub width: f64,
    pub height: f64,
}

/// The share of the screen the panel takes when the user has set no size of his own.
pub const DEFAULT_SHARE: f64 = 0.9;

/// Where the default share stops growing. A window is already a modest size, but 90% of a large display is a wall of
/// terminal rather than a quick aside.
pub const DEFAULT_MAX_SIZE: PanelSize = PanelSize {
    width: 1100.0,
    height: 700.0,
};

/// The sizes offered in Settings: the panel is summoned and dismissed, so the choice is which of a few shapes it
/// should have, not a value worth tuning by the pixel.
pub const SIZE_PERCENT_CHOICES: [i64; 6] = [40, 50, 60, 70, 80, 90];

/// Bounds for a user-set percentage: the ceiling is [`DEFAULT_SHARE`], the floor roughly what [`DEFAULT_MAX_SIZE`]
/// already yields on a large display, so every offered value is a real choice.
pub const SIZE_PERCENT_RANGE: RangeInclusive<i64> = 40..=90;

/// Bounds a raw percentage to [`SIZE_PERCENT_RANGE`], so a hand-edited `settings.json` cannot produce a panel too small
/// to read or one covering the screen.
#[must_use]
pub fn clamp_size_percent(percent: i64) -> i64 {
    percent.clamp(*SIZE_PERCENT_RANGE.start(), *SIZE_PERCENT_RANGE.end())
}

/// The panel's size on a screen whose usable area measures `visible`; it clamps, so callers pass the stored setting
/// straight through.
///
/// A set percentage REPLACES both the default share and its ceiling rather than raising the ceiling: a size in pixels
/// is what ages badly across displays. `None` keeps the built-in pair, which no single percentage reproduces (the
/// ceiling binds above roughly 1222x780 and the share below it).
#[must_use]
pub fn panel_size(visible: PanelSize, size_percent: Option<i64>) -> PanelSize {
    let Some(percent) = size_percent else {
        return PanelSize {
            width: (visible.width * DEFAULT_SHARE).min(DEFAULT_MAX_SIZE.width),
            height: (visible.height * DEFAULT_SHARE).min(DEFAULT_MAX_SIZE.height),
        };
    };
    // the clamped percentage is at most 90, exact in an f64
    let share = f64::from(u8::try_from(clamp_size_percent(percent)).unwrap_or(90)) / 100.0;
    PanelSize {
        width: visible.width * share,
        height: visible.height * share,
    }
}

impl AppSettings {
    /// The quick-terminal size the panel uses: the stored percentage when it is one of [`SIZE_PERCENT_CHOICES`], else
    /// `None` (the built-in default), so a hand-edited off-grid value falls back rather than producing a size the
    /// Settings picker cannot show (upstream's `effectiveQuickTerminalSizePercent`).
    #[must_use]
    pub fn effective_quick_terminal_size_percent(&self) -> Option<i64> {
        self.quick_terminal_size_percent
            .filter(|percent| SIZE_PERCENT_CHOICES.contains(percent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32" 4K display in logical pixels, the case that motivated the setting.
    const LARGE: PanelSize = PanelSize {
        width: 2560.0,
        height: 1440.0,
    };
    /// A small display, under the roughly 1222x780 where the built-in maximum takes over.
    const SMALL: PanelSize = PanelSize {
        width: 1200.0,
        height: 760.0,
    };

    /// Compared with a tolerance: a share of a screen is a floating-point product (70% of 1440 is 1007.9999…).
    fn assert_size(size: PanelSize, width: f64, height: f64) {
        assert!((size.width - width).abs() < 0.001, "{size:?} vs {width}");
        assert!((size.height - height).abs() < 0.001, "{size:?} vs {height}");
    }

    #[test]
    fn default_caps_a_large_screen_at_the_built_in_maximum() {
        assert_size(panel_size(LARGE, None), 1100.0, 700.0);
    }

    #[test]
    fn default_takes_the_share_when_it_falls_under_the_maximum() {
        assert_size(panel_size(SMALL, None), 1080.0, 684.0);
    }

    #[test]
    fn default_mixes_share_and_maximum_per_axis() {
        let visible = PanelSize {
            width: 2000.0,
            height: 600.0,
        };
        assert_size(panel_size(visible, None), 1100.0, 540.0);
    }

    #[test]
    fn set_percentage_replaces_the_maximum_instead_of_raising_it() {
        assert_size(panel_size(LARGE, Some(70)), 1792.0, 1008.0);
    }

    #[test]
    fn set_percentage_applies_to_a_small_screen_too() {
        assert_size(panel_size(SMALL, Some(50)), 600.0, 380.0);
    }

    /// Both axes, since the claim is that the whole size matches.
    fn choices_matching_the_default(visible: PanelSize) -> Vec<i64> {
        let by_default = panel_size(visible, None);
        SIZE_PERCENT_CHOICES
            .into_iter()
            .filter(|percent| {
                let sized = panel_size(visible, Some(*percent));
                (sized.width - by_default.width).abs() < 0.001 && (sized.height - by_default.height).abs() < 0.001
            })
            .collect()
    }

    #[test]
    fn no_percentage_reproduces_the_default_on_both_screens() {
        assert_eq!(choices_matching_the_default(SMALL), vec![90]);
        assert!(choices_matching_the_default(LARGE).is_empty());
    }

    #[test]
    fn percentages_out_of_range_clamp_to_its_ends() {
        assert_eq!(panel_size(LARGE, Some(400)), panel_size(LARGE, Some(90)));
        assert_eq!(panel_size(LARGE, Some(0)), panel_size(LARGE, Some(40)));
        assert_eq!(clamp_size_percent(-5), 40);
        assert_eq!(clamp_size_percent(1000), 90);
        assert_eq!(clamp_size_percent(70), 70);
    }

    #[test]
    fn every_offered_choice_is_in_range() {
        for percent in SIZE_PERCENT_CHOICES {
            assert_eq!(clamp_size_percent(percent), percent);
        }
    }

    #[test]
    fn the_setting_resolves_off_grid_values_to_the_default() {
        let effective = |percent: Option<i64>| {
            AppSettings {
                quick_terminal_size_percent: percent,
                ..AppSettings::default()
            }
            .effective_quick_terminal_size_percent()
        };
        assert_eq!(effective(Some(70)), Some(70));
        for off_grid in [75, 400, 0] {
            assert_eq!(effective(Some(off_grid)), None, "{off_grid}");
        }
        assert_eq!(effective(None), None);
        for percent in SIZE_PERCENT_CHOICES {
            assert_eq!(effective(Some(percent)), Some(percent));
        }
    }

    #[test]
    fn percentage_is_monotonic_across_the_offered_choices() {
        let widths: Vec<f64> = SIZE_PERCENT_CHOICES
            .into_iter()
            .map(|percent| panel_size(LARGE, Some(percent)).width)
            .collect();
        assert!(widths.windows(2).all(|pair| pair[0] < pair[1]), "{widths:?}");
    }
}
