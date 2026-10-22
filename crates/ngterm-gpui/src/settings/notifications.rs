//! Notifications: banners, badges, the attention request, the delivery sound and the attention indicator, each
//! independent (upstream's `NotificationsSettingsView`).

use ngterm_core::model::STANDARD_SOUND_NAMES;
use ngterm_core::persist::settings::DockBounce;
use ngterm_core::settings_model::{NotificationsChange, SettingsChange};

use super::Snapshot;
use super::form::{Control, Item, Picker, Section, picker, toggle};

fn change(change: NotificationsChange) -> SettingsChange {
    SettingsChange::Notifications(change)
}

/// The sound pickers' rows: None, then the standard names (upstream's `StatusSoundPlayer.standardNames`).
#[must_use]
pub fn sound_options() -> Vec<(String, Option<String>)> {
    std::iter::once(("None".to_owned(), None))
        .chain(
            STANDARD_SOUND_NAMES
                .iter()
                .map(|name| ((*name).to_owned(), Some((*name).to_owned()))),
        )
        .collect()
}

/// A sound picker over `current`; a picked sound previews, as upstream's pickers do (this build sounds the display's
/// alert for every name).
pub fn sound_picker(
    id: &str,
    title: &str,
    current: Option<&String>,
    set: impl Fn(Option<String>) -> SettingsChange + 'static,
) -> Item {
    let mut picker = Picker::new(sound_options(), &current.cloned(), set);
    picker.previews_sound = true;
    Item::new(id, title, Control::Picker(picker))
}

/// The page's sections.
#[must_use]
pub fn sections(snapshot: &Snapshot) -> Vec<Section> {
    let settings = &snapshot.settings;
    vec![Section::new(
        "Notifications",
        vec![
            toggle(
                "settings-notifications",
                "Show notification banners",
                settings.notifications_enabled.unwrap_or(true),
                |on| change(NotificationsChange::Banners(on)),
            ),
            toggle(
                "settings-notification-badges",
                "Show notification badges",
                settings.notification_badge_enabled.unwrap_or(true),
                |on| change(NotificationsChange::Badges(on)),
            ),
            picker(
                "settings-dock-bounce",
                "Bounce Dock icon",
                &[
                    ("None", DockBounce::Off),
                    ("Once", DockBounce::Once),
                    ("Until focused", DockBounce::UntilFocused),
                ],
                &settings.effective_dock_bounce(),
                |bounce| change(NotificationsChange::DockBounce(bounce)),
            ),
            sound_picker(
                "settings-notification-sound",
                "Notification sound",
                settings.notification_sound_name.as_ref(),
                |name| change(NotificationsChange::Sound(name)),
            ),
            toggle(
                "settings-attention-button",
                "Show attention indicator",
                settings.attention_button_enabled.unwrap_or(false),
                |on| change(NotificationsChange::AttentionButton(on)),
            ),
        ],
    )]
}
