//! When the session server stops: never while a session lives, and once it holds no session and no client for a
//! grace period.
//!
//! herdr: `src/server/shutdown.rs` at `HERDR_REV` (`build/pins.env`), licensed under the Apache License 2.0, modified.
//! Kept: one stop decision, made once, with its reason. Changed: herdr's server runs until it is stopped (a signal,
//! an api request, the host's shutdown); agterm's stops by itself when it holds nothing, so an instance with live
//! sessions off never keeps one, and the decision is this module's pure state machine, which the server's coordinator
//! drives with the clock.

use std::fmt;
use std::time::{Duration, Instant};

/// How long a server just started waits for its first client: the launcher connects as soon as the endpoint answers,
/// so this only bounds a server whose launcher died before connecting.
pub const STARTUP_GRACE: Duration = Duration::from_secs(10);

/// How long a server that holds no session stays after its last client left, so a client's next connection (a
/// command run right after another) finds it instead of starting a new one.
pub const IDLE_GRACE: Duration = Duration::from_secs(3);

/// How long an empty server waits before it stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Graces {
    /// From the start until the first client.
    pub startup: Duration,
    /// From the moment it holds no session and no client again.
    pub idle: Duration,
}

impl Default for Graces {
    /// [`STARTUP_GRACE`] and [`IDLE_GRACE`].
    fn default() -> Self {
        Self {
            startup: STARTUP_GRACE,
            idle: IDLE_GRACE,
        }
    }
}

/// Why the server stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// It held no session and no client for its grace period.
    Idle,
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => f.write_str("no session and no client left"),
        }
    }
}

/// The server's sessions and clients, and the stop decision they lead to. Once [`Lifecycle::poll`] has decided to
/// stop, the decision holds: a client that connects after it is refused ([`Lifecycle::client_connected`]) and
/// reconnects to the next server.
#[derive(Debug)]
pub struct Lifecycle {
    sessions: usize,
    clients: usize,
    /// When the server last became empty, with the grace it has from then.
    idle: Option<(Instant, Duration)>,
    idle_grace: Duration,
    stopped: Option<StopReason>,
}

impl Lifecycle {
    /// A server started at `now`, empty, with the startup grace for its first client.
    #[must_use]
    pub fn new(now: Instant, graces: Graces) -> Self {
        Self {
            sessions: 0,
            clients: 0,
            idle: Some((now, graces.startup)),
            idle_grace: graces.idle,
            stopped: None,
        }
    }

    /// Counts a new client. False once the server has decided to stop: the caller closes the connection.
    pub fn client_connected(&mut self) -> bool {
        if self.stopped.is_some() {
            return false;
        }
        self.clients += 1;
        self.idle = None;
        true
    }

    /// A client left at `now`; the last one leaving an empty server starts its idle grace.
    pub fn client_disconnected(&mut self, now: Instant) {
        self.clients = self.clients.saturating_sub(1);
        self.become_idle_if_empty(now);
    }

    /// Counts a new session. False once the server has decided to stop: the caller does not create it.
    pub fn session_started(&mut self) -> bool {
        if self.stopped.is_some() {
            return false;
        }
        self.sessions += 1;
        self.idle = None;
        true
    }

    /// A session ended at `now`; the last one ending in a server without clients starts its idle grace.
    pub fn session_ended(&mut self, now: Instant) {
        self.sessions = self.sessions.saturating_sub(1);
        self.become_idle_if_empty(now);
    }

    /// The live sessions.
    #[must_use]
    pub fn sessions(&self) -> usize {
        self.sessions
    }

    /// The connected clients.
    #[must_use]
    pub fn clients(&self) -> usize {
        self.clients
    }

    /// When the server stops if nothing arrives before, `None` while it holds a session or a client (or has stopped).
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        if self.stopped.is_some() {
            return None;
        }
        self.idle.map(|(since, grace)| since + grace)
    }

    /// Decides at `now` whether the server stops. The first decision to stop holds and is returned from then on.
    pub fn poll(&mut self, now: Instant) -> Option<StopReason> {
        if self.stopped.is_none() && self.deadline().is_some_and(|deadline| now >= deadline) {
            self.stopped = Some(StopReason::Idle);
        }
        self.stopped
    }

    /// Whether the server has decided to stop.
    #[must_use]
    pub fn stopped(&self) -> Option<StopReason> {
        self.stopped
    }

    fn become_idle_if_empty(&mut self, now: Instant) {
        if self.sessions == 0 && self.clients == 0 && self.idle.is_none() {
            self.idle = Some((now, self.idle_grace));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    fn started(now: Instant) -> Lifecycle {
        Lifecycle::new(now, Graces::default())
    }

    #[test]
    fn a_server_nobody_connects_to_stops_after_its_startup_grace() {
        let start = Instant::now();
        let mut lifecycle = started(start);
        assert_eq!(lifecycle.deadline(), Some(start + STARTUP_GRACE));
        assert_eq!(lifecycle.poll(start + STARTUP_GRACE.saturating_sub(secs(1))), None);
        assert_eq!(lifecycle.poll(start + STARTUP_GRACE), Some(StopReason::Idle));
    }

    #[test]
    fn a_client_holds_the_server_and_its_leaving_starts_the_idle_grace() {
        let start = Instant::now();
        let mut lifecycle = started(start);
        assert!(lifecycle.client_connected());
        assert_eq!(lifecycle.deadline(), None);
        assert_eq!(lifecycle.poll(start + secs(3600)), None);
        let left = start + secs(3600);
        lifecycle.client_disconnected(left);
        assert_eq!(lifecycle.deadline(), Some(left + IDLE_GRACE));
        assert_eq!(
            lifecycle.poll(left + IDLE_GRACE.saturating_sub(Duration::from_millis(1))),
            None
        );
        assert_eq!(lifecycle.poll(left + IDLE_GRACE), Some(StopReason::Idle));
    }

    #[test]
    fn the_server_never_stops_while_a_session_lives() {
        let start = Instant::now();
        let mut lifecycle = started(start);
        assert!(lifecycle.client_connected());
        assert!(lifecycle.session_started());
        lifecycle.client_disconnected(start + secs(1));
        // no client is left, but the session is
        assert_eq!(lifecycle.clients(), 0);
        assert_eq!(lifecycle.deadline(), None);
        assert_eq!(lifecycle.poll(start + secs(86_400)), None);
        let ended = start + secs(86_400);
        lifecycle.session_ended(ended);
        assert_eq!(lifecycle.poll(ended + IDLE_GRACE), Some(StopReason::Idle));
    }

    #[test]
    fn a_session_ending_while_a_client_stays_keeps_the_server() {
        let start = Instant::now();
        let mut lifecycle = started(start);
        assert!(lifecycle.client_connected());
        assert!(lifecycle.session_started());
        lifecycle.session_ended(start + secs(1));
        assert_eq!(lifecycle.deadline(), None);
        assert_eq!(lifecycle.poll(start + secs(60)), None);
    }

    #[test]
    fn a_client_arriving_in_the_grace_cancels_it() {
        let start = Instant::now();
        let mut lifecycle = started(start);
        assert!(lifecycle.client_connected());
        lifecycle.client_disconnected(start);
        assert!(lifecycle.client_connected());
        assert_eq!(lifecycle.poll(start + IDLE_GRACE + secs(1)), None);
        // the grace restarts in full when the client leaves again
        let left = start + secs(10);
        lifecycle.client_disconnected(left);
        assert_eq!(lifecycle.deadline(), Some(left + IDLE_GRACE));
    }

    #[test]
    fn the_decision_to_stop_holds_and_refuses_what_arrives_after_it() {
        let start = Instant::now();
        let graces = Graces {
            startup: Duration::ZERO,
            idle: IDLE_GRACE,
        };
        let mut lifecycle = Lifecycle::new(start, graces);
        assert_eq!(lifecycle.poll(start), Some(StopReason::Idle));
        assert!(!lifecycle.client_connected());
        assert!(!lifecycle.session_started());
        assert_eq!(lifecycle.clients(), 0);
        assert_eq!(lifecycle.sessions(), 0);
        assert_eq!(lifecycle.deadline(), None);
        assert_eq!(lifecycle.stopped(), Some(StopReason::Idle));
        assert_eq!(lifecycle.poll(start + secs(1)), Some(StopReason::Idle));
    }

    #[test]
    fn counts_never_go_below_zero() {
        let start = Instant::now();
        let mut lifecycle = started(start);
        lifecycle.client_disconnected(start);
        lifecycle.session_ended(start);
        assert_eq!((lifecycle.clients(), lifecycle.sessions()), (0, 0));
        // an extra leave of an empty server does not restart its grace
        assert_eq!(lifecycle.deadline(), Some(start + STARTUP_GRACE));
    }

    #[test]
    fn the_idle_grace_is_the_configured_one() {
        let start = Instant::now();
        let graces = Graces {
            startup: secs(1),
            idle: secs(7),
        };
        let mut lifecycle = Lifecycle::new(start, graces);
        assert_eq!(lifecycle.deadline(), Some(start + secs(1)));
        assert!(lifecycle.client_connected());
        lifecycle.client_disconnected(start);
        assert_eq!(lifecycle.deadline(), Some(start + secs(7)));
    }

    #[test]
    fn the_reason_reads_as_a_sentence() {
        assert_eq!(StopReason::Idle.to_string(), "no session and no client left");
    }
}
