//! The terminal theme on screen: following the desktop's light/dark appearance through libadwaita's style manager
//! (upstream's `SystemAppearanceObserver.swift` and `SettingsModel.appearanceChanged`), and the config reloads a theme
//! or settings change asks for (`SettingsModel.reloadConfigClearingSessionZoom`).
//!
//! Following uses ghostty's own dual `theme = light:…,dark:…` line: the config text does not change across a flip, so
//! a flip re-feeds the config directly under the new color scheme, keeping every terminal's font zoom. A flip is
//! debounced (upstream's 0.05 s) and acted on only while following with both slots set and when the side differs from
//! the one last applied. Every reload, whichever path took it, records the side it applied and re-themes the pages.

use std::cell::RefCell;
use std::time::Duration;

use gtk::glib;

use crate::state::{self, AppState, Poster};

/// How long a burst of appearance notifications settles before one re-apply (upstream's `appearanceDebounceInterval`).
const APPEARANCE_DEBOUNCE: Duration = Duration::from_millis(50);

thread_local! {
    /// The pending debounced appearance report.
    static PENDING: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
}

/// The side the terminals render, as the last config feed applied it (upstream's `lastAppliedIsDark`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appearance {
    pub last_applied_dark: bool,
}

impl Appearance {
    /// The side the app started on.
    #[must_use]
    pub fn new(dark: bool) -> Self {
        Self {
            last_applied_dark: dark,
        }
    }

    /// Whether a flip to `dark` re-feeds the config: only while following with both slots set (a dual line exists),
    /// and only when the side moved.
    #[must_use]
    pub fn flip_reloads(self, settings: &agterm_core::persist::AppSettings, dark: bool) -> bool {
        settings.follows_system_appearance()
            && settings.theme.is_some()
            && settings.dark_theme.is_some()
            && dark != self.last_applied_dark
    }
}

/// Whether the desktop prefers the dark appearance now.
#[must_use]
pub fn desktop_is_dark() -> bool {
    adw::StyleManager::default().is_dark()
}

/// Follows the desktop's appearance: each settled change is posted to the app state as one report.
pub fn follow_appearance(poster: &Poster) {
    let poster = poster.clone();
    adw::StyleManager::default().connect_dark_notify(move |manager| {
        // the side rides the notification, never re-read when the debounce fires
        let dark = manager.is_dark();
        let poster = poster.clone();
        let source = glib::timeout_add_local_once(APPEARANCE_DEBOUNCE, move || {
            PENDING.with(|pending| pending.borrow_mut().take());
            state::post(&poster, move |state: &mut AppState| state.appearance_changed(dark));
        });
        if let Some(previous) = PENDING.with(|pending| pending.borrow_mut().replace(source)) {
            previous.remove();
        }
    });
}

#[cfg(test)]
mod tests {
    use agterm_core::persist::AppSettings;

    use super::*;

    fn following(theme: Option<&str>, dark: Option<&str>, follow: Option<bool>) -> AppSettings {
        AppSettings {
            theme: theme.map(str::to_owned),
            dark_theme: dark.map(str::to_owned),
            follow_system_appearance: follow,
            ..AppSettings::default()
        }
    }

    #[test]
    fn a_flip_reloads_only_while_following_both_slots_and_on_a_new_side() {
        let light = Appearance::new(false);
        let both = following(Some("Dracula"), Some("Nord"), Some(true));
        assert!(light.flip_reloads(&both, true));
        assert!(!light.flip_reloads(&both, false), "the side did not move");
        assert!(Appearance::new(true).flip_reloads(&both, false));
        for settings in [
            following(Some("Dracula"), Some("Nord"), None),
            following(Some("Dracula"), None, Some(true)),
            following(None, Some("Nord"), Some(true)),
        ] {
            assert!(!light.flip_reloads(&settings, true), "{settings:?}");
        }
    }
}
