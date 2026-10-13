//! The client side of the daemon lifecycle (upstream's `ZmxClient` reap and kill paths): the launch reap, a pane's
//! semantic kill, orphan kills for `zmx.prune`, the leader map refresh and the poll that confirms a kill by its
//! leader's exit. The decisions are core's ([`agterm_core::zmx::lifecycle`]); this runs them.

use std::collections::{BTreeMap, BTreeSet};
use std::thread;
use std::time::{Duration, Instant};

use agterm_core::persist::RestoreMode;
use agterm_core::zmx::daemon_name;
use agterm_core::zmx::lifecycle::{leaders, names_to_kill};
use agterm_host::KillOutcome;
use uuid::Uuid;

use crate::client::{ZmxClient, kill_outcome};

/// What the launch reap learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapOutcome {
    /// Every daemon whose client count the listing could read, alive with or without a client, so an unreadable
    /// `err=` row is absent and its pane paces like a missing daemon; `None` when the list was skipped, failed or did
    /// not parse.
    pub running_names: Option<BTreeSet<String>>,
    /// Whether the kill of every orphan it chose went through.
    pub killed_all: bool,
}

impl ZmxClient {
    /// The launch reap: lists the namespace and kills the zero-client daemons [`names_to_kill`] picks for the launch's
    /// `requested` mode against the panes it restores (`known_pane_identities`, `None` when that inventory is
    /// incomplete: a live launch then skips every invocation).
    pub fn reap(&self, known_pane_identities: Option<&BTreeSet<Uuid>>, requested: RestoreMode) -> ReapOutcome {
        if requested == RestoreMode::Live && known_pane_identities.is_none() {
            tracing::error!("skipping the live zmx reap: the persisted pane inventory is incomplete");
            return ReapOutcome {
                running_names: None,
                killed_all: true,
            };
        }
        let Ok(records) = self.session_records(None) else {
            return ReapOutcome {
                running_names: None,
                killed_all: false,
            };
        };
        let running = records
            .iter()
            .filter(|record| record.clients.is_some())
            .map(|record| record.name.clone())
            .collect();
        let known_names: Option<BTreeSet<String>> =
            known_pane_identities.map(|panes| panes.iter().copied().map(daemon_name).collect());
        let killed_all =
            names_to_kill(&records, requested, known_names.as_ref()).is_none_or(|names| self.kill_batch(&names, None));
        ReapOutcome {
            running_names: Some(running),
            killed_all,
        }
    }

    /// Force-kills the daemons of `pane_identities`, whose panes semantic deletion already removed: nothing is left to
    /// be wrong about, so the answer is whether the invocation went through.
    pub fn kill_panes(&self, pane_identities: &[Uuid]) -> bool {
        let names: Vec<String> = pane_identities.iter().copied().map(daemon_name).collect();
        self.kill_batch(&names, None)
    }

    /// One `zmx kill <names…> --force` for every distinct name, in order; `timeout` overrides this invocation's bound.
    /// Diagnostic only: zmx handles the names in turn, so a failure part-way has already reached some daemons, and a
    /// caller confirms each by its leader's exit.
    pub fn kill_batch(&self, names: &[String], timeout: Option<Duration>) -> bool {
        let mut seen = BTreeSet::new();
        let unique: Vec<&str> = names
            .iter()
            .filter(|name| seen.insert(name.as_str()))
            .map(String::as_str)
            .collect();
        if unique.is_empty() {
            return true;
        }
        let mut arguments = vec!["kill"];
        arguments.extend(&unique);
        arguments.push("--force");
        match self.invoke(&arguments, timeout, true, None) {
            Ok(_) => true,
            Err(error) => {
                tracing::error!(names = unique.join(","), %error, "zmx kill failed");
                false
            }
        }
    }

    /// Kills daemons the caller's listing observed as unclaimed and detached, one invocation each so the result is
    /// per name. Never `--force`: its stale-socket unlink is the outcome that cannot be told from success. Pinned zmx
    /// has no kill-if-detached, so the caller re-lists right before.
    pub fn kill_observed_orphans(&self, names: &[String]) -> BTreeMap<String, KillOutcome> {
        let unique: BTreeSet<&str> = names.iter().map(String::as_str).collect();
        unique
            .into_iter()
            .map(|name| {
                let outcome = match self.invoke(&["kill", name], None, true, None) {
                    Ok(output) => kill_outcome(&output, name),
                    Err(error) => {
                        tracing::error!(name, %error, "zmx kill failed");
                        KillOutcome::Failed(error.to_string())
                    }
                };
                (name.to_owned(), outcome)
            })
            .collect()
    }

    /// The leader pid of every app daemon, from one full listing; `None` when it failed.
    #[must_use]
    pub fn leader_pids(&self, timeout: Option<Duration>) -> Option<BTreeMap<String, i32>> {
        self.session_records(timeout).ok().map(|records| leaders(&records))
    }
}

/// How [`leaders_exited`] waits.
pub struct LeaderPoll<'a> {
    pub now: &'a dyn Fn() -> Instant,
    pub sleep: &'a dyn Fn(Duration),
    pub every: Duration,
}

impl LeaderPoll<'_> {
    /// The real clock, every 100 ms.
    pub const REAL: LeaderPoll<'static> = LeaderPoll {
        now: &Instant::now,
        sleep: &thread::sleep,
        every: Duration::from_millis(100),
    };
}

/// Polls `pids` until every leader is gone or `deadline` passes; the pids still alive.
pub fn leaders_exited(
    pids: &BTreeSet<i32>,
    deadline: Instant,
    poll: &LeaderPoll<'_>,
    is_alive: impl Fn(i32) -> bool,
) -> BTreeSet<i32> {
    let mut pending = pids.clone();
    loop {
        pending.retain(|pid| is_alive(*pid));
        if pending.is_empty() || (poll.now)() >= deadline {
            return pending;
        }
        (poll.sleep)(poll.every);
    }
}

/// Whether process `pid` exists: `kill(pid, 0)` succeeds, or fails only for want of permission.
#[must_use]
pub fn pid_is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 sends nothing; `kill` only checks that `pid` exists and may be signalled.
    let status = unsafe { libc::kill(pid, 0) };
    status == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::client::{CommandError, Invocation, Runner};

    type Log = Arc<Mutex<Vec<Invocation>>>;

    fn recording(
        reply: impl Fn(&Invocation) -> Result<String, CommandError> + Send + Sync + 'static,
    ) -> (ZmxClient, Log) {
        let log = Log::default();
        let sink = Arc::clone(&log);
        let runner: Runner = Box::new(move |invocation| {
            sink.lock().unwrap().push(invocation.clone());
            reply(invocation)
        });
        (
            ZmxClient::with_runner("/tmp/zmx", "/tmp/zmx-dir", Duration::from_secs(3), runner),
            log,
        )
    }

    fn arguments(log: &Log) -> Vec<Vec<String>> {
        log.lock()
            .unwrap()
            .iter()
            .map(|invocation| invocation.arguments.clone())
            .collect()
    }

    fn listing(rows: &[String]) -> impl Fn(&Invocation) -> Result<String, CommandError> + Send + Sync + 'static {
        let rows = rows.join("\n");
        move |invocation| {
            Ok(if invocation.arguments[0] == "list" {
                rows.clone()
            } else {
                String::new()
            })
        }
    }

    #[test]
    fn the_live_reap_lists_then_kills_only_unclaimed_zero_client_names() {
        let known = Uuid::from_u128(0x1111);
        let orphan = daemon_name(Uuid::from_u128(0xaaaa));
        let attached = daemon_name(Uuid::from_u128(0xbbbb));
        let (client, log) = recording(listing(&[
            format!("name={}\tpid=1\tclients=0", daemon_name(known)),
            format!("name={orphan}\tpid=2\tclients=0"),
            format!("name={attached}\tpid=3\tclients=1"),
            "name=notes\tpid=4\tclients=0".to_owned(),
            format!("name={}\terr=Timeout", daemon_name(Uuid::from_u128(0xcccc))),
        ]));
        let outcome = client.reap(Some(&BTreeSet::from([known])), RestoreMode::Live);
        assert_eq!(
            arguments(&log),
            [
                vec!["list".to_owned()],
                vec!["kill".to_owned(), orphan.clone(), "--force".to_owned()]
            ]
        );
        assert!(outcome.killed_all);
        let running = outcome.running_names.unwrap();
        assert_eq!(
            running,
            BTreeSet::from([daemon_name(known), orphan, attached, "notes".to_owned()])
        );
    }

    #[test]
    fn an_incomplete_live_inventory_skips_every_invocation_and_a_non_live_one_claims_nothing() {
        let (client, log) = recording(listing(&[format!(
            "name={}\tpid=1\tclients=0",
            daemon_name(Uuid::from_u128(1))
        )]));
        let skipped = client.reap(None, RestoreMode::Live);
        assert_eq!(
            skipped,
            ReapOutcome {
                running_names: None,
                killed_all: true
            }
        );
        assert!(log.lock().unwrap().is_empty());
        let fresh = client.reap(None, RestoreMode::None);
        assert!(fresh.killed_all);
        assert_eq!(arguments(&log)[1][0], "kill");
    }

    #[test]
    fn a_failed_listing_reports_no_running_names_and_a_failed_kill_keeps_them() {
        let (broken, _) = recording(|_| Err(CommandError::TimedOut));
        assert_eq!(
            broken.reap(Some(&BTreeSet::new()), RestoreMode::Live),
            ReapOutcome {
                running_names: None,
                killed_all: false
            }
        );
        let orphan = daemon_name(Uuid::from_u128(2));
        let row = format!("name={orphan}\tpid=2\tclients=0");
        let (client, _) = recording(move |invocation| {
            if invocation.arguments[0] == "list" {
                Ok(row.clone())
            } else {
                Err(CommandError::Failed(1, "boom".to_owned()))
            }
        });
        let outcome = client.reap(Some(&BTreeSet::new()), RestoreMode::Live);
        assert!(!outcome.killed_all);
        assert_eq!(outcome.running_names, Some(BTreeSet::from([orphan])));
    }

    #[test]
    fn a_semantic_kill_uses_full_pane_daemon_names_once_each() {
        let (client, log) = recording(|_| Ok(String::new()));
        let pane = Uuid::from_u128(7);
        assert!(client.kill_panes(&[pane, pane]));
        assert_eq!(
            arguments(&log),
            [vec!["kill".to_owned(), daemon_name(pane), "--force".to_owned()]]
        );
        assert!(log.lock().unwrap()[0].merges_stderr);
        assert!(client.kill_panes(&[]));
        assert_eq!(log.lock().unwrap().len(), 1, "nothing to kill runs nothing");
    }

    #[test]
    fn an_orphan_kill_never_forces_and_separates_a_stale_socket_from_a_kill() {
        let (client, log) = recording(|invocation| {
            Ok(if invocation.arguments[1] == "agterm-gone" {
                "cleaned up stale session agterm-gone\n".to_owned()
            } else {
                "killed session agterm-live\n".to_owned()
            })
        });
        let outcomes = client.kill_observed_orphans(&["agterm-live".to_owned(), "agterm-gone".to_owned()]);
        assert_eq!(outcomes["agterm-live"], KillOutcome::Killed);
        assert_eq!(outcomes["agterm-gone"], KillOutcome::StaleSocket);
        assert!(
            arguments(&log)
                .iter()
                .all(|arguments| arguments.len() == 2 && arguments[0] == "kill")
        );
    }

    #[test]
    fn the_leader_refresh_uses_one_listing_and_keeps_only_app_sessions() {
        let ours = daemon_name(Uuid::from_u128(9));
        let (client, log) = recording(listing(&[
            format!("name={ours}\tpid=41\tclients=1"),
            "name=notes\tpid=42\tclients=0".to_owned(),
        ]));
        assert_eq!(client.leader_pids(None), Some(BTreeMap::from([(ours, 41)])));
        assert_eq!(arguments(&log), [["list"]]);
        assert_eq!(recording(|_| Err(CommandError::TimedOut)).0.leader_pids(None), None);
    }

    #[test]
    fn the_leader_poll_returns_the_survivors_at_the_deadline() {
        let start = Instant::now();
        let clock = Cell::new(start);
        let now = || clock.get();
        let sleep = |step: Duration| clock.set(clock.get() + step);
        let poll = LeaderPoll {
            now: &now,
            sleep: &sleep,
            every: Duration::from_millis(100),
        };
        let checks = Cell::new(0);
        let survivors = leaders_exited(
            &BTreeSet::from([10, 20]),
            start + Duration::from_millis(250),
            &poll,
            |pid| {
                checks.set(checks.get() + 1);
                pid == 20
            },
        );
        assert_eq!(survivors, BTreeSet::from([20]));
        assert_eq!(clock.get(), start + Duration::from_millis(300));
        let gone = leaders_exited(&BTreeSet::from([10]), start, &poll, |_| false);
        assert!(gone.is_empty());
    }

    #[test]
    fn a_pid_is_alive_while_its_process_runs() {
        assert!(pid_is_alive(i32::try_from(std::process::id()).unwrap()));
        assert!(!pid_is_alive(0));
        assert!(!pid_is_alive(-1));
    }
}
