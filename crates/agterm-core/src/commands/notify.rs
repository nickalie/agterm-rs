//! `notify` and terminal desktop notifications (OSC 9/777): the badge, the `notify` event and the banner the frontend
//! posts (upstream's `ControlDispatcher` `.notify` arm, `ControlServer.sendNotification`, `NotificationManager.notify`
//! and `send(toSession:)`, `Notifications.swift`, `notifications.md`).
//!
//! The core records the event and bumps the badge; the banner itself is the frontend's ([`Core::take_notifications`]),
//! gated by the "Show notification banners" setting. Withdrawing delivered banners is edge-triggered, as upstream's
//! `clearDelivered`: the core queues a [`NotificationEffect::Clear`] in the same queue as the posts at each site that
//! clears them (a pane taking focus, zoomed or not, and a successful swap), and only for a session an open window
//! holds. The badge clears on its own path (a selection, `session.seen`), which leaves the banners alone.

use agterm_proto::{ControlResult, Request};
use uuid::Uuid;

use super::{Outcome, args_of, with_id};
use crate::Core;
use crate::error::ControlError;
use crate::model::StatusPane;
use crate::pane_env::uuid_string;
use crate::resolve::Resolver;

/// `result.text` when banners are off (upstream's `ControlNotify.bannersOffNote`).
pub const BANNERS_OFF_NOTE: &str = "badge updated, but \"Show notification banners\" is off, so no banner was posted";

/// Which surface of a session fired a notification (upstream's `PaneRole`), part of the banner's identity so a click
/// reveals that pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneRole {
    Main,
    Split,
    Overlay,
}

impl PaneRole {
    pub const ALL: [Self; 3] = [Self::Main, Self::Split, Self::Overlay];

    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Split => "split",
            Self::Overlay => "overlay",
        }
    }

    #[must_use]
    pub fn from_wire(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.wire_name() == raw)
    }
}

/// A banner's identity, `<windowID>:<sessionID>:<paneRole>` (upstream's `TerminalNotification.identity`): repeats from
/// one pane replace each other, and a click decodes its target from it.
#[must_use]
pub fn identity(window: Uuid, session: Uuid, pane: PaneRole) -> String {
    format!("{}:{}:{}", uuid_string(window), uuid_string(session), pane.wire_name())
}

/// The parts of an [`identity`]; `None` for a malformed one (upstream's `parseIdentity`).
#[must_use]
pub fn parse_identity(identity: &str) -> Option<(Uuid, Uuid, PaneRole)> {
    let parts: Vec<&str> = identity.split(':').collect();
    let [window, session, pane] = parts.as_slice() else {
        return None;
    };
    Some((
        Uuid::parse_str(window).ok()?,
        Uuid::parse_str(session).ok()?,
        PaneRole::from_wire(pane)?,
    ))
}

/// Whether a terminal notification is delivered: suppressed only while the firing pane holds the keyboard in the
/// active app (upstream's `shouldDeliver`).
#[must_use]
pub fn should_deliver(firing_is_focused: bool, app_active: bool) -> bool {
    !(firing_is_focused && app_active)
}

/// A banner for the frontend to post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPost {
    pub window: Uuid,
    pub session: Uuid,
    pub pane: PaneRole,
    /// The effective title: the caller's, else the session's name.
    pub title: String,
    pub body: String,
}

impl NotificationPost {
    /// The banner's [`identity`].
    #[must_use]
    pub fn identity(&self) -> String {
        identity(self.window, self.session, self.pane)
    }
}

/// What the frontend does with banners, queued in order so a post and a withdrawal are never reordered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationEffect {
    /// Post a banner.
    Post(NotificationPost),
    /// Withdraw every pane's delivered banner of session `session` in open window `window` (upstream's
    /// `clearDelivered`).
    Clear { window: Uuid, session: Uuid },
    /// Post an app-level banner tied to no pane (a keymap or hooks problem, a failed custom command or hook), under
    /// `identity`, which coalesces repeats.
    App {
        identity: String,
        title: String,
        body: String,
    },
}

impl NotificationEffect {
    /// The identities the effect withdraws: every pane's for a clear, none for a post.
    #[must_use]
    pub fn withdrawn_identities(&self) -> Vec<String> {
        match self {
            Self::Post(_) | Self::App { .. } => Vec::new(),
            Self::Clear { window, session } => PaneRole::ALL
                .into_iter()
                .map(|pane| identity(*window, *session, pane))
                .collect(),
        }
    }
}

/// `notify`: a body is required, checked before the target; the badge always counts it and the banner obeys the
/// setting, whose being off the result notes.
pub(crate) fn dispatch(core: &mut Core, request: &Request) -> Outcome {
    let args = args_of(request);
    let body = args
        .body
        .as_deref()
        .filter(|body| !body.is_empty())
        .ok_or_else(|| ControlError::rejected("notify requires a body"))?;
    let resolved = Resolver::new(core.library()).session(request.target.as_deref(), args.window.as_deref())?;
    let title = args.title.as_deref().unwrap_or("");
    if !core.deliver_notification(resolved.window, resolved.session, PaneRole::Main, title, body) {
        return Err(ControlError::rejected("session's window is not open"));
    }
    let mut result = with_id(resolved.session);
    if !core.banners_enabled() {
        result = ControlResult {
            text: Some(BANNERS_OFF_NOTE.to_owned()),
            ..result
        };
    }
    Ok(result)
}

impl Core {
    /// Whether banners are posted (the "Show notification banners" setting, on by default).
    #[must_use]
    pub fn banners_enabled(&self) -> bool {
        self.settings().notifications_enabled.unwrap_or(true)
    }

    /// Records a notification on session `session` of open window `window` and, with banners on, queues its banner:
    /// the `notify` event, the badge, the post. False when the window or session is gone.
    fn deliver_notification(&mut self, window: Uuid, session: Uuid, pane: PaneRole, title: &str, body: &str) -> bool {
        let Some(title) = self
            .library_mut()
            .store_mut(window)
            .and_then(|store| store.record_notification(session, title, body))
        else {
            return false;
        };
        if self.banners_enabled() {
            self.notifications.push(NotificationEffect::Post(NotificationPost {
                window,
                session,
                pane,
                title,
                body: body.to_owned(),
            }));
        } else {
            tracing::info!(%session, "notify: badge bumped, but banners are off in the settings");
        }
        true
    }

    /// A terminal's OSC 9/777 desktop notification from the pane carrying `token` of session `session` (upstream's
    /// `NotificationManager.notify`). The frontend has already applied [`should_deliver`]. The title falls back to the
    /// session's name. Whether it was recorded; commit afterwards.
    pub fn terminal_notification(&mut self, session: Uuid, token: &str, title: &str, body: &str) -> bool {
        let Some(window) = self.library().window_holding_session(session) else {
            return false;
        };
        let pane = match self
            .library()
            .store(window)
            .and_then(|store| store.session(session))
            .and_then(|found| found.pane_role_for_token(token))
        {
            Some(StatusPane::Right) => PaneRole::Split,
            _ => PaneRole::Main,
        };
        self.deliver_notification(window, session, pane, title, body)
    }

    /// A banner was clicked (upstream's `AppActions.reveal`): the session is revealed in the open window holding it,
    /// else window `window` is opened again when it was closed since. The owning window leaves its zoom (the selection
    /// would change behind it), the session is selected and its badge cleared, the pane that fired is asked to take the
    /// keyboard (its banners are withdrawn only once the frontend gives it focus, which a cover may block)
    /// (the primary when the session no longer has a split), and the window comes to the front. Whether the split
    /// survives is the model's `split.exists` (upstream's `hasSplit`), not whether its terminal is mounted yet: a
    /// reopened window's split, or a restored hidden one, has none until the frontend lays it out, and the focus
    /// request takes it once it does. A closed window is reopened even when the session turns out gone, as upstream's.
    /// Whether anything changed; commit afterwards.
    pub fn reveal_notification(&mut self, window: Uuid, session: Uuid, pane: PaneRole) -> bool {
        let holder = self.library().window_id_for_session(session);
        let mut reopened = false;
        if holder.is_none() && !self.library().is_open(window) {
            self.open_window(window);
            reopened = self.library().is_open(window);
        }
        let window = holder.unwrap_or(window);
        if self
            .library()
            .store(window)
            .and_then(|store| store.session(session))
            .is_none()
        {
            return reopened;
        }
        self.set_zoom_target(window, None);
        self.note_window_activity(window);
        let Some(store) = self.library_mut().store_mut(window) else {
            return reopened;
        };
        let Some(found) = store.session(session) else {
            return reopened;
        };
        let split = pane == PaneRole::Split && found.split.exists;
        store.select_session(Some(session));
        store.clear_unseen(session);
        store.set_split_focus(session, split);
        self.request_pane_focus(session);
        self.take_frontmost(window);
        self.request_window(crate::windows::WindowRequest::Raise(window));
        true
    }

    /// Queues the withdrawal of session `session`'s delivered banners, every pane's (upstream's `clearDelivered`): a
    /// no-op when no open window holds the session, so a closed window's banners stay clickable. The badge is left
    /// as it is. Whether a withdrawal was queued; settle afterwards when it was.
    pub(crate) fn clear_delivered(&mut self, session: Uuid) -> bool {
        let Some(window) = self.library().window_id_for_session(session) else {
            tracing::debug!(%session, "notify: no open window holds the session; no banner to clear");
            return false;
        };
        self.notifications.push(NotificationEffect::Clear { window, session });
        true
    }

    /// Queues an app-level banner when banners are on (upstream's `NotificationManager` app-level posts, each guarded
    /// by `bannersEnabled`).
    pub(crate) fn post_app_banner(&mut self, identity: impl Into<String>, title: &str, body: impl Into<String>) {
        if !self.banners_enabled() {
            return;
        }
        self.notifications.push(NotificationEffect::App {
            identity: identity.into(),
            title: title.to_owned(),
            body: body.into(),
        });
    }

    /// Takes the banner posts and withdrawals queued since the last take, in order.
    pub fn take_notifications(&mut self) -> Vec<NotificationEffect> {
        std::mem::take(&mut self.notifications)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_round_trip_and_malformed_ones_are_refused() {
        let (window, session) = (Uuid::new_v4(), Uuid::new_v4());
        for pane in PaneRole::ALL {
            let text = identity(window, session, pane);
            assert_eq!(parse_identity(&text), Some((window, session, pane)));
        }
        assert_eq!(
            identity(window, session, PaneRole::Split),
            format!("{}:{}:split", uuid_string(window), uuid_string(session))
        );
        for bad in [
            "",
            "a:b:main",
            &format!("{}:{}", uuid_string(window), uuid_string(session)),
        ] {
            assert_eq!(parse_identity(bad), None, "{bad}");
        }
        let extra = format!("{}:{}:main:x", uuid_string(window), uuid_string(session));
        assert_eq!(parse_identity(&extra), None);
        let role = format!("{}:{}:scratch", uuid_string(window), uuid_string(session));
        assert_eq!(parse_identity(&role), None);
    }

    #[test]
    fn only_the_focused_pane_of_the_active_app_suppresses() {
        assert!(!should_deliver(true, true));
        assert!(should_deliver(true, false));
        assert!(should_deliver(false, true));
        assert!(should_deliver(false, false));
    }
}
