//! The remote links' frontend side: retrying them now when the network comes back or the machine wakes (upstream's
//! `RemoteLinkObserver`, its path monitor and its screens-did-wake bridge; the GTK build's `remote.rs`, which read
//! gio's `NetworkMonitor` and logind's `PrepareForSleep` over `GLib`). The presentation bridge itself is frontend-neutral
//! ([`ngterm_app::remote`]); the core decides everything else ([`ngterm_core::remote::client`]).
//!
//! A backoff measured on the monotonic clock does not advance across a suspend, so without this a pane could keep
//! waiting for up to five minutes after a resume, or after the network came back.
//!
//! - **The seam**: a platform's sources report [`LinkEvent`]s from threads of their own: the network's reachability
//!   (every report, the first one being the state at start) and a resume from sleep. [`LinkRetries`] folds them into
//!   retries (a report of the network available after an earlier report retries, as upstream's path monitor does; a
//!   resume always does), each posting [`AppState::retry_remote_links`]. Nothing in the seam names GPUI or D-Bus, so
//!   macOS (`NWPathMonitor`, the wake notification) and Windows plug their own sources in beside Linux's.
//! - **Linux** ([`dbus`]): `NetworkManager` on the system bus, else the desktop portal's network monitor on the session
//!   bus, else the kernel's default routes over rtnetlink ([`routes`], gio's netlink monitor), else none, and the
//!   network reads as always reachable (logged): the links then retry on their backoff alone. The resume is logind's `PrepareForSleep(false)`; without logind there is none (logged).
//! - **Elsewhere**: no source yet; the network reads as always reachable and no resume is reported (logged).

use std::sync::{Arc, Mutex, PoisonError};

use ngterm_core::remote::reconnect::NetworkReports;

use crate::state::{AppState, Poster, post};

#[cfg(target_os = "linux")]
pub mod dbus;
#[cfg(target_os = "linux")]
pub mod routes;

/// What a platform's source reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkEvent {
    /// The network's reachability now: on every change, the first report being the state at start.
    Network { available: bool },
    /// The machine woke from sleep.
    Resumed,
}

/// Where a source sends its events, from any thread.
pub type Report = Arc<dyn Fn(LinkEvent) + Send + Sync>;

/// Which events retry the links (module docs).
#[derive(Debug, Default)]
pub struct LinkRetries {
    network: NetworkReports,
}

impl LinkRetries {
    /// Whether `event` retries the links now: the network reported available after an earlier report, or a resume.
    pub fn retries(&mut self, event: LinkEvent) -> bool {
        match event {
            LinkEvent::Network { available } => self.network.changed(available),
            LinkEvent::Resumed => true,
        }
    }
}

/// The report that folds the events into retries ([`LinkRetries`]) and runs `retry` for each, on the source's thread.
#[must_use]
pub fn retrying(retry: impl Fn(LinkEvent) + Send + Sync + 'static) -> Report {
    let retries = Mutex::new(LinkRetries::default());
    Arc::new(move |event| {
        let retries_now = retries.lock().unwrap_or_else(PoisonError::into_inner).retries(event);
        if retries_now {
            retry(event);
        }
    })
}

/// Starts this platform's sources for the app's life; each retry posts [`AppState::retry_remote_links`]. Once, at
/// launch.
pub fn observe_links(poster: Poster) {
    start_sources(&retrying(move |event| {
        tracing::info!(?event, "network back or machine awake: retrying the remote links now");
        post(&poster, AppState::retry_remote_links);
    }));
}

#[cfg(target_os = "linux")]
fn start_sources(report: &Report) {
    dbus::start(&dbus::Endpoints::desktop(), report);
}

#[cfg(not(target_os = "linux"))]
fn start_sources(_report: &Report) {
    tracing::info!(
        "remote links: no network or resume source on this platform; the network reads as always reachable and the \
         links retry on their backoff alone"
    );
}

#[cfg(test)]
mod tests;
