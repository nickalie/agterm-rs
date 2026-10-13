//! Notifications: banners, badges, the attention request, the delivery sound and the attention indicator, each
//! independent (upstream's `NotificationsSettingsView`).

use adw::prelude::*;
use agterm_core::model::STANDARD_SOUND_NAMES;
use agterm_core::persist::settings::DockBounce;
use agterm_core::settings_model::{NotificationsChange, SettingsChange};

use super::{Rows, rows};

fn change(change: NotificationsChange) -> SettingsChange {
    SettingsChange::Notifications(change)
}

/// The sound pickers' rows: None, then the standard names (upstream's `StatusSoundPlayer.standardNames`).
pub fn sound_options() -> Vec<(String, Option<String>)> {
    std::iter::once(("None".to_owned(), None))
        .chain(
            STANDARD_SOUND_NAMES
                .iter()
                .map(|name| ((*name).to_owned(), Some((*name).to_owned()))),
        )
        .collect()
}

/// A picked sound previews, like upstream's pickers; this build sounds the display's alert for every name.
pub fn preview_sound(name: Option<&str>) {
    if name.is_some()
        && let Some(display) = gtk::gdk::Display::default()
    {
        display.beep();
    }
}

pub fn page(rows: &mut Rows) -> adw::PreferencesPage {
    let page = rows::page(
        "Notifications",
        "notifications",
        "preferences-system-notifications-symbolic",
    );
    let group = rows::group("Notifications");
    group.add(&rows::switch(
        rows,
        "settings-notifications",
        "Show notification banners",
        |snapshot| snapshot.settings.notifications_enabled.unwrap_or(true),
        |on| change(NotificationsChange::Banners(on)),
    ));
    group.add(&rows::switch(
        rows,
        "settings-notification-badges",
        "Show notification badges",
        |snapshot| snapshot.settings.notification_badge_enabled.unwrap_or(true),
        |on| change(NotificationsChange::Badges(on)),
    ));
    group.add(&rows::picker(
        rows,
        "settings-dock-bounce",
        "Bounce Dock icon",
        &[
            ("None", DockBounce::Off),
            ("Once", DockBounce::Once),
            ("Until focused", DockBounce::UntilFocused),
        ],
        |snapshot| snapshot.settings.effective_dock_bounce(),
        |bounce| change(NotificationsChange::DockBounce(bounce)),
    ));
    group.add(&rows::choice(
        rows,
        "settings-notification-sound",
        "Notification sound",
        |_| sound_options(),
        |snapshot| snapshot.settings.notification_sound_name.clone(),
        |name| {
            preview_sound(name.as_deref());
            change(NotificationsChange::Sound(name))
        },
    ));
    group.add(&rows::switch(
        rows,
        "settings-attention-button",
        "Show attention indicator",
        |snapshot| snapshot.settings.attention_button_enabled.unwrap_or(false),
        |on| change(NotificationsChange::AttentionButton(on)),
    ));
    page.add(&group);
    page
}
