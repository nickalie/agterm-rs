//! Desktop notifications (upstream's `Notifications/NotificationManager.swift`, `notifications.md`) through
//! `GNotification`, which `GLib` delivers over the instance's own session bus (the portal or
//! `org.freedesktop.Notifications`); the Linux glue upstream's `UNUserNotificationCenter` has no counterpart for.
//!
//! The core records each notification (the badge and the `notify` event) and queues its banner; this posts the queued
//! banners, each under its identity `<windowID>:<sessionID>:<pane>`, so a repeat from one pane replaces the last, and
//! a click reveals the pane that fired. A session's delivered banners are withdrawn once its badge clears (selected,
//! seen, or its pane focused), as upstream's `clearDelivered`. `GNotification` carries no sound, so the configured
//! notification sound is not played; the badge and the event never depend on it.

use std::collections::{BTreeSet, HashMap};

use agterm_core::{Core, NotificationPost, parse_notification_identity, should_deliver};
use gtk::gio;
use gtk::prelude::*;
use uuid::Uuid;

use crate::state::{self, AppState, Poster};

/// The app action a banner's click activates, with the banner's identity as its target.
const REVEAL_ACTION: &str = "notification-reveal";

/// The banners delivered and not withdrawn yet, by session.
#[derive(Debug, Default)]
pub struct Notifications {
    delivered: HashMap<Uuid, BTreeSet<String>>,
}

impl Notifications {
    /// Posts `posts` on `app`, then withdraws the banners of every session whose badge is clear or that is gone.
    pub fn deliver(&mut self, app: &impl IsA<gio::Application>, posts: &[NotificationPost], core: &Core) {
        for post in posts {
            let identity = post.identity();
            let notification = gio::Notification::new(&post.title);
            notification.set_body(Some(&post.body));
            notification
                .set_default_action_and_target_value(&format!("app.{REVEAL_ACTION}"), Some(&identity.to_variant()));
            tracing::info!(session = %post.session, "notify: posting banner");
            app.send_notification(Some(&identity), &notification);
            self.delivered.entry(post.session).or_default().insert(identity);
        }
        self.withdraw_seen(app, core);
    }

    /// Withdraws the banners of sessions whose badge is clear, or that are gone.
    fn withdraw_seen(&mut self, app: &impl IsA<gio::Application>, core: &Core) {
        let seen: Vec<Uuid> = self
            .delivered
            .keys()
            .copied()
            .filter(|session| unseen(core, *session) == 0)
            .collect();
        for session in seen {
            for identity in self.delivered.remove(&session).unwrap_or_default() {
                app.withdraw_notification(&identity);
            }
        }
    }
}

/// Session `session`'s unseen count, 0 when it is gone.
fn unseen(core: &Core, session: Uuid) -> usize {
    core.library()
        .window_holding_session(session)
        .and_then(|window| core.library().store(window))
        .and_then(|store| store.session(session))
        .map_or(0, |found| found.unseen_count)
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
}
