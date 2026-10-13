//! Desktop notifications (upstream's `Notifications/NotificationManager.swift`, `notifications.md`) through
//! `GNotification`, which `GLib` delivers over the instance's own session bus (the portal or
//! `org.freedesktop.Notifications`); the Linux glue upstream's `UNUserNotificationCenter` has no counterpart for.
//!
//! The core records each notification (the badge and the `notify` event) and queues its banner; this posts the queued
//! banners, each under its identity `<windowID>:<sessionID>:<pane>`, so a repeat from one pane replaces the last, and
//! a click reveals the pane that fired. Withdrawals are the core's explicit clears, queued with the posts and applied
//! in that order: when a pane of the session takes focus (zoomed or not, the user's doing or the frontend's) and after
//! a successful swap, as upstream's `clearDelivered` call sites. A clear withdraws every pane's identity of that window
//! and session; a session no open window holds gets none, so a closed window's banners stay clickable and reopen it.
//! The badge clears on its own path (a selection, `session.seen`) and leaves the banners. `GNotification` carries no
//! sound, so a batch that posts banners sounds the configured notification sound as the display's alert (Linux has
//! none of upstream's named sounds); the badge and the event never depend on it.

use agterm_core::{NotificationEffect, parse_notification_identity, should_deliver};
use gtk::gio;
use gtk::prelude::*;
use uuid::Uuid;

use crate::state::{self, AppState, Poster};

/// The app action a banner's click activates, with the banner's identity as its target.
const REVEAL_ACTION: &str = "notification-reveal";

/// Where banners go: the application's notification server, or a recorder in tests.
pub trait BannerSink {
    /// Posts a banner under `identity`, replacing one posted under it before.
    fn send(&self, identity: &str, title: &str, body: &str);
    /// Withdraws the banner posted under `identity`, if any.
    fn withdraw(&self, identity: &str);
}

impl BannerSink for adw::Application {
    fn send(&self, identity: &str, title: &str, body: &str) {
        let notification = gio::Notification::new(title);
        notification.set_body(Some(body));
        notification.set_default_action_and_target_value(&format!("app.{REVEAL_ACTION}"), Some(&identity.to_variant()));
        self.send_notification(Some(identity), &notification);
    }

    fn withdraw(&self, identity: &str) {
        self.withdraw_notification(identity);
    }
}

/// Applies the core's banner posts and withdrawals on `sink`, in the order the core queued them.
pub fn deliver(sink: &impl BannerSink, effects: &[NotificationEffect]) {
    for effect in effects {
        match effect {
            NotificationEffect::Post(post) => {
                tracing::info!(session = %post.session, "notify: posting banner");
                sink.send(&post.identity(), &post.title, &post.body);
            }
            NotificationEffect::App { identity, title, body } => {
                tracing::info!(%identity, "notify: posting app banner");
                sink.send(identity, title, body);
            }
            NotificationEffect::Clear { session, .. } => {
                tracing::debug!(%session, "notify: withdrawing the session's delivered banners");
                for identity in effect.withdrawn_identities() {
                    sink.withdraw(&identity);
                }
            }
        }
    }
}

/// The configured notification sound when `effects` post a banner (upstream attaches it to every banner it posts):
/// one alert per batch, however many banners it carries.
#[must_use]
pub fn banner_sound<'a>(
    effects: &[NotificationEffect],
    settings: &'a agterm_core::persist::AppSettings,
) -> Option<&'a str> {
    let posts = effects
        .iter()
        .any(|effect| matches!(effect, NotificationEffect::Post(_) | NotificationEffect::App { .. }));
    settings
        .notification_sound_name
        .as_deref()
        .filter(|name| posts && !name.is_empty())
}

/// Adds the banners' click action to `app`: it reveals the pane the clicked banner names.
pub fn install_actions(app: &adw::Application, poster: Poster) {
    let action = gio::SimpleAction::new(REVEAL_ACTION, Some(glib_string_type()));
    action.connect_activate(move |_, target| {
        let Some(identity) = target.and_then(gtk::glib::Variant::get::<String>) else {
            return;
        };
        state::post(&poster, move |state: &mut AppState| {
            state.reveal_notification(&identity);
        });
    });
    app.add_action(&action);
}

fn glib_string_type() -> &'static gtk::glib::VariantTy {
    gtk::glib::VariantTy::STRING
}

/// Where the keyboard was when a terminal's notification fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    /// The firing pane held its window's keyboard (upstream's first-responder check).
    pub pane: bool,
    /// Its window was the active one (upstream's `NSApp.isActive`).
    pub active: bool,
}

impl AppState {
    /// A terminal's desktop notification, suppressed while its pane holds the keyboard of the active window
    /// (upstream's `shouldDeliver`).
    pub fn terminal_notification(&mut self, session: Uuid, token: &str, title: &str, body: &str, focus: Focus) {
        if !should_deliver(focus.pane, focus.active) {
            tracing::info!(%session, "notify: the firing pane is focused; suppressed");
            return;
        }
        if self.core.terminal_notification(session, token, title, body) {
            self.settle();
        }
    }

    /// A banner was clicked: its pane is revealed (its closed window reopened, even when the session turned out gone);
    /// a malformed identity only leaves the app up.
    pub fn reveal_notification(&mut self, identity: &str) {
        let Some((window, session, pane)) = parse_notification_identity(identity) else {
            return;
        };
        if self.core.reveal_notification(window, session, pane) {
            self.settle();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reveal_action_takes_the_identity_as_a_string() {
        assert_eq!(glib_string_type().as_str(), "s");
        assert_eq!(format!("app.{REVEAL_ACTION}"), "app.notification-reveal");
    }

    #[test]
    fn the_notification_sound_follows_a_posted_banner_only() {
        let banner = NotificationEffect::App {
            identity: "w:s:main".to_owned(),
            title: "t".to_owned(),
            body: "b".to_owned(),
        };
        let mut settings = agterm_core::persist::AppSettings::default();
        assert_eq!(
            banner_sound(std::slice::from_ref(&banner), &settings),
            None,
            "no sound picked"
        );
        settings.notification_sound_name = Some("Glass".to_owned());
        assert_eq!(banner_sound(std::slice::from_ref(&banner), &settings), Some("Glass"));
        assert_eq!(banner_sound(&[], &settings), None, "no banner, no sound");
    }

    /// Records what reaches the notification server, in order.
    #[derive(Default)]
    struct Recorder(std::cell::RefCell<Vec<String>>);

    impl BannerSink for Recorder {
        fn send(&self, identity: &str, _title: &str, body: &str) {
            self.0.borrow_mut().push(format!("send {identity} {body}"));
        }

        fn withdraw(&self, identity: &str) {
            self.0.borrow_mut().push(format!("withdraw {identity}"));
        }
    }

    fn post(window: Uuid, session: Uuid, pane: agterm_core::PaneRole, body: &str) -> NotificationEffect {
        NotificationEffect::Post(agterm_core::NotificationPost {
            window,
            session,
            pane,
            title: "t".to_owned(),
            body: body.to_owned(),
        })
    }

    #[test]
    fn posts_and_withdrawals_reach_the_server_in_order_and_only_for_their_session() {
        use agterm_core::{PaneRole, notification_identity as identity};
        let (window, other_window) = (Uuid::new_v4(), Uuid::new_v4());
        let (session, other) = (Uuid::new_v4(), Uuid::new_v4());
        let effects = [
            post(window, session, PaneRole::Split, "one"),
            post(window, other, PaneRole::Main, "two"),
            post(other_window, session, PaneRole::Main, "elsewhere"),
            NotificationEffect::Clear { window, session },
            post(window, session, PaneRole::Main, "three"),
        ];
        let recorder = Recorder::default();
        deliver(&recorder, &effects);
        let expected = [
            format!("send {} one", identity(window, session, PaneRole::Split)),
            format!("send {} two", identity(window, other, PaneRole::Main)),
            format!("send {} elsewhere", identity(other_window, session, PaneRole::Main)),
            format!("withdraw {}", identity(window, session, PaneRole::Main)),
            format!("withdraw {}", identity(window, session, PaneRole::Split)),
            format!("withdraw {}", identity(window, session, PaneRole::Overlay)),
            format!("send {} three", identity(window, session, PaneRole::Main)),
        ];
        assert_eq!(*recorder.0.borrow(), expected);
    }

    #[test]
    fn posts_alone_withdraw_nothing() {
        let recorder = Recorder::default();
        let (window, session) = (Uuid::new_v4(), Uuid::new_v4());
        deliver(&recorder, &[post(window, session, agterm_core::PaneRole::Main, "b")]);
        deliver(&recorder, &[]);
        deliver(&recorder, &[]);
        assert_eq!(recorder.0.borrow().len(), 1);
        assert!(recorder.0.borrow()[0].starts_with("send "));
    }
}
