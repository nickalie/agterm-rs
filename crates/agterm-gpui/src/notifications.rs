//! Desktop notifications (upstream's `Notifications/NotificationManager.swift`, `notifications.md`; port of the GTK
//! build's `notifications.rs`, which posted through `GNotification`) over the freedesktop notification server
//! (`org.freedesktop.Notifications` on the session bus) through `notify-rust`.
//!
//! The core records each notification (the badge and the `notify` event) and queues its banner; this posts the queued
//! banners, each under its identity `<windowID>:<sessionID>:<pane>`: a repeat from one pane replaces the last one
//! (`replaces_id`), and a click on a banner (its `default` action, `ActionInvoked`) reveals the pane that fired.
//! Withdrawals are the core's explicit clears, queued with the posts and applied in that order
//! (`CloseNotification`): when a pane of the session takes focus and after a successful swap, as upstream's
//! `clearDelivered` call sites. A clear withdraws every pane's identity of that window and session; a session no open
//! window holds gets none, so a closed window's banners stay clickable and reopen it. A banner the server closes (it
//! expired, the user dismissed it) is forgotten. The badge clears on its own path and leaves the banners.
//!
//! The server sees no sound: a batch that posts banners sounds the configured notification sound as the display's
//! alert (Linux has none of upstream's named sounds), through the status sounds' throttled bell
//! ([`crate::status_sound`]); the badge and the event never depend on it.
//!
//! The banners go out in order on one task ([`run`]), each in its own `notify-rust` handle (a connection of its own,
//! which is where the server's signals for that banner arrive), and each delivered banner's click is waited for
//! beside it.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::state::{AppState, Poster, post};
use agterm_core::{NotificationEffect, parse_notification_identity};
use futures_util::future::{AbortHandle, Abortable, Either};
use futures_util::stream::{FuturesUnordered, StreamExt as _};
use notify_rust::{Hint, Notification, NotificationHandle, NotificationResponse};

/// The application name banners carry (the server's grouping; GTK's `g_get_application_name`).
pub const APP_NAME: &str = "agterm";

/// The action key a click on a banner's body invokes.
const DEFAULT_ACTION: &str = "default";

/// Where banners go: the notification server, or a recorder in tests.
pub trait BannerSink {
    /// Posts a banner under `identity`, replacing one posted under it before.
    fn send(&self, identity: &str, title: &str, body: &str);
    /// Withdraws the banner posted under `identity`, if any.
    fn withdraw(&self, identity: &str);
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

/// What the banner task does next, in the order the core asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Banner {
    Send {
        identity: String,
        title: String,
        body: String,
    },
    Withdraw {
        identity: String,
    },
}

/// The notification server's side of [`BannerSink`]: the banners queue for [`run`].
pub struct Desktop {
    banners: async_channel::Sender<Banner>,
}

impl Desktop {
    /// A sink whose banners [`run`] posts, and the queue to hand it.
    #[must_use]
    pub fn new() -> (Self, async_channel::Receiver<Banner>) {
        let (banners, queue) = async_channel::unbounded();
        (Self { banners }, queue)
    }

    /// Posts the banners on `executor`, a click revealing its pane through `poster`, under the desktop entry `app_id`.
    #[must_use]
    pub fn start(app_id: String, poster: Poster, executor: &gpui_kit::BackgroundExecutor) -> Self {
        let (sink, queue) = Self::new();
        let clicked = move |identity: String| {
            post(&poster, move |state: &mut AppState| {
                state.reveal_notification(&identity);
            });
        };
        executor.spawn(run(queue, app_id, clicked)).detach();
        sink
    }

    fn queue(&self, banner: Banner) {
        if self.banners.try_send(banner).is_err() {
            tracing::debug!("notify: the banner task is gone");
        }
    }
}

impl BannerSink for Desktop {
    fn send(&self, identity: &str, title: &str, body: &str) {
        self.queue(Banner::Send {
            identity: identity.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
        });
    }

    fn withdraw(&self, identity: &str) {
        self.queue(Banner::Withdraw {
            identity: identity.to_owned(),
        });
    }
}

/// A banner on screen: which delivery it is, its handle (its server id, the connection its signals reach) and the
/// wait for its click, aborted when it is replaced or withdrawn.
struct Delivered {
    serial: u64,
    handle: Arc<NotificationHandle>,
    wait: AbortHandle,
}

/// How a banner's wait ended: clicked, or closed by the server (expired, dismissed), for delivery `serial` of
/// `identity`.
struct Ended {
    identity: String,
    serial: u64,
    clicked: bool,
}

type Wait = Pin<Box<dyn Future<Output = Option<Ended>> + Send>>;

/// The banner task: posts and withdraws `banners` in order, each identity's repeat replacing its banner, and calls
/// `clicked` with the identity of a banner the user clicked. Ends when every sink is gone.
pub async fn run(banners: async_channel::Receiver<Banner>, app_id: String, clicked: impl Fn(String) + Send + 'static) {
    let mut delivered: HashMap<String, Delivered> = HashMap::new();
    let mut waits: FuturesUnordered<Wait> = FuturesUnordered::new();
    let mut serial = 0_u64;
    loop {
        // a wait that ended is handled before the next banner, so a click seen first forgets its banner first
        let next = if waits.is_empty() {
            Either::Right(banners.recv().await)
        } else {
            match futures_util::future::select(waits.next(), std::pin::pin!(banners.recv())).await {
                Either::Left((ended, _)) => Either::Left(ended),
                Either::Right((banner, _)) => Either::Right(banner),
            }
        };
        match next {
            Either::Left(Some(Some(ended))) => {
                if delivered
                    .get(&ended.identity)
                    .is_some_and(|known| known.serial == ended.serial)
                {
                    delivered.remove(&ended.identity);
                }
                if ended.clicked {
                    tracing::info!(identity = %ended.identity, "notify: banner clicked");
                    clicked(ended.identity);
                }
            }
            Either::Left(_) => {}
            Either::Right(Ok(banner @ Banner::Send { .. })) => {
                serial += 1;
                if let Some(wait) = send(&banner, &app_id, serial, &mut delivered).await {
                    waits.push(wait);
                }
            }
            Either::Right(Ok(Banner::Withdraw { identity })) => {
                if let Some(gone) = delivered.remove(&identity) {
                    gone.wait.abort();
                    gone.handle.close_async().await;
                }
            }
            Either::Right(Err(_)) => return,
        }
    }
}

/// Posts `banner` (a send) as delivery `serial`, in place of its identity's banner on screen in `delivered`: its wait,
/// which the task polls beside the next banners.
async fn send(banner: &Banner, app_id: &str, serial: u64, delivered: &mut HashMap<String, Delivered>) -> Option<Wait> {
    let Banner::Send { identity, title, body } = banner else {
        return None;
    };
    let replaced = delivered.remove(identity);
    let mut notification = Notification::new();
    notification
        .appname(APP_NAME)
        .summary(title)
        .body(body)
        .action(DEFAULT_ACTION, "")
        .hint(Hint::DesktopEntry(app_id.to_owned()));
    if let Some(replaced) = replaced {
        replaced.wait.abort();
        notification.id(replaced.handle.id());
    }
    let handle = match notification.show_async().await {
        Ok(handle) => Arc::new(handle),
        Err(error) => {
            tracing::warn!(%error, "notify: the banner was not posted");
            return None;
        }
    };
    let (abort, registration) = AbortHandle::new_pair();
    delivered.insert(
        identity.to_owned(),
        Delivered {
            serial,
            handle: Arc::clone(&handle),
            wait: abort,
        },
    );
    let identity = identity.to_owned();
    let waiting = async move {
        let mut clicked = false;
        handle
            .wait_for_action_async(|response| {
                clicked = matches!(
                    response,
                    NotificationResponse::Default | NotificationResponse::Action(_)
                );
            })
            .await;
        Ended {
            identity,
            serial,
            clicked,
        }
    };
    Some(Box::pin(
        async move { Abortable::new(waiting, registration).await.ok() },
    ))
}

impl AppState {
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
mod tests;
