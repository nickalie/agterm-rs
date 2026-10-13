//! Reconnecting a viewer's remote links (upstream's `RemoteReconnect.swift`): the retry schedule every remote link
//! shares, the attach wrapper's word that ssh lost the connection, and the book of panes waiting to be attached again.
//!
//! Nothing here keeps a clock: the owner passes `now` into every call, which keeps the schedule deterministic to test.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use agterm_proto::tree::ControlReconnect;
use uuid::Uuid;

/// The cap of the first failures in a row.
const FIRST_CAP: Duration = Duration::from_secs(30);
/// The cap once a link kept failing: a laptop asleep for the night is not retried every thirty seconds, and never
/// given up on either.
const LATE_CAP: Duration = Duration::from_secs(300);
/// Failures in a row before the cap grows.
const FAILURES_BEFORE_LATE_CAP: u32 = 8;

/// The wait after the `failures`th failure in a row, counted from one (upstream's `RemoteRetryBackoff.delay`): doubling
/// from one second to thirty, then to five minutes after eight failures.
#[must_use]
pub fn retry_delay(failures: u32) -> Duration {
    let cap = if failures > FAILURES_BEFORE_LATE_CAP {
        LATE_CAP
    } else {
        FIRST_CAP
    };
    let exponent = failures.saturating_sub(1).min(30);
    Duration::from_secs(1u64 << exponent).min(cap)
}

const NOTICE_PREFIX: &str = "agterm-remote;";
const NOTICE_SUFFIX: &str = ":lost";

/// The attach wrapper's word that ssh lost the connection: a title under a reserved prefix, like the lead notices. It
/// carries the pane's lead nonce, so only the pane's current attachment is believed (upstream's `RemoteLinkNotice`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteLinkNotice {
    pub nonce: String,
}

impl RemoteLinkNotice {
    /// The notice a title carries, `None` for any other title.
    #[must_use]
    pub fn parse(title: &str) -> Option<Self> {
        let nonce = title.strip_prefix(NOTICE_PREFIX)?.strip_suffix(NOTICE_SUFFIX)?;
        Some(Self {
            nonce: nonce.to_owned(),
        })
    }

    /// The title the wrapper sets for `nonce`.
    #[must_use]
    pub fn title(nonce: &str) -> String {
        format!("{NOTICE_PREFIX}{nonce}{NOTICE_SUFFIX}")
    }
}

/// A pane that loses the link again this soon after attaching keeps its backoff, so a host that answers the probe but
/// fails the attach is not retried every second.
const SETTLE: Duration = Duration::from_secs(60);
/// The longest reason a waiting pane shows, in characters.
pub const REASON_LIMIT: usize = 200;

/// One pane waiting to be attached again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectEntry {
    pub session: Uuid,
    pub host: String,
    /// The origin reported lead roles, so the fresh attach will too and may be covered until it does.
    pub cover: bool,
    failures: u32,
    /// What the last failed probe's ssh said, `None` when it said nothing. It describes that probe only, never the
    /// attach that follows a probe that answered.
    reason: Option<String>,
    retry_at: Instant,
    probing: bool,
}

/// Panes whose ssh lost the connection and wait to be attached again, keyed by pane identity (upstream's
/// `RemoteReconnectBook`).
#[derive(Debug, Default)]
pub struct ReconnectBook {
    entries: HashMap<Uuid, ReconnectEntry>,
    /// Panes attached again lately, with when and the failures they had, for [`SETTLE`].
    resumed: HashMap<Uuid, (Instant, u32)>,
}

impl ReconnectBook {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn waiting(&self, pane: Uuid) -> bool {
        self.entries.contains_key(&pane)
    }

    /// The entry of a waiting pane.
    #[must_use]
    pub fn entry(&self, pane: Uuid) -> Option<&ReconnectEntry> {
        self.entries.get(&pane)
    }

    /// Registers a pane to be reconnected, unless it is already waiting.
    pub fn wait(&mut self, pane: Uuid, session: Uuid, host: &str, cover: bool, now: Instant) {
        if self.entries.contains_key(&pane) {
            return;
        }
        let mut entry = ReconnectEntry {
            session,
            host: host.to_owned(),
            cover,
            failures: 0,
            reason: None,
            retry_at: now,
            probing: false,
        };
        if let Some((at, failures)) = self.resumed.remove(&pane)
            && now.saturating_duration_since(at) < SETTLE
        {
            entry.failures = failures + 1;
            entry.retry_at = now + retry_delay(entry.failures);
        }
        self.entries.insert(pane, entry);
    }

    /// The panes to probe now, marked so a slow probe is not started twice.
    pub fn due(&mut self, now: Instant) -> Vec<Uuid> {
        self.resumed
            .retain(|_, (at, _)| now.saturating_duration_since(*at) < SETTLE);
        let mut panes: Vec<Uuid> = self
            .entries
            .iter()
            .filter(|(_, entry)| !entry.probing && entry.retry_at <= now)
            .map(|(pane, _)| *pane)
            .collect();
        panes.sort_unstable();
        for pane in &panes {
            if let Some(entry) = self.entries.get_mut(pane) {
                entry.probing = true;
            }
        }
        panes
    }

    /// A failed probe schedules the next one. One that answered ends the wait and returns the entry to attach again; a
    /// result for a pane cancelled meanwhile returns `None`.
    pub fn finished(&mut self, pane: Uuid, ok: bool, stderr: &str, now: Instant) -> Option<ReconnectEntry> {
        let entry = self.entries.get_mut(&pane).filter(|entry| entry.probing)?;
        if !ok {
            entry.probing = false;
            entry.failures += 1;
            entry.reason = reason(stderr);
            entry.retry_at = now + retry_delay(entry.failures);
            return None;
        }
        let entry = self.entries.remove(&pane)?;
        self.resumed.insert(pane, (now, entry.failures));
        Some(entry)
    }

    /// The tree's view of a waiting pane, `None` for one that is not waiting.
    #[must_use]
    pub fn readback(&self, pane: Uuid) -> Option<ControlReconnect> {
        self.entries.get(&pane).map(|entry| ControlReconnect {
            failures: i64::from(entry.failures),
            reason: entry.reason.clone(),
        })
    }

    /// Due now with the backoff over; a probe already running is not started twice.
    pub fn retry_now(&mut self, pane: Uuid, now: Instant) {
        let Some(entry) = self.entries.get_mut(&pane) else {
            return;
        };
        entry.failures = 0;
        if !entry.probing {
            entry.retry_at = now;
        }
    }

    pub fn retry_all_now(&mut self, now: Instant) {
        let panes: Vec<Uuid> = self.entries.keys().copied().collect();
        for pane in panes {
            self.retry_now(pane, now);
        }
    }

    pub fn cancel(&mut self, pane: Uuid) {
        self.entries.remove(&pane);
        self.resumed.remove(&pane);
    }

    /// Every waiting pane.
    #[must_use]
    pub fn panes(&self) -> Vec<Uuid> {
        self.entries.keys().copied().collect()
    }

    /// When the next probe is due, `None` with none waiting or every one probing.
    #[must_use]
    pub fn next_due(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter(|entry| !entry.probing)
            .map(|entry| entry.retry_at)
            .min()
    }
}

/// The last non-empty line ssh printed, its control characters removed and its length capped (upstream's
/// `reason(fromStderr:)`).
fn reason(stderr: &str) -> Option<String> {
    let last = stderr
        .split(['\n', '\r', '\u{0b}', '\u{0c}', '\u{85}', '\u{2028}', '\u{2029}'])
        .map(crate::model::naming::sanitized)
        .rfind(|line| !line.chars().all(char::is_whitespace))?;
    Some(
        last.trim_matches(|ch: char| ch == ' ' || ch == '\t')
            .chars()
            .take(REASON_LIMIT)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(value: f64) -> Duration {
        Duration::from_secs_f64(value)
    }

    struct Fixture {
        pane: Uuid,
        session: Uuid,
        t0: Instant,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                pane: Uuid::new_v4(),
                session: Uuid::new_v4(),
                t0: Instant::now(),
            }
        }

        fn at(&self, offset: f64) -> Instant {
            self.t0 + secs(offset)
        }

        fn waiting(&self, cover: bool) -> ReconnectBook {
            let mut book = ReconnectBook::default();
            book.wait(self.pane, self.session, "mini", cover, self.t0);
            book
        }
    }

    #[test]
    fn the_schedule_doubles_to_thirty_seconds_then_slows_down() {
        let delays: Vec<u64> = (1..=10).map(|failures| retry_delay(failures).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30, 256, 300]);
        assert_eq!(retry_delay(100), LATE_CAP);
    }

    #[test]
    fn the_wrappers_title_round_trips_and_nothing_else_is_a_notice() {
        assert_eq!(
            RemoteLinkNotice::parse(&RemoteLinkNotice::title("n1")),
            Some(RemoteLinkNotice { nonce: "n1".to_owned() })
        );
        for title in [
            "agterm-remote;n1",
            "agterm-remote;n1:gone",
            "zmx-role;n1:leader:1",
            "build",
        ] {
            assert_eq!(RemoteLinkNotice::parse(title), None, "{title}");
        }
    }

    #[test]
    fn the_first_probe_is_immediate_and_never_started_twice() {
        let f = Fixture::new();
        let mut book = f.waiting(false);
        assert_eq!(book.due(f.t0), [f.pane]);
        assert!(book.due(f.at(60.0)).is_empty());
        assert!(book.waiting(f.pane));
    }

    #[test]
    fn a_failed_probe_backs_off() {
        let f = Fixture::new();
        let mut book = f.waiting(false);
        book.due(f.t0);
        assert_eq!(book.finished(f.pane, false, "", f.t0), None);
        assert!(book.due(f.at(0.5)).is_empty());
        assert_eq!(book.due(f.at(1.0)), [f.pane]);
        book.finished(f.pane, false, "", f.at(1.0));
        assert!(book.due(f.at(2.5)).is_empty());
        assert_eq!(book.due(f.at(3.0)), [f.pane]);
    }

    #[test]
    fn a_link_lost_soon_after_attaching_counts_in_the_streak_before_any_probe_fails() {
        let f = Fixture::new();
        let mut book = f.waiting(false);
        book.due(f.t0);
        book.finished(f.pane, false, "", f.t0);
        book.due(f.at(1.0));
        assert!(book.finished(f.pane, true, "", f.at(1.0)).is_some());
        book.wait(f.pane, f.session, "mini", false, f.at(2.0));
        assert_eq!(
            book.readback(f.pane),
            Some(ControlReconnect {
                failures: 2,
                reason: None
            })
        );
    }

    #[test]
    fn a_failed_probe_keeps_what_ssh_said_last_and_a_quiet_one_clears_it() {
        let f = Fixture::new();
        let mut book = f.waiting(false);
        assert_eq!(
            book.readback(f.pane),
            Some(ControlReconnect {
                failures: 0,
                reason: None
            })
        );
        book.due(f.t0);
        let stderr = "Warning: Permanently added 'mini'\n\u{1b}[31mHost key verification failed.\r\n\n";
        book.finished(f.pane, false, stderr, f.t0);
        assert_eq!(
            book.readback(f.pane),
            Some(ControlReconnect {
                failures: 1,
                reason: Some("[31mHost key verification failed.".to_owned())
            })
        );
        book.due(f.at(1.0));
        book.finished(f.pane, false, " \n", f.at(1.0));
        assert_eq!(
            book.readback(f.pane),
            Some(ControlReconnect {
                failures: 2,
                reason: None
            })
        );
    }

    #[test]
    fn the_reason_is_capped_and_goes_with_the_wait() {
        let f = Fixture::new();
        let mut book = f.waiting(false);
        book.due(f.t0);
        book.finished(f.pane, false, &"x".repeat(500), f.t0);
        let reason = book.readback(f.pane).and_then(|readback| readback.reason).unwrap();
        assert_eq!(reason.chars().count(), REASON_LIMIT);
        book.due(f.at(1.0));
        assert!(book.finished(f.pane, true, "", f.at(1.0)).is_some());
        assert_eq!(book.readback(f.pane), None);
    }

    #[test]
    fn a_probe_that_answers_ends_the_wait_and_carries_the_cover() {
        let f = Fixture::new();
        let mut book = f.waiting(true);
        book.due(f.t0);
        let entry = book.finished(f.pane, true, "", f.t0).unwrap();
        assert!(entry.session == f.session && entry.host == "mini" && entry.cover);
        assert!(!book.waiting(f.pane));
        assert!(book.is_empty());
    }

    #[test]
    fn retry_now_makes_a_later_probe_due_but_not_a_second_concurrent_one() {
        let f = Fixture::new();
        let mut book = f.waiting(false);
        for step in 0..6 {
            let now = f.at(f64::from(step) * 100.0);
            book.due(now);
            book.finished(f.pane, false, "", now);
        }
        let later = f.at(501.0);
        assert!(book.due(later).is_empty());
        book.retry_now(f.pane, later);
        assert_eq!(book.due(later), [f.pane]);
        book.retry_now(f.pane, later);
        assert!(book.due(later).is_empty());
    }

    #[test]
    fn retry_all_now_makes_every_waiting_pane_due_and_starts_its_backoff_over() {
        let f = Fixture::new();
        let other = Uuid::new_v4();
        let mut book = ReconnectBook::default();
        book.wait(f.pane, f.session, "mini", false, f.t0);
        book.wait(other, f.session, "mini", false, f.t0);
        let mut now = f.t0;
        for step in 0..9 {
            now = f.at(f64::from(step) * 400.0);
            book.due(now);
            book.finished(f.pane, false, "", now);
            book.finished(other, false, "", now);
        }
        let later = now + secs(200.0);
        assert!(
            book.due(later).is_empty(),
            "nine failures in a row: minutes until the next probe"
        );
        book.retry_all_now(later);
        let mut due = book.due(later);
        due.sort_unstable();
        let mut both = vec![f.pane, other];
        both.sort_unstable();
        assert_eq!(due, both);
        book.finished(f.pane, false, "", later);
        assert_eq!(
            book.due(later + secs(1.0)),
            [f.pane],
            "a retry that failed ramps from one second again"
        );
    }

    #[test]
    fn a_retry_during_a_probe_still_starts_the_backoff_over_when_that_probe_fails() {
        let f = Fixture::new();
        let mut book = f.waiting(false);
        let mut now = f.t0;
        for step in 0..5 {
            now = f.at(f64::from(step) * 100.0);
            book.due(now);
            book.finished(f.pane, false, "", now);
        }
        let probing = now + secs(100.0);
        assert_eq!(book.due(probing), [f.pane]);
        book.retry_all_now(probing);
        assert!(book.due(probing).is_empty(), "the running probe is not started twice");
        book.finished(f.pane, false, "", probing);
        assert_eq!(book.due(probing + secs(1.0)), [f.pane]);
    }

    #[test]
    fn a_cancelled_panes_probe_result_is_dropped() {
        let f = Fixture::new();
        let mut book = f.waiting(false);
        book.due(f.t0);
        book.cancel(f.pane);
        assert_eq!(book.finished(f.pane, true, "", f.t0), None);
        assert!(book.is_empty());
    }

    #[test]
    fn a_pane_that_loses_the_link_soon_after_attaching_keeps_its_backoff() {
        let f = Fixture::new();
        let mut book = f.waiting(false);
        book.due(f.t0);
        book.finished(f.pane, false, "", f.t0);
        book.due(f.at(1.0));
        book.finished(f.pane, true, "", f.at(1.0));
        book.wait(f.pane, f.session, "mini", false, f.at(5.0));
        assert!(book.due(f.at(6.0)).is_empty());
        assert_eq!(book.due(f.at(7.0)), [f.pane]);
    }

    #[test]
    fn a_pane_that_loses_the_link_long_after_starts_over() {
        let f = Fixture::new();
        let mut book = f.waiting(false);
        book.due(f.t0);
        book.finished(f.pane, true, "", f.t0);
        book.wait(f.pane, f.session, "mini", false, f.at(600.0));
        assert_eq!(book.due(f.at(600.0)), [f.pane]);
    }

    #[test]
    fn waiting_again_while_waiting_keeps_the_probe_in_flight() {
        let f = Fixture::new();
        let mut book = f.waiting(false);
        assert_eq!(book.due(f.t0), [f.pane]);
        book.wait(f.pane, Uuid::new_v4(), "other", true, f.t0);
        assert!(book.due(f.t0).is_empty());
        assert_eq!(
            book.finished(f.pane, true, "", f.t0).map(|entry| entry.host),
            Some("mini".to_owned())
        );
    }
}
