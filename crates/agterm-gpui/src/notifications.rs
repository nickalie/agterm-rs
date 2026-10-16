//! Desktop notifications (upstream's `Notifications/NotificationManager.swift`, `notifications.md`; port of the GTK
//! build's `notifications.rs`, which posted through `GNotification`) over the freedesktop notification server
//! (`org.freedesktop.Notifications` on the session bus) through `zbus`.
//!
//! The core records each notification (the badge and the `notify` event) and queues its banner; this posts the queued
//! banners, each under its identity `<windowID>:<sessionID>:<pane>`: a repeat from one pane replaces the last one
//! (`replaces_id`), and a click on a banner (its `default` action, `ActionInvoked`) reveals the pane that fired and
//! raises its window with the activation token the server sent just before (`ActivationToken`), as `GNotification`'s
//! `activate-action` carries it. Withdrawals are the core's explicit clears, queued with the posts and applied in that
//! order (`CloseNotification`): when a pane of the session takes focus and after a successful swap, as upstream's
//! `clearDelivered` call sites. A clear withdraws every pane's identity of that window and session; a session no open
//! window holds gets none, so a closed window's banners stay clickable and reopen it. A banner the server closes (it
//! expired, the user dismissed it) is forgotten. The badge clears on its own path and leaves the banners.
//!
//! The server sees no sound: a batch that posts banners sounds the configured notification sound as the display's
//! alert (Linux has none of upstream's named sounds), through the status sounds' throttled bell
//! ([`crate::status_sound`]); the badge and the event never depend on it.
//!
//! The banners go out in order on one task ([`run`]) over one session-bus connection of its own, which is also where
//! the server's signals arrive (a server may send `ActivationToken` to the connection that posted the banner alone);
//! the task routes them to their banners by server id.

use std::collections::{HashMap, VecDeque};
use std::future::Future;

use crate::instance::Activation;
use crate::state::{AppState, Poster, post};
use agterm_core::{NotificationEffect, parse_notification_identity};
use futures_util::future::Either;
use futures_util::stream::StreamExt as _;
use zbus::message::Type as MessageType;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::Value;
use zbus::{Connection, MatchRule, Message, MessageStream};

/// The application name banners carry (the server's grouping; GTK's `g_get_application_name`).
pub const APP_NAME: &str = "agterm";

/// The action key a click on a banner's body invokes.
const DEFAULT_ACTION: &str = "default";

/// The hint naming the desktop entry a banner belongs to.
const DESKTOP_ENTRY_HINT: &str = "desktop-entry";

/// The notification server's bus name, object and interface.
const SERVER: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";

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
        let clicked = move |click: Click| {
            post(&poster, move |state: &mut AppState| {
                state.reveal_notification(&click.identity, click.token);
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

/// A click on a banner: the identity it was posted under, and the activation token the server sent with the click
/// (`ActivationToken`: an xdg-activation token on Wayland, a startup id on X11), which raises the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Click {
    pub identity: String,
    pub token: Option<String>,
}

/// What the server says of a banner it shows.
#[derive(Debug, PartialEq, Eq)]
enum Signal {
    /// The token for the click that follows (`ActivationToken`).
    Token { id: u32, token: String },
    /// The user clicked the banner or one of its actions (`ActionInvoked`).
    Invoked { id: u32 },
    /// The banner is gone: it expired, the user dismissed it, or it was withdrawn (`NotificationClosed`).
    Closed { id: u32 },
}

impl Signal {
    /// The signal `message` carries when `server` sent it; `None` for anything else.
    fn of(message: &Message, server: Option<&OwnedUniqueName>) -> Option<Self> {
        let header = message.header();
        let server = server?;
        if header.message_type() != MessageType::Signal || header.sender() != Some(&**server) {
            return None;
        }
        let body = message.body();
        match header.member()?.as_str() {
            "ActivationToken" => {
                let (id, token) = body.deserialize::<(u32, String)>().ok()?;
                Some(Self::Token { id, token })
            }
            "ActionInvoked" => {
                let (id, _key) = body.deserialize::<(u32, String)>().ok()?;
                Some(Self::Invoked { id })
            }
            "NotificationClosed" => {
                let (id, _reason) = body.deserialize::<(u32, u32)>().ok()?;
                Some(Self::Closed { id })
            }
            _ => None,
        }
    }
}

/// The banner task's session-bus connection: the server's signals it reads (`signals`) and those that arrived while a
/// call waited for its reply (`early`), handled next.
struct Bus {
    connection: Connection,
    signals: MessageStream,
    early: VecDeque<Message>,
}

impl Bus {
    async fn open() -> zbus::Result<Self> {
        let connection = Connection::session().await?;
        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(INTERFACE)?
            .path(PATH)?
            .build();
        let signals = MessageStream::for_match_rule(rule, &connection, None).await?;
        Ok(Self {
            connection,
            signals,
            early: VecDeque::new(),
        })
    }

    /// The next of the server's signals, `None` once the connection is gone.
    async fn signal(&mut self) -> Option<Message> {
        if let Some(early) = self.early.pop_front() {
            return Some(early);
        }
        loop {
            match self.signals.next().await? {
                Ok(message) => return Some(message),
                Err(error) => tracing::debug!(%error, "notify: an unreadable message on the bus"),
            }
        }
    }

    /// Waits for `reply` while still reading the signals into `early`: a connection whose signal queue fills stops
    /// reading its socket, the reply included.
    async fn reply(
        signals: &mut MessageStream,
        early: &mut VecDeque<Message>,
        reply: impl Future<Output = zbus::Result<Message>>,
    ) -> zbus::Result<Message> {
        let mut reply = std::pin::pin!(reply);
        loop {
            match futures_util::future::select(reply.as_mut(), signals.next()).await {
                Either::Left((reply, _)) => return reply,
                Either::Right((Some(Ok(message)), _)) => early.push_back(message),
                Either::Right((Some(Err(_)), _)) => {}
                Either::Right((None, _)) => return reply.await,
            }
        }
    }

    /// Posts a banner in place of banner `replaces` (0 for none): its id, and the server's unique name.
    async fn notify(
        &mut self,
        replaces: u32,
        title: &str,
        body: &str,
        app_id: &str,
    ) -> zbus::Result<(u32, Option<OwnedUniqueName>)> {
        let Self {
            connection,
            signals,
            early,
        } = self;
        let actions = vec![DEFAULT_ACTION, ""];
        let hints = HashMap::from([(DESKTOP_ENTRY_HINT, Value::from(app_id))]);
        // the server's own expiry
        let expire_timeout = -1_i32;
        let arguments = (APP_NAME, replaces, "", title, body, actions, hints, expire_timeout);
        let call = connection.call_method(Some(SERVER), PATH, Some(INTERFACE), "Notify", &arguments);
        let reply = Self::reply(signals, early, call).await?;
        let server = reply
            .header()
            .sender()
            .map(|name| OwnedUniqueName::from(name.to_owned()));
        Ok((reply.body().deserialize::<u32>()?, server))
    }

    /// Withdraws banner `id`.
    async fn close(&mut self, id: u32) -> zbus::Result<()> {
        let Self {
            connection,
            signals,
            early,
        } = self;
        let arguments = (id,);
        let call = connection.call_method(Some(SERVER), PATH, Some(INTERFACE), "CloseNotification", &arguments);
        Self::reply(signals, early, call).await.map(drop)
    }
}

/// The banners on screen: their server ids by identity, the tokens the server sent for them, and the server that
/// posted them (whose signals alone count).
#[derive(Default)]
struct Shown {
    ids: HashMap<String, u32>,
    tokens: HashMap<u32, String>,
    server: Option<OwnedUniqueName>,
}

impl Shown {
    /// Forgets banner `id`: the identity it was posted under, if it is still that identity's banner.
    fn forget(&mut self, id: u32) -> Option<String> {
        self.tokens.remove(&id);
        let identity = self.ids.iter().find(|(_, known)| **known == id)?.0.clone();
        self.ids.remove(&identity);
        Some(identity)
    }

    /// Applies the server's signal `message`: the click it reports, if any.
    fn signal(&mut self, message: &Message) -> Option<Click> {
        match Signal::of(message, self.server.as_ref())? {
            Signal::Token { id, token } => {
                if self.ids.values().any(|known| *known == id) {
                    self.tokens.insert(id, token);
                }
                None
            }
            Signal::Invoked { id } => {
                let token = self.tokens.remove(&id);
                let identity = self.forget(id)?;
                Some(Click { identity, token })
            }
            Signal::Closed { id } => {
                self.forget(id);
                None
            }
        }
    }

    /// Posts `banner` (a send) on `bus`, in place of its identity's banner on screen. A post that fails leaves that
    /// banner known, so a later clear still withdraws it and its click still reveals its pane.
    async fn send(&mut self, bus: &mut Bus, banner: &Banner, app_id: &str) {
        let Banner::Send { identity, title, body } = banner else {
            return;
        };
        let replaces = self.ids.get(identity).copied().unwrap_or(0);
        match bus.notify(replaces, title, body, app_id).await {
            Ok((id, server)) => {
                if replaces != 0 && replaces != id {
                    self.tokens.remove(&replaces);
                }
                self.ids.insert(identity.clone(), id);
                self.server = server;
            }
            Err(error) => tracing::warn!(%error, "notify: the banner was not posted"),
        }
    }

    /// Withdraws banner `identity` from `bus`.
    async fn withdraw(&mut self, bus: &mut Bus, identity: &str) {
        let Some(id) = self.ids.remove(identity) else {
            return;
        };
        self.tokens.remove(&id);
        if let Err(error) = bus.close(id).await {
            tracing::debug!(%error, "notify: the banner was not withdrawn");
        }
    }

    /// Whether `banner` has anything to do on the server: every send, a withdrawal of a banner on screen.
    fn reaches_server(&self, banner: &Banner) -> bool {
        match banner {
            Banner::Send { .. } => true,
            Banner::Withdraw { identity } => self.ids.contains_key(identity),
        }
    }
}

/// What the banner task handles next.
enum Next {
    Banner(Banner),
    Signal(Message),
    /// The connection is gone: the next banner opens another.
    Lost,
    /// Every sink is gone.
    Done,
}

/// The banner task: posts and withdraws `banners` in order on one session-bus connection (opened with the first
/// banner, again after it drops), each identity's repeat replacing its banner, and calls `clicked` with each banner
/// the user clicked. Ends when every sink is gone.
pub async fn run(banners: async_channel::Receiver<Banner>, app_id: String, clicked: impl Fn(Click) + Send + 'static) {
    let mut shown = Shown::default();
    let mut bus: Option<Bus> = None;
    loop {
        match next(&mut bus, &banners).await {
            Next::Signal(message) => {
                if let Some(click) = shown.signal(&message) {
                    tracing::info!(identity = %click.identity, token = click.token.is_some(), "notify: banner clicked");
                    clicked(click);
                }
            }
            Next::Lost => {
                tracing::debug!("notify: the session-bus connection is gone");
                bus = None;
            }
            Next::Banner(banner) => {
                if !shown.reaches_server(&banner) {
                    continue;
                }
                let Some(open) = connected(&mut bus).await else {
                    continue;
                };
                match &banner {
                    Banner::Send { .. } => shown.send(open, &banner, &app_id).await,
                    Banner::Withdraw { identity } => shown.withdraw(open, identity).await,
                }
            }
            Next::Done => return,
        }
    }
}

/// The next banner or server signal: a signal ready first is handled first, so a click seen before the next banner
/// forgets its banner first.
async fn next(bus: &mut Option<Bus>, banners: &async_channel::Receiver<Banner>) -> Next {
    let Some(open) = bus.as_mut() else {
        return banners.recv().await.map_or(Next::Done, Next::Banner);
    };
    let signal = std::pin::pin!(open.signal());
    let banner = std::pin::pin!(banners.recv());
    match futures_util::future::select(signal, banner).await {
        Either::Left((Some(message), _)) => Next::Signal(message),
        Either::Left((None, _)) => Next::Lost,
        Either::Right((Ok(banner), _)) => Next::Banner(banner),
        Either::Right((Err(_), _)) => Next::Done,
    }
}

/// The task's connection, opened when there is none; `None` when the bus is out of reach (the banner is dropped).
async fn connected(bus: &mut Option<Bus>) -> Option<&mut Bus> {
    if bus.is_none() {
        match Bus::open().await {
            Ok(open) => *bus = Some(open),
            Err(error) => {
                tracing::warn!(%error, "notify: no session bus to post banners on");
                return None;
            }
        }
    }
    bus.as_mut()
}

impl AppState {
    /// A banner was clicked: its pane is revealed (its closed window reopened, even when the session turned out gone)
    /// and its window raised with the server's activation `token` when it sent one (a compositor that prevents focus
    /// stealing may refuse a raise without one); a malformed identity only leaves the app up.
    pub fn reveal_notification(&mut self, identity: &str, token: Option<String>) {
        let Some((window, session, pane)) = parse_notification_identity(identity) else {
            return;
        };
        if self.core.reveal_notification(window, session, pane) {
            if let Some(token) = token {
                self.activation_requested(Activation {
                    token: Some(token),
                    startup_id: None,
                });
            }
            self.settle();
        }
    }
}

#[cfg(test)]
mod tests;
