//! The global hotkey on Wayland: the desktop's global shortcuts portal (`org.freedesktop.portal.GlobalShortcuts`),
//! the one way a Wayland client gets a key whichever surface has the keyboard. One session holds the one shortcut
//! ([`super::SHORTCUT_ID`]), bound with the keymap's chord as its preferred trigger; a new chord closes that session
//! and binds a fresh one, so the desktop drops the old trigger. The portal's `Activated` signal for the session's
//! shortcut runs the hotkey. Without the portal (wlroots compositors, niri) nothing is bound: logged once.

use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut, Shortcut};
use ashpd::desktop::{CreateSessionOptions, Session};
use futures_util::StreamExt as _;
use gpui_kit::{App, Task};

/// What the binding is asked to hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// No shortcut.
    Unbound,
    /// The shortcut, with its preferred trigger (`None`: the desktop asks the user for one).
    Bound(Option<String>),
}

/// The portal binding: a task of the app's that owns the portal session.
pub struct Binding {
    wanted: async_channel::Sender<Wanted>,
    _task: Task<()>,
}

/// What the task hears: a new wish, an activation of some shortcut (its id), or the binding's end.
enum Heard {
    Wanted(Wanted),
    Activated(String),
    Ended,
}

impl Binding {
    /// Starts the binding; `fired` runs for each activation of the bound shortcut.
    pub fn start(fired: impl Fn() + Send + Sync + 'static, cx: &mut App) -> Self {
        let (wanted, asked) = async_channel::unbounded::<Wanted>();
        let task = cx.spawn(async move |_| run(asked, fired).await);
        Self { wanted, _task: task }
    }

    /// Asks the binding to hold `wanted`.
    pub fn set(&self, wanted: Wanted) {
        if self.wanted.try_send(wanted).is_err() {
            tracing::debug!("the global shortcuts binding has ended");
        }
    }
}

async fn run(asked: async_channel::Receiver<Wanted>, fired: impl Fn()) {
    let Some((portal, activations)) = open().await else {
        return;
    };
    let wishes = asked
        .map(Heard::Wanted)
        .chain(futures_util::stream::iter(std::iter::once(Heard::Ended)));
    let mut heard = std::pin::pin!(futures_util::stream::select(wishes, activations));
    let mut session: Option<Session<GlobalShortcuts>> = None;
    while let Some(event) = heard.next().await {
        match event {
            Heard::Wanted(wanted) => {
                close(session.take()).await;
                if let Wanted::Bound(trigger) = wanted {
                    session = bind(&portal, trigger.as_deref()).await;
                }
            }
            // the portal signals this connection's sessions only, and every earlier one is closed
            Heard::Activated(id) if session.is_some() && id == super::SHORTCUT_ID => fired(),
            Heard::Activated(_) => {}
            Heard::Ended => break,
        }
    }
    close(session).await;
}

/// The portal and its activations; `None` (logged) without one.
async fn open() -> Option<(GlobalShortcuts, impl futures_util::Stream<Item = Heard>)> {
    let portal = match GlobalShortcuts::new().await {
        Ok(portal) => portal,
        Err(error) => {
            tracing::warn!(%error, "no global shortcuts portal: global-hotkey is ignored on this desktop");
            return None;
        }
    };
    match portal.receive_activated().await {
        Ok(activations) => {
            let heard = activations.map(|activated| Heard::Activated(activated.shortcut_id().to_owned()));
            Some((portal, heard))
        }
        Err(error) => {
            tracing::warn!(%error, "the global shortcuts portal's activations cannot be followed");
            None
        }
    }
}

async fn close(session: Option<Session<GlobalShortcuts>>) {
    if let Some(session) = session
        && let Err(error) = session.close().await
    {
        tracing::debug!(%error, "a global shortcuts session did not close");
    }
}

/// A session with the shortcut bound to `trigger`; `None` (logged) when the portal refused.
async fn bind(portal: &GlobalShortcuts, trigger: Option<&str>) -> Option<Session<GlobalShortcuts>> {
    let session = match portal.create_session(CreateSessionOptions::default()).await {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(%error, "the global shortcuts portal opened no session");
            return None;
        }
    };
    let shortcut = NewShortcut::new(super::SHORTCUT_ID, super::SHORTCUT_DESCRIPTION).preferred_trigger(trigger);
    let bound = portal
        .bind_shortcuts(&session, &[shortcut], None, BindShortcutsOptions::default())
        .await
        .and_then(|request| request.response());
    match bound {
        Ok(response) => {
            let shown: Vec<&str> = response.shortcuts().iter().map(Shortcut::trigger_description).collect();
            tracing::info!(?trigger, ?shown, "global hotkey: bound through the portal");
            Some(session)
        }
        Err(error) => {
            tracing::warn!(%error, "the global shortcuts portal did not bind the global hotkey");
            close(Some(session)).await;
            None
        }
    }
}
