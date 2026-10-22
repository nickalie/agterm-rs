//! After upstream's `HookSchedulerTests`, with a fake launcher that records every launch and a fake clock.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use ngterm_proto::{EventKind, EventPayload};

use super::*;
use crate::hooks::parse_hooks_conf;

/// One recorded launch.
struct Launch {
    token: HookRunToken,
    command: String,
    seq: u64,
}

/// Records launches and hands out pids from 101; `fail_next` makes the next launch fail with that message.
#[derive(Default)]
struct FakeLauncher {
    launches: Vec<Launch>,
    fail_next: Option<String>,
    last_pid: i32,
}

impl HookLauncher for FakeLauncher {
    fn launch(&mut self, run: HookRun<'_>) -> Result<i32, String> {
        if let Some(message) = self.fail_next.take() {
            return Err(message);
        }
        self.launches.push(Launch {
            token: run.token,
            command: run.entry.command().to_owned(),
            seq: run.event.seq,
        });
        if self.last_pid == 0 {
            self.last_pid = 100;
        }
        self.last_pid += 1;
        Ok(self.last_pid)
    }
}

/// A scheduler, its launcher and a clock the test advances.
struct Harness {
    scheduler: HookScheduler,
    launcher: FakeLauncher,
    elapsed: Rc<Cell<Duration>>,
}

impl Harness {
    fn new() -> Self {
        let base = Instant::now();
        let elapsed = Rc::new(Cell::new(Duration::ZERO));
        let clock = Rc::clone(&elapsed);
        Self {
            scheduler: HookScheduler::new(Box::new(move || base + clock.get())),
            launcher: FakeLauncher::default(),
            elapsed,
        }
    }

    fn apply(&mut self, text: &str) {
        self.scheduler.apply(&parse_hooks_conf(text).0);
    }

    fn dispatch(&mut self, kind: EventKind, seq: u64) {
        self.scheduler.dispatch(&event(kind, seq), &mut self.launcher);
    }

    fn token(&self, launch: usize) -> HookRunToken {
        self.launcher.launches[launch].token.clone()
    }

    fn exit(&mut self, launch: usize, status: i32) {
        let token = self.token(launch);
        self.scheduler.exited(&token, status, &mut self.launcher);
    }

    fn launched_commands(&self) -> Vec<&str> {
        self.launcher
            .launches
            .iter()
            .map(|launch| launch.command.as_str())
            .collect()
    }

    fn launched_seqs(&self) -> Vec<u64> {
        self.launcher.launches.iter().map(|launch| launch.seq).collect()
    }

    fn banners(&mut self) -> Vec<String> {
        self.scheduler
            .take_failures()
            .into_iter()
            .map(|(_, message)| message)
            .collect()
    }

    fn row(&self, index: usize) -> ControlHookEntry {
        self.scheduler.status().swap_remove(index)
    }

    fn status_commands(&self) -> Vec<String> {
        self.scheduler.status().into_iter().map(|row| row.command).collect()
    }

    fn status_retired(&self) -> Vec<Option<bool>> {
        self.scheduler.status().into_iter().map(|row| row.retired).collect()
    }
}

fn event(kind: EventKind, seq: u64) -> ControlEvent {
    ControlEvent {
        seq,
        #[expect(clippy::cast_precision_loss, reason = "test sequences are small")]
        ts: seq as f64,
        kind,
        window: None,
        workspace: None,
        session: Some(format!("s{seq}")),
        payload: EventPayload::default(),
    }
}

fn capacity() -> u64 {
    u64::try_from(PENDING_CAPACITY).unwrap()
}

#[test]
fn events_fan_out_to_every_matching_hook_with_independent_slots() {
    let mut h = Harness::new();
    h.apply("on status a\non status b\non notify c");

    h.dispatch(EventKind::Status, 1);
    h.dispatch(EventKind::TreeChanged, 2);

    assert_eq!(h.launched_commands(), ["a", "b"]);
    assert_eq!(h.launched_seqs(), [1, 1]);
    h.exit(0, 0);
    h.dispatch(EventKind::Status, 3);
    assert_eq!(h.launched_commands(), ["a", "b", "a"]);
    let pending: Vec<i64> = h.scheduler.status().iter().map(|row| row.pending).collect();
    assert_eq!(pending, [0, 1, 0]);
}

#[test]
fn a_burst_is_delivered_in_order_one_child_at_a_time() {
    let mut h = Harness::new();
    h.apply("on status a");

    for seq in 1..=4 {
        h.dispatch(EventKind::Status, seq);
    }
    assert_eq!(h.launched_seqs(), [1]);
    assert_eq!(h.row(0).pending, 3);
    assert!(h.row(0).running_pid.is_some());

    h.exit(0, 0);
    assert_eq!(h.launched_seqs(), [1, 2]);
    h.exit(1, 0);
    h.exit(2, 0);
    assert_eq!(h.launched_seqs(), [1, 2, 3, 4]);
    assert_eq!(h.row(0).pending, 0);
    h.exit(3, 0);
    assert_eq!(h.row(0).running_pid, None);
    assert_eq!(h.row(0).elapsed_seconds, None);
}

#[test]
fn pending_is_capped_dropping_the_oldest_and_counting() {
    let mut h = Harness::new();
    h.apply("on status a");

    for seq in 1..=capacity() + 3 {
        h.dispatch(EventKind::Status, seq);
    }
    assert_eq!(h.row(0).pending, i64::try_from(PENDING_CAPACITY).unwrap());
    assert_eq!(h.row(0).dropped, 2);

    // 1 ran, 2 and 3 were dropped: the queue resumes at 4
    h.exit(0, 0);
    assert_eq!(h.launched_seqs(), [1, 4]);
}

#[test]
fn a_launch_failure_advances_the_queue_and_records_one_failure() {
    let mut h = Harness::new();
    h.apply("on status a");

    h.dispatch(EventKind::Status, 1);
    h.launcher.fail_next = Some("no such file".to_owned());
    h.dispatch(EventKind::Status, 2);
    h.dispatch(EventKind::Status, 3);
    h.exit(0, 0);

    assert_eq!(h.launched_seqs(), [1, 3]);
    assert_eq!(h.row(0).last_failure.as_deref(), Some("spawn failed: no such file"));
    assert_eq!(h.banners(), ["spawn failed: no such file"]);
    h.exit(1, 0);
    assert_eq!(h.row(0).last_failure, None);
}

#[test]
fn a_launch_failure_on_an_idle_hook_leaves_it_idle() {
    let mut h = Harness::new();
    h.apply("on status a");
    h.launcher.fail_next = Some("boom".to_owned());

    h.dispatch(EventKind::Status, 1);

    assert!(h.launcher.launches.is_empty());
    assert_eq!(h.row(0).running_pid, None);
    assert_eq!(h.row(0).last_failure.as_deref(), Some("spawn failed: boom"));
    h.dispatch(EventKind::Status, 2);
    assert_eq!(h.launched_seqs(), [2]);
}

#[test]
fn a_delivery_failure_on_a_live_run_never_releases_the_slot() {
    let mut h = Harness::new();
    h.apply("on status a");

    h.dispatch(EventKind::Status, 1);
    h.dispatch(EventKind::Status, 2);
    h.scheduler.delivery_failed(&h.token(0), "EBADF");
    assert_eq!(h.launcher.launches.len(), 1);
    assert!(h.row(0).running_pid.is_some());
    assert_eq!(h.row(0).pending, 1);
    assert_eq!(h.row(0).last_failure.as_deref(), Some("delivery failed: EBADF"));
    assert_eq!(h.banners(), ["delivery failed: EBADF"]);

    h.apply("on status a");
    assert_eq!(h.row(0).last_failure.as_deref(), Some("delivery failed: EBADF"));

    // a clean exit after a delivery failure keeps the failure
    h.exit(0, 0);
    assert_eq!(h.row(0).last_failure.as_deref(), Some("delivery failed: EBADF"));
    assert_eq!(h.launched_seqs(), [1, 2]);
    h.exit(1, 0);
    assert_eq!(h.row(0).last_failure, None);
}

#[test]
fn a_non_zero_exit_banners_once_until_success_or_reload() {
    let mut h = Harness::new();
    h.apply("on status a");

    h.dispatch(EventKind::Status, 1);
    h.exit(0, 2);
    h.dispatch(EventKind::Status, 2);
    h.exit(1, 3);
    assert_eq!(h.banners(), ["exit 2"]);
    assert_eq!(h.row(0).last_failure.as_deref(), Some("exit 3"));

    h.apply("on status a");
    h.dispatch(EventKind::Status, 3);
    h.exit(2, 4);
    assert_eq!(h.banners(), ["exit 4"]);

    h.dispatch(EventKind::Status, 4);
    h.exit(3, 0);
    assert_eq!(h.row(0).last_failure, None);
    h.dispatch(EventKind::Status, 5);
    h.exit(4, 5);
    assert_eq!(h.banners(), ["exit 5"]);
}

#[test]
fn a_banner_carries_the_hooks_entry() {
    let mut h = Harness::new();
    h.apply("# comment\non status a");
    h.dispatch(EventKind::Status, 1);
    h.exit(0, 1);

    let failures = h.scheduler.take_failures();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0.command(), "a");
    assert_eq!(failures[0].0.line, 2);
    assert!(h.scheduler.take_failures().is_empty());
}

#[test]
fn a_reload_after_reorder_and_comments_keeps_state_and_updates_lines() {
    let mut h = Harness::new();
    h.apply("on status a\non status b");
    for seq in 1..=3 {
        h.dispatch(EventKind::Status, seq);
    }
    h.exit(1, 7);
    let pid_a = h.row(0).running_pid;

    h.apply("# moved\n\non status b\n# a comment above a\non status a");

    assert_eq!(h.status_commands(), ["b", "a"]);
    let rows = h.scheduler.status();
    assert_eq!(rows.iter().map(|row| row.line).collect::<Vec<_>>(), [3, 5]);
    assert_eq!(rows[1].running_pid, pid_a);
    assert_eq!(rows[1].pending, 2);
    assert_eq!(rows[0].last_failure.as_deref(), Some("exit 7"));
    assert_eq!(rows[0].pending, 1);
    assert_eq!(h.launched_seqs(), [1, 1, 2]);

    h.dispatch(EventKind::Status, 4);
    assert_eq!(h.launcher.launches.len(), 3);
    h.exit(2, 8);
    assert_eq!(h.banners(), ["exit 7", "exit 8"]);
    assert_eq!(h.launched_seqs(), [1, 1, 2, 3]);
}

#[test]
fn a_reload_keeps_a_nonzero_dropped_count() {
    let mut h = Harness::new();
    h.apply("on status a");
    for seq in 1..=capacity() + 3 {
        h.dispatch(EventKind::Status, seq);
    }

    h.apply("# comment\non status a");

    assert_eq!(h.row(0).dropped, 2);
    assert_eq!(h.row(0).pending, i64::try_from(PENDING_CAPACITY).unwrap());
    assert_eq!(h.row(0).line, 2);
}

#[test]
fn a_removed_hook_drops_pending_and_stays_occupied_until_exit() {
    let mut h = Harness::new();
    h.apply("on status a\non status b");
    for seq in 1..=3 {
        h.dispatch(EventKind::Status, seq);
    }

    h.apply("on status b");
    h.dispatch(EventKind::Status, 4);
    assert_eq!(h.status_commands(), ["b", "a"]);
    assert_eq!(h.status_retired(), [None, Some(true)]);
    assert_eq!(h.row(1).pending, 0);
    assert_eq!(h.row(1).running_pid, Some(101));
    let runs_of_a = |h: &Harness| h.launched_commands().iter().filter(|command| **command == "a").count();
    assert_eq!(runs_of_a(&h), 1);

    h.exit(0, 0);
    assert_eq!(runs_of_a(&h), 1);
    assert_eq!(h.status_commands(), ["b"]);
}

#[test]
fn an_idle_removed_hook_goes_at_once() {
    let mut h = Harness::new();
    h.apply("on status a\non status b");
    h.apply("on status b");
    assert_eq!(h.status_commands(), ["b"]);
}

#[test]
fn a_changed_command_is_a_new_identity_running_alongside_the_retired_one() {
    let mut h = Harness::new();
    h.apply("on status a");
    h.dispatch(EventKind::Status, 1);

    h.apply("on status a2");
    h.dispatch(EventKind::Status, 2);

    assert_eq!(h.launched_commands(), ["a", "a2"]);
    assert_eq!(h.status_commands(), ["a2", "a"]);
    assert_eq!(h.status_retired(), [None, Some(true)]);
    h.exit(0, 0);
    assert_eq!(h.launcher.launches.len(), 2);
    assert_eq!(h.status_commands(), ["a2"]);
}

#[test]
fn retired_rows_list_after_live_ones_by_kind_then_command() {
    let mut h = Harness::new();
    h.apply("on status z\non notify y\non status b");
    h.dispatch(EventKind::Status, 1);
    h.dispatch(EventKind::Notify, 2);
    h.apply("on tree.changed live");

    assert_eq!(h.status_commands(), ["live", "y", "b", "z"]);
    assert_eq!(h.status_retired(), [None, Some(true), Some(true), Some(true)]);
}

#[test]
fn a_re_add_before_exit_reattaches_and_the_original_exit_starts_the_new_queue() {
    let mut h = Harness::new();
    h.apply("on status a");
    h.dispatch(EventKind::Status, 1);
    h.dispatch(EventKind::Status, 2);
    let first = h.token(0);

    h.apply("");
    assert_eq!(h.status_retired(), [Some(true)]);
    h.apply("on status a");
    h.dispatch(EventKind::Status, 3);
    assert_eq!(h.launcher.launches.len(), 1);
    assert_eq!(h.scheduler.status().len(), 1);
    assert_eq!(h.row(0).retired, None);
    assert_eq!(h.row(0).pending, 1);
    assert_eq!(h.row(0).running_pid, Some(101));

    h.scheduler.exited(&first, 0, &mut h.launcher);
    assert_eq!(h.launched_seqs(), [1, 3]);

    // the same exit reported again is stale: the slot belongs to the newer run
    h.scheduler.exited(&first, 0, &mut h.launcher);
    assert_eq!(h.launcher.launches.len(), 2);
    assert!(h.row(0).running_pid.is_some());
    h.exit(1, 0);
    assert_eq!(h.row(0).running_pid, None);
}

#[test]
fn stale_tokens_are_ignored() {
    let mut h = Harness::new();
    h.apply("on status a");
    h.dispatch(EventKind::Status, 1);
    h.exit(0, 0);
    h.dispatch(EventKind::Status, 2);
    let old = h.token(0);

    h.scheduler.delivery_failed(&old, "EPIPE");
    h.scheduler.exited(&old, 1, &mut h.launcher);
    assert_eq!(h.row(0).running_pid, Some(102));
    assert_eq!(h.row(0).last_failure, None);
    assert!(h.banners().is_empty());

    // a run of a hook that is gone entirely
    h.apply("");
    h.exit(1, 0);
    assert!(h.scheduler.status().is_empty());
    h.scheduler.delivery_failed(&h.token(1), "late");
    h.exit(1, 3);
    assert!(h.banners().is_empty());
}

#[test]
fn status_reports_live_state_in_file_order() {
    let mut h = Harness::new();
    h.apply("on notify n\non status a");
    h.dispatch(EventKind::Status, 1);
    h.dispatch(EventKind::Status, 2);
    h.elapsed.set(Duration::from_millis(2_500));

    let rows = h.scheduler.status();
    assert_eq!(
        rows.iter().map(|row| row.kind.as_str()).collect::<Vec<_>>(),
        ["notify", "status"]
    );
    assert_eq!(
        rows[0],
        ControlHookEntry {
            kind: "notify".to_owned(),
            command: "n".to_owned(),
            line: 1,
            running_pid: None,
            elapsed_seconds: None,
            pending: 0,
            dropped: 0,
            last_failure: None,
            retired: None,
        }
    );
    assert_eq!(rows[1].running_pid, Some(101));
    assert_eq!(rows[1].elapsed_seconds, Some(2.5));
    assert_eq!(rows[1].pending, 1);
    assert_eq!(rows[1].dropped, 0);
    assert_eq!(rows[1].last_failure, None);
}

#[test]
fn a_launch_hands_over_the_entry_and_the_event() {
    struct Capture(Vec<(HookEntry, ControlEvent)>);
    impl HookLauncher for Capture {
        fn launch(&mut self, run: HookRun<'_>) -> Result<i32, String> {
            self.0.push((run.entry.clone(), run.event.clone()));
            Ok(1)
        }
    }
    let mut scheduler = HookScheduler::default();
    scheduler.apply(&parse_hooks_conf("\non session.created echo hi").0);
    let mut capture = Capture(Vec::new());
    scheduler.dispatch(&event(EventKind::SessionCreated, 9), &mut capture);

    assert_eq!(capture.0.len(), 1);
    assert_eq!(capture.0[0].0.command(), "echo hi");
    assert_eq!(capture.0[0].0.line, 2);
    assert_eq!(capture.0[0].1, event(EventKind::SessionCreated, 9));
}
