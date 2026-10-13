//! Runs `hooks.conf` entries against control events (upstream's `HookScheduler`): one child per hook at a time, an
//! ordered bounded queue behind it, and only process exit releasing the slot.
//!
//! Host-free: the frontend supplies a [`HookLauncher`] that starts the child and hands back its pid, then reports the
//! run's stdin delivery failure and its exit through [`HookScheduler::delivery_failed`] and [`HookScheduler::exited`]
//! with the run's [`HookRunToken`]. A token from a superseded run is ignored, so a late report can never release the
//! slot of a newer child.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use agterm_proto::{ControlEvent, ControlHookEntry};

use super::{HookEntry, HookIdentity, Hooks};

/// How many events wait behind a busy hook; past it the oldest is dropped and counted (upstream's
/// `HookScheduler.pendingCapacity`).
pub const PENDING_CAPACITY: usize = 256;

/// Names one launched run of one hook. Opaque: the frontend keeps it with the child and hands it back to
/// [`HookScheduler::exited`] and [`HookScheduler::delivery_failed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRunToken {
    identity: HookIdentity,
    run: u64,
}

/// One child to start: the hook, the event it runs for, and the token its later reports carry.
#[derive(Debug)]
pub struct HookRun<'a> {
    pub token: HookRunToken,
    pub entry: &'a HookEntry,
    pub event: &'a ControlEvent,
}

/// Starts hook children (upstream's `HookLauncher` protocol).
pub trait HookLauncher {
    /// Starts `/bin/sh -c <command>` for `run`, delivering the event on its stdin and [`super::hook_environment`] in
    /// its environment, and returns the child's pid.
    ///
    /// A returned pid promises exactly one later [`HookScheduler::exited`] for `run.token`, after the child's stdin
    /// is closed and after any [`HookScheduler::delivery_failed`]; neither may be called from inside `launch`.
    ///
    /// # Errors
    ///
    /// No child started; the message lands in the hook's `spawn failed: <message>`, and no report may follow.
    fn launch(&mut self, run: HookRun<'_>) -> Result<i32, String>;
}

/// The live child of one hook.
struct Running {
    pid: i32,
    started_at: Instant,
    run: u64,
    delivery_failed: bool,
}

/// One hook's definition and live state.
struct Record {
    entry: HookEntry,
    running: Option<Running>,
    pending: VecDeque<ControlEvent>,
    dropped: u64,
    last_failure: Option<String>,
    banner_shown: bool,
    /// Removed from the file while its child still runs: accepts nothing, and goes away on exit.
    retired: bool,
}

impl Record {
    fn new(entry: HookEntry) -> Self {
        Self {
            entry,
            running: None,
            pending: VecDeque::new(),
            dropped: 0,
            last_failure: None,
            banner_shown: false,
            retired: false,
        }
    }

    /// Records a failure and queues its banner, unless this hook already bannered since its last success or reload.
    fn fail(&mut self, message: String, failures: &mut Vec<(HookEntry, String)>) {
        if !self.banner_shown {
            self.banner_shown = true;
            failures.push((self.entry.clone(), message.clone()));
        }
        self.last_failure = Some(message);
    }
}

/// The hook runner (see the module docs).
pub struct HookScheduler {
    now: Box<dyn Fn() -> Instant>,
    records: HashMap<HookIdentity, Record>,
    /// The live identities in file order.
    order: Vec<HookIdentity>,
    next_run: u64,
    /// Banners not yet taken, oldest first.
    failures: Vec<(HookEntry, String)>,
}

impl Default for HookScheduler {
    fn default() -> Self {
        Self::new(Box::new(Instant::now))
    }
}

impl HookScheduler {
    /// An empty scheduler reading the time from `now`, which only `status`'s elapsed seconds use.
    #[must_use]
    pub fn new(now: Box<dyn Fn() -> Instant>) -> Self {
        Self {
            now,
            records: HashMap::new(),
            order: Vec::new(),
            next_run: 1,
            failures: Vec::new(),
        }
    }

    /// Replaces the definitions. An unchanged identity keeps its child, queue and counters and takes the new line; a
    /// removed one drops its queue, accepts nothing and stays until its child exits; a re-add before that exit
    /// reattaches. Every banner is re-armed.
    pub fn apply(&mut self, hooks: &Hooks) {
        let live: HashSet<&HookIdentity> = hooks.entries.iter().map(|entry| &entry.identity).collect();
        self.records.retain(|identity, record| {
            if live.contains(identity) {
                return true;
            }
            record.retired = true;
            record.pending.clear();
            record.running.is_some()
        });
        for entry in &hooks.entries {
            if let Some(record) = self.records.get_mut(&entry.identity) {
                record.entry = entry.clone();
                record.retired = false;
            } else {
                self.records.insert(entry.identity.clone(), Record::new(entry.clone()));
            }
        }
        for record in self.records.values_mut() {
            record.banner_shown = false;
        }
        self.order = hooks.entries.iter().map(|entry| entry.identity.clone()).collect();
    }

    /// Fans one ring event out to every live hook of its kind, in file order: an idle hook starts a child, a busy one
    /// queues the event.
    pub fn dispatch(&mut self, event: &ControlEvent, launcher: &mut dyn HookLauncher) {
        let targets: Vec<HookIdentity> = self
            .order
            .iter()
            .filter(|identity| identity.kind == event.kind)
            .filter(|identity| self.records.get(*identity).is_some_and(|record| !record.retired))
            .cloned()
            .collect();
        for identity in targets {
            let Some(record) = self.records.get_mut(&identity) else {
                continue;
            };
            if record.running.is_none() {
                self.start(&identity, event.clone(), launcher);
            } else {
                record.pending.push_back(event.clone());
                if record.pending.len() > PENDING_CAPACITY {
                    record.pending.pop_front();
                    record.dropped += 1;
                }
            }
        }
    }

    /// The child of `token`'s run exited with `status`. Releases the hook's slot: a non-zero status is a failure, a
    /// clean exit without a delivery failure clears the last failure and re-arms the banner; a retired hook goes
    /// away, a live one starts its next pending event. A stale token is ignored.
    pub fn exited(&mut self, token: &HookRunToken, status: i32, launcher: &mut dyn HookLauncher) {
        let Some(record) = self.records.get_mut(&token.identity) else {
            return;
        };
        let Some(running) = record.running.take_if(|running| running.run == token.run) else {
            return;
        };
        if status != 0 {
            record.fail(format!("exit {status}"), &mut self.failures);
        } else if !running.delivery_failed {
            record.last_failure = None;
            record.banner_shown = false;
        }
        if record.retired {
            self.records.remove(&token.identity);
            return;
        }
        if let Some(next) = record.pending.pop_front() {
            self.start(&token.identity, next, launcher);
        }
    }

    /// Writing the event to `token`'s child failed. Recorded and bannered on the live run, which keeps its slot until
    /// it exits. A stale token is ignored.
    pub fn delivery_failed(&mut self, token: &HookRunToken, message: &str) {
        let Some(record) = self.records.get_mut(&token.identity) else {
            return;
        };
        let Some(running) = record.running.as_mut().filter(|running| running.run == token.run) else {
            return;
        };
        running.delivery_failed = true;
        record.fail(format!("delivery failed: {message}"), &mut self.failures);
    }

    /// The `hooks.list` rows: the live hooks in file order, then any removed hook whose child still runs, by kind and
    /// command.
    #[must_use]
    pub fn status(&self) -> Vec<ControlHookEntry> {
        let now = (self.now)();
        let mut retired: Vec<&Record> = self
            .records
            .values()
            .filter(|record| record.retired && record.running.is_some())
            .collect();
        retired.sort_by(|a, b| {
            (a.entry.kind().wire_name(), a.entry.command()).cmp(&(b.entry.kind().wire_name(), b.entry.command()))
        });
        self.order
            .iter()
            .filter_map(|identity| self.records.get(identity))
            .chain(retired)
            .map(|record| row(record, now))
            .collect()
    }

    /// Takes the queued failure banners, oldest first: at most one per hook until it succeeds or the file reloads
    /// (upstream's `onFailure`, the sole banner source).
    pub fn take_failures(&mut self) -> Vec<(HookEntry, String)> {
        std::mem::take(&mut self.failures)
    }

    /// Starts `event` on the idle hook `identity`. A spawn failure is recorded and the next pending event tried at
    /// once, so a broken command drains its queue rather than stalling it.
    fn start(&mut self, identity: &HookIdentity, event: ControlEvent, launcher: &mut dyn HookLauncher) {
        let Some(record) = self.records.get_mut(identity) else {
            return;
        };
        let mut next = Some(event);
        while let Some(current) = next.take() {
            let run = self.next_run;
            self.next_run += 1;
            let outcome = launcher.launch(HookRun {
                token: HookRunToken {
                    identity: identity.clone(),
                    run,
                },
                entry: &record.entry,
                event: &current,
            });
            match outcome {
                Ok(pid) => {
                    record.running = Some(Running {
                        pid,
                        started_at: (self.now)(),
                        run,
                        delivery_failed: false,
                    });
                }
                Err(message) => {
                    record.fail(format!("spawn failed: {message}"), &mut self.failures);
                    next = record.pending.pop_front();
                }
            }
        }
    }
}

/// One record's `hooks.list` row as of `now`.
fn row(record: &Record, now: Instant) -> ControlHookEntry {
    ControlHookEntry {
        kind: record.entry.kind().wire_name().to_owned(),
        command: record.entry.command().to_owned(),
        line: i64::try_from(record.entry.line).unwrap_or(i64::MAX),
        running_pid: record.running.as_ref().map(|running| running.pid),
        elapsed_seconds: record
            .running
            .as_ref()
            .map(|running| now.saturating_duration_since(running.started_at).as_secs_f64()),
        pending: i64::try_from(record.pending.len()).unwrap_or(i64::MAX),
        dropped: record.dropped,
        last_failure: record.last_failure.clone(),
        retired: record.retired.then_some(true),
    }
}

#[cfg(test)]
mod tests;
