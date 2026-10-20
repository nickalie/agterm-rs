//! The handoffs this process asked for, so that a server of another build is asked once ([`super::connect_or_start`]):
//! every attempt disconnects the server's clients, even one that then fails, so a failing handoff asked again by each
//! client reattaching would keep disconnecting the others.
//!
//! The memo is process-wide and keyed by the endpoint, the server's pid and build, and the build expected; it never
//! expires (time passing does not make a refused handoff work): a new server (another pid or build), another expected
//! build or another client process asks again, and a failed attempt leaves the caller's reset offer standing. An
//! attempt is reserved before its request, so a caller racing on it waits for it to settle and then looks again
//! instead of asking a second time; one whose outcome never settled (a reconnect that failed after the request, a
//! panic) counts as failed, with its outcome unknown. Settled with Codex (the run's
//! `discussions/fixer-task-21-handoff-retry`).

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use super::HandoffAttempt;

/// The reason an attempt whose outcome never settled is remembered with.
pub(super) const UNKNOWN: &str = "the handoff's outcome is unknown";

/// Which server was asked to hand over, to which build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AttemptKey {
    pub(super) endpoint: String,
    pub(super) pid: u32,
    pub(super) build: String,
    pub(super) expected: String,
}

#[derive(Debug)]
enum Entry {
    /// Asked, not settled yet.
    Running,
    /// Failed, for this reason.
    Failed(String),
}

/// The attempts, with a condition variable the callers waiting on a running one wait on.
pub(super) struct Attempts {
    entries: Mutex<Vec<(AttemptKey, Entry)>>,
    settled: Condvar,
}

/// This process's attempts.
pub(super) static ATTEMPTS: Attempts = Attempts::new();

/// What a caller about to ask a server for a handoff does.
pub(super) enum Claim<'a> {
    /// No attempt yet: this caller asks, holding the reservation until the attempt settles.
    Ask(Ticket<'a>),
    /// An earlier attempt failed, for this reason: no new one.
    Failed(String),
    /// Another caller's attempt ran meanwhile and settled: the caller connects again and looks anew.
    Waited,
}

impl Attempts {
    pub(super) const fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            settled: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<(AttemptKey, Entry)>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserve the attempt `key` names, or say why not: it failed before, or it was running and the caller waited for
    /// it to settle.
    pub(super) fn claim(&self, key: AttemptKey) -> Claim<'_> {
        let mut entries = self.lock();
        let mut waited = false;
        loop {
            match entries.iter().find(|(entry, _)| *entry == key).map(|(_, entry)| entry) {
                Some(Entry::Running) => {
                    waited = true;
                    entries = self.settled.wait(entries).unwrap_or_else(PoisonError::into_inner);
                }
                _ if waited => return Claim::Waited,
                Some(Entry::Failed(reason)) => return Claim::Failed(reason.clone()),
                None => {
                    entries.push((key.clone(), Entry::Running));
                    return Claim::Ask(Ticket {
                        attempts: self,
                        key,
                        failure: Some(UNKNOWN.to_owned()),
                    });
                }
            }
        }
    }
}

/// An attempt a caller reserved. Dropped, it settles: forgotten once it committed, else remembered as failed with the
/// last reason it was given ([`UNKNOWN`] when none was), and the callers waiting on it wake.
pub(super) struct Ticket<'a> {
    attempts: &'a Attempts,
    key: AttemptKey,
    /// `None` once it committed.
    failure: Option<String>,
}

impl Ticket<'_> {
    /// The attempt failed so far for `reason`.
    pub(super) fn fail(&mut self, reason: impl Into<String>) {
        self.failure = Some(reason.into());
    }

    /// The attempt came to `attempt`.
    pub(super) fn settle(mut self, attempt: &HandoffAttempt) {
        self.failure = match attempt {
            HandoffAttempt::Committed { .. } => None,
            HandoffAttempt::Failed { reason, .. } => Some(reason.clone()),
        };
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        let mut entries = self.attempts.lock();
        entries.retain(|(key, _)| *key != self.key);
        if let Some(reason) = self.failure.take() {
            entries.push((self.key.clone(), Entry::Failed(reason)));
        }
        drop(entries);
        self.attempts.settled.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    fn key(pid: u32, expected: &str) -> AttemptKey {
        AttemptKey {
            endpoint: "/tmp/live/server.sock".to_owned(),
            pid,
            build: "old".to_owned(),
            expected: expected.to_owned(),
        }
    }

    fn failed(reason: &str) -> HandoffAttempt {
        HandoffAttempt::Failed {
            from: "old".to_owned(),
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn a_failed_attempt_is_not_asked_again_for_the_same_server_and_build() {
        let attempts = Attempts::new();
        let Claim::Ask(ticket) = attempts.claim(key(1, "new")) else {
            panic!("the first claim asks");
        };
        ticket.settle(&failed("refused"));
        assert!(matches!(attempts.claim(key(1, "new")), Claim::Failed(reason) if reason == "refused"));
        // another server, or another build expected, asks again
        assert!(matches!(attempts.claim(key(2, "new")), Claim::Ask(_)));
        assert!(matches!(attempts.claim(key(1, "newer")), Claim::Ask(_)));
    }

    #[test]
    fn a_ticket_dropped_unsettled_keeps_the_attempt_used_up() {
        let attempts = Attempts::new();
        let Claim::Ask(ticket) = attempts.claim(key(1, "new")) else {
            panic!("the first claim asks");
        };
        drop(ticket);
        assert!(matches!(attempts.claim(key(1, "new")), Claim::Failed(reason) if reason == UNKNOWN));
        // the request's reason stands when the reconnect after it failed
        let Claim::Ask(mut ticket) = attempts.claim(key(2, "new")) else {
            panic!("another server asks");
        };
        ticket.fail("no answer");
        drop(ticket);
        assert!(matches!(attempts.claim(key(2, "new")), Claim::Failed(reason) if reason == "no answer"));
    }

    #[test]
    fn a_committed_attempt_is_forgotten() {
        let attempts = Attempts::new();
        let Claim::Ask(ticket) = attempts.claim(key(1, "new")) else {
            panic!("the first claim asks");
        };
        ticket.settle(&HandoffAttempt::Committed { from: "old".to_owned() });
        assert!(matches!(attempts.claim(key(1, "new")), Claim::Ask(_)));
    }

    /// A caller claiming while another's attempt runs waits for it, then looks again and finds it failed: never a
    /// second request, whether the attempt settled with a reason or unknown.
    #[test]
    fn a_concurrent_claimer_waits_and_never_asks_a_second_time() {
        for reason in [Some("refused"), None] {
            let attempts = Arc::new(Attempts::new());
            let Claim::Ask(ticket) = attempts.claim(key(1, "new")) else {
                panic!("the first claim asks");
            };
            let racing = Arc::clone(&attempts);
            let racer = thread::spawn(move || {
                let first = matches!(racing.claim(key(1, "new")), Claim::Waited);
                let again = match racing.claim(key(1, "new")) {
                    Claim::Failed(reason) => Some(reason),
                    _ => None,
                };
                (first, again)
            });
            thread::sleep(Duration::from_millis(100));
            assert!(!racer.is_finished(), "the racing claim waits for the attempt");
            match reason {
                Some(reason) => ticket.settle(&failed(reason)),
                None => drop(ticket),
            }
            let (first, again) = racer.join().unwrap();
            assert!(first, "the racing claim reports that it waited");
            assert_eq!(again.as_deref(), Some(reason.unwrap_or(UNKNOWN)));
        }
    }
}
